//! Graph renderer selection: `Auto | Vector (Kitty/Sixel) | Character Grid`,
//! terminal graphics capability probing, and the per-frame dispatch between
//! [`CellGridGraphBackend`] and the optional `PixelVectorGraphBackend`
//! (`graphics-protocol` Cargo feature).
//!
//! Frame protocol (driven by `views::render_app` + the main loop):
//! 1. [`GraphRenderer::begin_frame`] before anything is drawn;
//! 2. [`GraphRenderBackend::render_viewport`] from the Log table;
//! 3. [`GraphRenderer::finish_frame`] after every overlay is drawn, so the
//!    vector backend can see what now covers the graph area;
//! 4. [`GraphRenderer::write_emission`] after ratatui flushed the frame,
//!    writing any pending image escape sequences to the terminal.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::app::RenderContext;
use crate::graph::probe::{WindowSizePx, can_probe_wezterm_env, query_tmux_passthrough};
use crate::graph::{CellGridGraphBackend, GraphRenderBackend, GraphViewportSlice};

#[cfg(feature = "graphics-protocol")]
use crate::graph::vector::PixelVectorGraphBackend;

/// Maximum time spent probing `tmux` passthrough or protocol handshake before
/// silently degrading to the character grid.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(50);

/// User-selected graph renderer (`[⚙ Presentation] → Graph Renderer`, `F8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraphRendererMode {
    /// Vector tracks when a graphics protocol and the cell pixel size are known.
    #[default]
    Auto,
    /// Prefer vector tracks when a graphics protocol is detected (Kitty / iTerm2
    /// can stretch using [`CellPixelSize::FALLBACK`] when pixel size is unknown).
    Vector,
    /// Always use the universal rounded box-drawing grid.
    CharacterGrid,
}

impl GraphRendererMode {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Vector, Self::CharacterGrid];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Vector => "Vector (Kitty/Sixel)",
            Self::CharacterGrid => "Character Grid",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::Vector,
            Self::Vector => Self::CharacterGrid,
            Self::CharacterGrid => Self::Auto,
        }
    }
}

/// Terminal image protocol used by the vector backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphicsProtocol {
    /// Kitty Graphics Protocol with Unicode placeholders (`U=1`, `U+10EEEE`).
    Kitty,
    /// DEC Sixel bitmap drawn over the text layer.
    Sixel,
    /// iTerm2 / WezTerm `OSC 1337` inline image drawn over the text layer.
    Iterm2,
}

impl GraphicsProtocol {
    #[cfg_attr(any(not(test), not(feature = "graphics-protocol")), allow(dead_code))]
    pub fn label(self) -> &'static str {
        match self {
            Self::Kitty => "Kitty",
            Self::Sixel => "Sixel",
            Self::Iterm2 => "iTerm2",
        }
    }
}

/// Pixel size of one terminal cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellPixelSize {
    pub width: u16,
    pub height: u16,
}

impl CellPixelSize {
    /// Cell size assumed for Kitty / iTerm2 when the terminal does not report
    /// one: both protocols stretch the image over its `cols x rows` cells.
    /// Odd dimensions place track axes and row centers on exact half-texel
    /// midpoints (`10.5 / 21 = 0.5`, `22.5 / 45 = 0.5`) at HiDPI density.
    pub const FALLBACK: Self = Self {
        width: 21,
        height: 45,
    };

    pub fn is_usable(self) -> bool {
        (2..=256).contains(&self.width) && (4..=512).contains(&self.height)
    }

    /// Derives the cell size from `window_size()` (`columns, rows, width_px, height_px`);
    /// `None` when the terminal reports 0 pixels (common on Windows / some PTYs).
    pub fn from_window(columns: u16, rows: u16, width_px: u16, height_px: u16) -> Option<Self> {
        if columns == 0 || rows == 0 || width_px == 0 || height_px == 0 {
            return None;
        }
        let size = Self {
            width: width_px / columns,
            height: height_px / rows,
        };
        size.is_usable().then_some(size)
    }
}

/// What the running terminal can do, probed at startup and on every `Resize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GraphicsCapabilities {
    pub protocol: Option<GraphicsProtocol>,
    pub cell_px: Option<CellPixelSize>,
}

impl GraphicsCapabilities {
    /// Probes the real environment, `tmux` passthrough state (if inside `tmux`),
    /// and `crossterm::terminal::window_size()`.
    pub fn detect() -> Self {
        let start = Instant::now();
        let window = crossterm::terminal::window_size()
            .ok()
            .map(|w| WindowSizePx {
                columns: w.columns,
                rows: w.rows,
                width_px: w.width,
                height_px: w.height,
            });
        let tmux_passthrough = if std::env::var_os("TMUX").is_some() {
            Some(query_tmux_passthrough(HANDSHAKE_TIMEOUT))
        } else {
            None
        };
        Self::from_probe_with(
            |k| std::env::var(k).ok(),
            window,
            tmux_passthrough,
            |_protocol, timeout| {
                let elapsed = start.elapsed();
                (elapsed <= timeout).then_some(elapsed)
            },
        )
    }

    /// Pure capability probe over an environment lookup and an optional
    /// [`WindowSizePx`].
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn from_probe(env: impl Fn(&str) -> Option<String>, window: Option<WindowSizePx>) -> Self {
        Self::from_probe_with(env, window, None, |_, _| Some(Duration::ZERO))
    }

    /// Pure capability probe with explicit `tmux` passthrough state and a
    /// bounded protocol handshake callback (exceeding `timeout` or returning
    /// `None` degrades `protocol` to `None`).
    pub fn from_probe_with(
        env: impl Fn(&str) -> Option<String>,
        window: Option<WindowSizePx>,
        tmux_passthrough: Option<bool>,
        handshake: impl FnOnce(GraphicsProtocol, Duration) -> Option<Duration>,
    ) -> Self {
        let cell_px = window.and_then(WindowSizePx::cell_pixel_size);
        let mut protocol = detect_protocol_with(&env, tmux_passthrough);
        if let Some(p) = protocol {
            match handshake(p, HANDSHAKE_TIMEOUT) {
                Some(elapsed) if elapsed <= HANDSHAKE_TIMEOUT => {}
                _ => protocol = None,
            }
        }
        Self { protocol, cell_px }
    }

    /// Returns `true` when WezTerm is active without OS-reported pixel sizes,
    /// so pixel calibration should run asynchronously via `wezterm cli`.
    pub fn needs_async_wezterm_probe(&self) -> bool {
        self.protocol == Some(GraphicsProtocol::Iterm2)
            && self.cell_px.is_none()
            && can_probe_wezterm_env()
    }
}

/// Protocol detection from environment variables plus `tmux` passthrough state
/// (avoids writing raw terminal queries while `EventStream` owns stdin).
///
/// * Inside `tmux` without `allow-passthrough = on|all` (or inside GNU `screen`),
///   image escapes are swallowed, so detection reports `None`.
/// * Over SSH, `TERM=xterm-kitty` travels and works directly.
#[cfg_attr(not(test), allow(dead_code))]
pub fn detect_protocol(env: &impl Fn(&str) -> Option<String>) -> Option<GraphicsProtocol> {
    detect_protocol_with(env, None)
}

pub fn detect_protocol_with(
    env: &impl Fn(&str) -> Option<String>,
    tmux_passthrough: Option<bool>,
) -> Option<GraphicsProtocol> {
    let term = env("TERM").unwrap_or_default();
    let in_tmux = env("TMUX").is_some() || term.starts_with("tmux");
    if in_tmux && tmux_passthrough != Some(true) {
        return None;
    }
    if term.starts_with("screen") && !(in_tmux && tmux_passthrough == Some(true)) {
        return None;
    }
    let term_program = env("TERM_PROGRAM").unwrap_or_default();
    if term == "xterm-kitty"
        || env("KITTY_WINDOW_ID").is_some()
        || term == "xterm-ghostty"
        || term_program.eq_ignore_ascii_case("ghostty")
    {
        return Some(GraphicsProtocol::Kitty);
    }
    if term_program == "WezTerm"
        || term_program == "iTerm.app"
        || env("LC_TERMINAL").as_deref() == Some("iTerm2")
    {
        return Some(GraphicsProtocol::Iterm2);
    }
    if env("WT_SESSION").is_some() || term.starts_with("foot") || term.contains("sixel") {
        return Some(GraphicsProtocol::Sixel);
    }
    None
}

/// The backend a frame will actually use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedGraphBackend {
    CharacterGrid,
    Vector {
        protocol: GraphicsProtocol,
        cell_px: CellPixelSize,
    },
}

/// Maps the user's mode + probed capabilities to a concrete backend; anything
/// unknown silently degrades to the character grid so raw escape sequences are
/// never emitted to an unsupported terminal.
pub fn resolve_backend(
    mode: GraphRendererMode,
    caps: GraphicsCapabilities,
) -> ResolvedGraphBackend {
    if !cfg!(feature = "graphics-protocol") {
        return ResolvedGraphBackend::CharacterGrid;
    }
    let cell_px = caps.cell_px.filter(|c| c.is_usable());
    match mode {
        GraphRendererMode::CharacterGrid => ResolvedGraphBackend::CharacterGrid,
        GraphRendererMode::Auto => match (caps.protocol, cell_px) {
            (Some(protocol), Some(cell_px)) => ResolvedGraphBackend::Vector { protocol, cell_px },
            _ => ResolvedGraphBackend::CharacterGrid,
        },
        GraphRendererMode::Vector => {
            let Some(protocol) = caps.protocol else {
                return ResolvedGraphBackend::CharacterGrid;
            };
            // Sixel paints raw pixels, so it needs the real cell size; Kitty and
            // iTerm2 stretch the image over the cell grid themselves.
            let cell_px = cell_px
                .or((protocol != GraphicsProtocol::Sixel).then_some(CellPixelSize::FALLBACK));
            match cell_px {
                Some(cell_px) => ResolvedGraphBackend::Vector { protocol, cell_px },
                None => ResolvedGraphBackend::CharacterGrid,
            }
        }
    }
}

/// Owns both graph backends and the bytes the vector backend wants written
/// to the terminal after the current frame.
pub struct GraphRenderer {
    mode: GraphRendererMode,
    caps: GraphicsCapabilities,
    grid: CellGridGraphBackend,
    #[cfg(feature = "graphics-protocol")]
    vector: Option<PixelVectorGraphBackend>,
    emission: Vec<u8>,
}

impl Default for GraphRenderer {
    fn default() -> Self {
        Self::new(
            GraphRendererMode::default(),
            GraphicsCapabilities::default(),
        )
    }
}

impl std::fmt::Debug for GraphRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphRenderer")
            .field("mode", &self.mode)
            .field("caps", &self.caps)
            .field("resolved", &self.resolved())
            .finish_non_exhaustive()
    }
}

impl GraphRenderer {
    pub fn new(mode: GraphRendererMode, caps: GraphicsCapabilities) -> Self {
        Self {
            mode,
            caps,
            grid: CellGridGraphBackend::new(),
            #[cfg(feature = "graphics-protocol")]
            vector: None,
            emission: Vec::new(),
        }
    }

    pub fn mode(&self) -> GraphRendererMode {
        self.mode
    }

    pub fn capabilities(&self) -> GraphicsCapabilities {
        self.caps
    }

    pub fn set_mode(&mut self, mode: GraphRendererMode) {
        self.mode = mode;
        self.deactivate_vector_if_grid();
    }

    /// Re-calibrates after startup / `Event::Resize` (font zoom changes the cell
    /// pixel size): every cached strip, image and placement is invalidated so the
    /// next frame rebuilds at the new `(cell_px_w, cell_px_h)`. Unchanged caps
    /// are a no-op: a plain terminal resize keeps every cache, since viewport
    /// geometry is already part of the raster / emission keys.
    pub fn set_capabilities(&mut self, caps: GraphicsCapabilities) {
        if caps == self.caps {
            return;
        }
        self.caps = caps;
        self.invalidate();
        self.deactivate_vector_if_grid();
    }

    fn deactivate_vector_if_grid(&mut self) {
        #[cfg(feature = "graphics-protocol")]
        if self.resolved() == ResolvedGraphBackend::CharacterGrid
            && let Some(v) = self.vector.as_mut()
        {
            v.deactivate();
        }
    }

    pub fn resolved(&self) -> ResolvedGraphBackend {
        resolve_backend(self.mode, self.caps)
    }

    /// Human-readable status line (`Graph Renderer: Auto → Vector · Kitty`).
    #[cfg_attr(any(not(test), not(feature = "graphics-protocol")), allow(dead_code))]
    pub fn status_label(&self) -> String {
        let resolved = match self.resolved() {
            ResolvedGraphBackend::CharacterGrid => "Character Grid".to_string(),
            ResolvedGraphBackend::Vector { protocol, .. } => {
                format!("Vector · {}", protocol.label())
            }
        };
        let mut s = format!("Graph Renderer: {} → {resolved}", self.mode.label());
        if self.mode != GraphRendererMode::CharacterGrid
            && self.resolved() == ResolvedGraphBackend::CharacterGrid
        {
            s.push_str(if !cfg!(feature = "graphics-protocol") {
                " (built without graphics-protocol)"
            } else if self.caps.protocol.is_none() {
                " (no graphics protocol detected)"
            } else {
                " (cell pixel size unknown)"
            });
        }
        s
    }

    /// Forces the next vector frame to re-rasterize and re-transmit (after the
    /// terminal was cleared or re-entered, e.g. around an external editor).
    pub fn invalidate(&mut self) {
        #[cfg(feature = "graphics-protocol")]
        if let Some(v) = self.vector.as_mut() {
            v.invalidate();
        }
    }

    pub fn begin_frame(&mut self) {
        #[cfg(feature = "graphics-protocol")]
        if let Some(v) = self.vector.as_mut() {
            v.begin_frame();
        }
    }

    /// Post-overlay pass: occlusion handling and image encoding (see module docs).
    pub fn finish_frame(&mut self, buf: &mut Buffer, terminal_area: Rect, ctx: &RenderContext<'_>) {
        #[cfg(feature = "graphics-protocol")]
        if let Some(v) = self.vector.as_mut() {
            v.finish_frame(buf, terminal_area, ctx, &mut self.grid, &mut self.emission);
        }
        #[cfg(not(feature = "graphics-protocol"))]
        let _ = (buf, terminal_area, ctx);
    }

    /// Writes (and clears) the queued image escape sequences.
    pub fn write_emission(&mut self, out: &mut impl Write) -> io::Result<()> {
        if self.emission.is_empty() {
            return Ok(());
        }
        let res = out.write_all(&self.emission).and_then(|()| out.flush());
        self.emission.clear();
        res
    }

    /// Escapes that release terminal-side image memory before exit.
    pub fn shutdown_sequence(&mut self) -> Vec<u8> {
        #[cfg(feature = "graphics-protocol")]
        if let Some(v) = self.vector.as_mut() {
            return v.shutdown_sequence();
        }
        Vec::new()
    }
}

impl GraphRenderBackend for GraphRenderer {
    fn render_viewport(
        &mut self,
        slice: &GraphViewportSlice<'_>,
        buf: &mut Buffer,
        ctx: &mut RenderContext<'_>,
    ) {
        #[cfg(feature = "graphics-protocol")]
        if let ResolvedGraphBackend::Vector { protocol, cell_px } = self.resolved() {
            let vector = self
                .vector
                .get_or_insert_with(|| PixelVectorGraphBackend::new(protocol, cell_px));
            vector.configure(protocol, cell_px);
            if vector.try_render(slice, buf, ctx) {
                return;
            }
            vector.deactivate();
        }
        #[cfg(feature = "graphics-protocol")]
        if self.resolved() == ResolvedGraphBackend::CharacterGrid
            && let Some(v) = self.vector.as_mut()
        {
            v.deactivate();
        }
        self.grid.render_viewport(slice, buf, ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn detects_protocols_from_environment_and_tmux_passthrough() {
        let p = |pairs: &[(&str, &str)]| detect_protocol(&env_of(pairs));
        assert_eq!(p(&[("TERM", "xterm-kitty")]), Some(GraphicsProtocol::Kitty));
        assert_eq!(
            p(&[("KITTY_WINDOW_ID", "1")]),
            Some(GraphicsProtocol::Kitty)
        );
        assert_eq!(
            p(&[("TERM_PROGRAM", "ghostty")]),
            Some(GraphicsProtocol::Kitty)
        );
        assert_eq!(
            p(&[("TERM_PROGRAM", "WezTerm")]),
            Some(GraphicsProtocol::Iterm2)
        );
        assert_eq!(
            p(&[("TERM_PROGRAM", "iTerm.app")]),
            Some(GraphicsProtocol::Iterm2)
        );
        assert_eq!(p(&[("WT_SESSION", "abc")]), Some(GraphicsProtocol::Sixel));
        assert_eq!(p(&[("TERM", "xterm-256color")]), None);
        // tmux / screen without passthrough → silent grid fallback.
        assert_eq!(p(&[("TERM", "xterm-kitty"), ("TMUX", "/tmp/x")]), None);
        assert_eq!(p(&[("TERM", "screen-256color"), ("WT_SESSION", "x")]), None);
        // tmux with allow-passthrough enabled → preserves underlying terminal protocol.
        assert_eq!(
            detect_protocol_with(
                &env_of(&[
                    ("TERM", "tmux-256color"),
                    ("TMUX", "/tmp/x"),
                    ("TERM_PROGRAM", "WezTerm"),
                ]),
                Some(true),
            ),
            Some(GraphicsProtocol::Iterm2)
        );
        assert_eq!(
            detect_protocol_with(
                &env_of(&[
                    ("TERM", "tmux-256color"),
                    ("TMUX", "/tmp/x"),
                    ("TERM_PROGRAM", "WezTerm"),
                ]),
                Some(false),
            ),
            None
        );
    }

    #[test]
    fn handshake_timeout_over_50ms_degrades_to_none() {
        let win = Some(WindowSizePx {
            columns: 100,
            rows: 40,
            width_px: 900,
            height_px: 720,
        });
        let timed_out = GraphicsCapabilities::from_probe_with(
            env_of(&[("TERM", "xterm-kitty")]),
            win,
            None,
            |_, timeout| Some(timeout + Duration::from_millis(1)),
        );
        assert_eq!(timed_out.protocol, None);
        assert_eq!(
            resolve_backend(GraphRendererMode::Auto, timed_out),
            ResolvedGraphBackend::CharacterGrid
        );

        let fast = GraphicsCapabilities::from_probe_with(
            env_of(&[("TERM", "xterm-kitty")]),
            win,
            None,
            |_, _| Some(Duration::from_millis(10)),
        );
        assert_eq!(fast.protocol, Some(GraphicsProtocol::Kitty));
    }

    #[test]
    fn cell_pixel_size_from_window() {
        let caps = GraphicsCapabilities::from_probe(
            env_of(&[]),
            Some(WindowSizePx {
                columns: 100,
                rows: 40,
                width_px: 900,
                height_px: 720,
            }),
        );
        assert_eq!(
            caps.cell_px,
            Some(CellPixelSize {
                width: 9,
                height: 18
            })
        );
        // window_size() reporting 0 pixels → unknown.
        let caps = GraphicsCapabilities::from_probe(
            env_of(&[]),
            Some(WindowSizePx {
                columns: 100,
                rows: 40,
                width_px: 0,
                height_px: 0,
            }),
        );
        assert_eq!(caps.cell_px, None);
        assert_eq!(CellPixelSize::from_window(100, 40, 100, 100), None);
    }

    #[test]
    fn resolution_degrades_to_grid_when_capabilities_are_missing() {
        let cell = CellPixelSize {
            width: 9,
            height: 18,
        };
        let full = GraphicsCapabilities {
            protocol: Some(GraphicsProtocol::Kitty),
            cell_px: Some(cell),
        };
        let no_px = GraphicsCapabilities {
            cell_px: None,
            ..full
        };
        let sixel_no_px = GraphicsCapabilities {
            protocol: Some(GraphicsProtocol::Sixel),
            cell_px: None,
        };
        let none = GraphicsCapabilities::default();
        use GraphRendererMode::*;
        assert_eq!(
            resolve_backend(CharacterGrid, full),
            ResolvedGraphBackend::CharacterGrid
        );
        assert_eq!(
            resolve_backend(Auto, none),
            ResolvedGraphBackend::CharacterGrid
        );
        assert_eq!(
            resolve_backend(Auto, no_px),
            ResolvedGraphBackend::CharacterGrid
        );
        // Vector mode with no detected protocol never emits raw escapes.
        assert_eq!(
            resolve_backend(Vector, none),
            ResolvedGraphBackend::CharacterGrid
        );
        if cfg!(feature = "graphics-protocol") {
            assert_eq!(
                resolve_backend(Auto, full),
                ResolvedGraphBackend::Vector {
                    protocol: GraphicsProtocol::Kitty,
                    cell_px: cell
                }
            );
            // Forced vector: Kitty stretches the image, so an unknown size is fine…
            assert_eq!(
                resolve_backend(Vector, no_px),
                ResolvedGraphBackend::Vector {
                    protocol: GraphicsProtocol::Kitty,
                    cell_px: CellPixelSize::FALLBACK
                }
            );
            // …but Sixel paints raw pixels and needs the real cell size.
            assert_eq!(
                resolve_backend(Vector, sixel_no_px),
                ResolvedGraphBackend::CharacterGrid
            );
        }
    }

    #[test]
    fn mode_cycles_through_all_three() {
        let mut m = GraphRendererMode::Auto;
        for expected in [
            GraphRendererMode::Vector,
            GraphRendererMode::CharacterGrid,
            GraphRendererMode::Auto,
        ] {
            m = m.next();
            assert_eq!(m, expected);
        }
    }
}
