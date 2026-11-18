//! `PixelVectorGraphBackend`: pixel-level anti-aliased DAG tracks rendered with
//! `tiny-skia` and shipped to the terminal as one image per frame.
//!
//! * **Viewport strip** — the whole visible graph column
//!   (`area.width * cell_px_w` × `rows * cell_px_h`) is one RGBA buffer; each row
//!   is painted into its own `cell_px_h`-tall slice (pre-filled with that row's
//!   `row_bg`), which doubles as IntelliJ's per-row clip.
//! * **Dirty cache** — a content hash over everything that affects pixels
//!   (rows, elements, hover/selection, colors, area, cell size) gates
//!   rasterization; a second key that adds occlusion and the terminal size gates
//!   re-transmission.
//! * **Z-order** — Kitty uses Unicode placeholder cells, so later overlays simply
//!   overwrite them. Sixel / iTerm2 bitmaps sit *above* text, so after all
//!   overlays are drawn [`PixelVectorGraphBackend::finish_frame`] checks which
//!   graph cells no longer hold the backend's marker cells; if any do, that frame
//!   falls back to the character grid around the overlay and no image is sent.

pub mod encode;
pub mod painter;

use std::hash::{Hash, Hasher};

use jgit_core::graph::{EdgeDirection, GraphPrintElement, VisibleRowIdx};
use jgit_core::{BranchColorId, Oid};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use rustc_hash::{FxHashMap, FxHasher};
use tiny_skia::PixmapMut;

use crate::app::RenderContext;
use crate::graph::cell_grid::is_occluded;
use crate::graph::renderer::{CellPixelSize, GraphicsProtocol};
use crate::graph::{
    CellGridGraphBackend, GraphRenderBackend, GraphViewportSlice, RenderGraphElement, RowGraphSlice,
};
use crate::hit_test::{GraphHitKind, HitTarget, HitTestRegistry};

use encode::SixelScratch;
use painter::{PaintGeometry, Rgb, RowPainter};

/// Strip texture cap (≈ a 4K-wide graph column at 200 % scale); larger
/// viewports fall back to the character grid instead of allocating unbounded.
pub const MAX_STRIP_PIXELS: u64 = 4096 * 2048;

/// Alternating Kitty image ids (double buffering: the new image is transmitted
/// under the other id, placeholders switch over, then the old one is deleted).
pub const KITTY_IMAGE_IDS: [u32; 2] = [0x4A_47_01, 0x4A_47_02];

/// Foreground of the blank marker cells written under Sixel / iTerm2 images;
/// any graph cell that no longer carries it was drawn over by an overlay.
pub const OVERLAY_MARKER_FG: Color = Color::Rgb(1, 2, 3);

const GRAPH_ELEMENT_HIT_Z: u8 = 2;

#[derive(Debug, Default)]
struct OwnedRow {
    visible_row: VisibleRowIdx,
    commit_oid: Oid,
    row_bg: Color,
    elements: Vec<RenderGraphElement>,
    colors: Vec<Rgb>,
}

#[derive(Debug, Clone, Copy)]
struct PendingFrame {
    area: Rect,
    rows: usize,
    content_key: u64,
    kitty_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KittyImage {
    id: u32,
    content_key: u64,
}

/// Pixel-level vector graph backend (see module docs).
pub struct PixelVectorGraphBackend {
    protocol: GraphicsProtocol,
    cell_px: CellPixelSize,
    geometry: PaintGeometry,
    painter: RowPainter,
    fallback_grid: CellGridGraphBackend,
    strip: Vec<u8>,
    strip_size: (u32, u32),
    rows: Vec<OwnedRow>,
    rows_len: usize,
    color_map: FxHashMap<BranchColorId, Color>,
    raster_key: Option<u64>,
    pending: Option<PendingFrame>,
    /// Key of the Sixel / iTerm2 image currently on screen.
    emitted_key: Option<u64>,
    /// Kitty image currently stored in the terminal.
    kitty_current: Option<KittyImage>,
    /// Escapes owed to the terminal regardless of the next frame (stale images).
    cleanup: Vec<u8>,
    occluded: Vec<Rect>,
    scratch_hits: HitTestRegistry,
    png: Vec<u8>,
    b64: Vec<u8>,
    sixel: SixelScratch,
}

impl PixelVectorGraphBackend {
    pub fn new(protocol: GraphicsProtocol, cell_px: CellPixelSize) -> Self {
        Self {
            protocol,
            cell_px,
            geometry: PaintGeometry::new(cell_px.width, cell_px.height),
            painter: RowPainter::new(),
            fallback_grid: CellGridGraphBackend::new(),
            strip: Vec::new(),
            strip_size: (0, 0),
            rows: Vec::new(),
            rows_len: 0,
            color_map: FxHashMap::default(),
            raster_key: None,
            pending: None,
            emitted_key: None,
            kitty_current: None,
            cleanup: Vec::new(),
            occluded: Vec::new(),
            scratch_hits: HitTestRegistry::new(),
            png: Vec::new(),
            b64: Vec::new(),
            sixel: SixelScratch::default(),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn geometry(&self) -> &PaintGeometry {
        &self.geometry
    }

    /// The last rasterized strip as `(rgba, width_px, height_px)`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn strip(&self) -> (&[u8], u32, u32) {
        (&self.strip, self.strip_size.0, self.strip_size.1)
    }

    /// Switches protocol / cell size; any change drops every cache.
    pub fn configure(&mut self, protocol: GraphicsProtocol, cell_px: CellPixelSize) {
        if protocol == self.protocol && cell_px == self.cell_px {
            return;
        }
        if protocol != self.protocol {
            self.release_kitty_image();
        }
        self.protocol = protocol;
        self.cell_px = cell_px;
        self.geometry = PaintGeometry::new(cell_px.width, cell_px.height);
        self.invalidate();
    }

    pub fn invalidate(&mut self) {
        self.raster_key = None;
        self.emitted_key = None;
        // Re-transmit under the other id; the old image is deleted afterwards.
        if let Some(cur) = self.kitty_current.as_mut() {
            cur.content_key = u64::MAX;
        }
    }

    /// Releases any active terminal-side Kitty image and clears emitted state
    /// when switching to the character grid or falling back.
    pub fn deactivate(&mut self) {
        self.release_kitty_image();
        self.emitted_key = None;
        self.pending = None;
    }

    pub fn begin_frame(&mut self) {
        self.pending = None;
    }

    fn release_kitty_image(&mut self) {
        if let Some(cur) = self.kitty_current.take() {
            encode::kitty_delete(cur.id, &mut self.cleanup);
        }
    }

    pub fn shutdown_sequence(&mut self) -> Vec<u8> {
        self.release_kitty_image();
        std::mem::take(&mut self.cleanup)
    }

    /// Renders the viewport as an image (placeholder / marker cells + hits).
    /// Returns `false` when this frame must use the character grid instead.
    pub fn try_render(
        &mut self,
        slice: &GraphViewportSlice<'_>,
        buf: &mut Buffer,
        ctx: &mut RenderContext<'_>,
    ) -> bool {
        let area = slice.area.intersection(buf.area);
        let rows = (area.height as usize).min(slice.rows.len());
        if area.is_empty() || rows == 0 {
            return true;
        }
        let clipped_area = Rect::new(area.x, area.y, area.width, rows as u16);
        if self.protocol != GraphicsProtocol::Kitty
            && slice
                .occluding_rects
                .iter()
                .any(|r| r.intersects(clipped_area))
        {
            self.emitted_key = None;
            return false;
        }
        let cw = u64::from(self.cell_px.width);
        let ch = u64::from(self.cell_px.height);
        if u64::from(area.width) * cw * rows as u64 * ch > MAX_STRIP_PIXELS {
            return false;
        }
        if self.protocol == GraphicsProtocol::Kitty
            && (area.width > encode::KITTY_MAX_CELLS || rows > encode::KITTY_MAX_CELLS as usize)
        {
            return false;
        }

        let content_key = self.capture(slice, rows, clipped_area, ctx);
        if self.raster_key != Some(content_key) {
            self.rasterize(
                clipped_area.width,
                rows,
                Rgb::from_color(ctx.theme.fg_bright, Rgb(255, 255, 255)),
            );
            self.raster_key = Some(content_key);
        }
        self.register_hits(clipped_area, rows, ctx);

        let kitty_id = match self.kitty_current {
            Some(cur) if cur.content_key == content_key => cur.id,
            Some(cur) => other_kitty_id(cur.id),
            None => KITTY_IMAGE_IDS[0],
        };
        self.write_cells(buf, clipped_area, rows, slice.occluding_rects, kitty_id);
        self.pending = Some(PendingFrame {
            area: clipped_area,
            rows,
            content_key,
            kitty_id,
        });
        true
    }

    /// Sub-pixel hit testing using `isOverNode` / `isOverEdge` directly on
    /// pixel coordinates relative to `area` (for SGR-Pixels `1016` coordinates).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn hit_test_pixel(&self, area: Rect, px_x: u16, px_y: u16) -> Option<HitTarget> {
        let origin_x = u32::from(area.x) * u32::from(self.cell_px.width);
        let origin_y = u32::from(area.y) * u32::from(self.cell_px.height);
        let rel_x = u32::from(px_x).checked_sub(origin_x)?;
        let rel_y = u32::from(px_y).checked_sub(origin_y)?;
        let row_h = u32::from(self.cell_px.height.max(1));
        let row_idx = (rel_y / row_h) as usize;
        if row_idx >= self.rows_len {
            return None;
        }
        let row = &self.rows[row_idx];
        let local_x = rel_x as f32;
        let local_y = (rel_y % row_h) as f32;
        let g = &self.geometry;
        let visible_row = row.visible_row as usize;

        for (idx, item) in row.elements.iter().enumerate() {
            if let GraphPrintElement::Node { pos, .. } = item.element
                && g.is_over_node(pos, local_x, local_y)
            {
                return Some(HitTarget::GraphPrintElement {
                    visible_row,
                    element_idx: idx,
                    commit_oid: row.commit_oid,
                    kind: GraphHitKind::Node {
                        commit_oid: row.commit_oid,
                    },
                });
            }
        }
        for (idx, item) in row.elements.iter().enumerate() {
            if let (
                GraphPrintElement::TerminalEdge {
                    pos,
                    direction,
                    up_row,
                    down_row,
                    target_row,
                    ..
                },
                Some(target_oid),
            ) = (&item.element, item.target_oid)
            {
                let down = *direction == EdgeDirection::Down;
                if g.is_over_edge(*pos, *pos, down, local_x, local_y) {
                    return Some(HitTarget::GraphPrintElement {
                        visible_row,
                        element_idx: idx,
                        commit_oid: row.commit_oid,
                        kind: GraphHitKind::Arrow {
                            up_row: *up_row as usize,
                            down_row: *down_row as usize,
                            target_row: *target_row as usize,
                            target_oid,
                            is_down: down,
                        },
                    });
                }
            }
        }
        for (idx, item) in row.elements.iter().enumerate() {
            if let GraphPrintElement::Edge {
                from_pos,
                to_pos,
                direction,
                up_row,
                down_row,
                ..
            } = item.element
            {
                let down = direction == EdgeDirection::Down;
                if g.is_over_edge(from_pos, to_pos, down, local_x, local_y) {
                    return Some(HitTarget::GraphPrintElement {
                        visible_row,
                        element_idx: idx,
                        commit_oid: row.commit_oid,
                        kind: GraphHitKind::Edge {
                            up_row: up_row as usize,
                            down_row: down_row as usize,
                        },
                    });
                }
            }
        }
        None
    }

    /// Copies the viewport rows (reusing allocations), resolves colors and
    /// returns the content hash.
    fn capture(
        &mut self,
        slice: &GraphViewportSlice<'_>,
        rows: usize,
        area: Rect,
        ctx: &RenderContext<'_>,
    ) -> u64 {
        self.color_map.clear();
        self.color_map
            .extend(slice.color_map.iter().map(|(k, v)| (*k, *v)));
        if self.rows.len() < rows {
            self.rows.resize_with(rows, OwnedRow::default);
        }
        self.rows_len = rows;

        let mut h = FxHasher::default();
        (
            self.protocol,
            self.cell_px,
            area.x,
            area.y,
            area.width,
            rows,
        )
            .hash(&mut h);
        ctx.theme.fg_bright.hash(&mut h);
        for (src, dst) in slice.rows.iter().take(rows).zip(self.rows.iter_mut()) {
            dst.visible_row = src.visible_row;
            dst.commit_oid = src.commit_oid;
            dst.row_bg = src.row_bg;
            dst.elements.clear();
            dst.elements.extend_from_slice(src.elements);
            dst.colors.clear();
            dst.colors.extend(src.elements.iter().map(|el| {
                let c = CellGridGraphBackend::resolve_color(
                    slice.color_map,
                    el.element.color_id(),
                    ctx,
                );
                Rgb::from_color(c, Rgb(187, 187, 187))
            }));
            src.row_bg.hash(&mut h);
            src.elements.hash(&mut h);
            dst.colors.hash(&mut h);
        }
        h.finish()
    }

    fn rasterize(&mut self, cols: u16, rows: usize, hover_outline: Rgb) {
        let w = u32::from(cols) * u32::from(self.cell_px.width);
        let row_h = u32::from(self.cell_px.height);
        let h = rows as u32 * row_h;
        let row_bytes = (w * row_h * 4) as usize;
        self.strip.resize(row_bytes * rows, 0);
        self.strip_size = (w, h);

        let geom = self.geometry;
        for (i, row) in self.rows.iter().take(rows).enumerate() {
            let bg = Rgb::from_color(row.row_bg, Rgb(0, 0, 0));
            let bytes = &mut self.strip[i * row_bytes..(i + 1) * row_bytes];
            let Some(mut pixmap) = PixmapMut::from_bytes(bytes, w, row_h) else {
                continue;
            };
            pixmap.fill(bg.to_skia());
            self.painter.paint_row(
                &mut pixmap,
                &geom,
                &row.elements,
                &row.colors,
                bg,
                hover_outline,
            );
        }
    }

    /// Cell-mapped hit regions using the reference `isOverEdge` / `isOverNode`
    /// formulas: a cell is hit when any pixel on its vertical center line lies on
    /// the element. Registration order edges → arrows → nodes mirrors the
    /// character grid, so the topmost target per cell matches in both modes.
    fn register_hits(&self, area: Rect, rows: usize, ctx: &mut RenderContext<'_>) {
        let g = &self.geometry;
        let cw = g.cell_width;
        let cols = i64::from(area.width);
        let col_of = |px: f32| (px / cw).floor() as i64;
        for (i, row) in self.rows.iter().take(rows).enumerate() {
            let row_y = area.y + i as u16;
            let visible_row = row.visible_row as usize;
            let mut register = |c0: i64, c1: i64, idx: usize, kind: GraphHitKind| {
                let (c0, c1) = (c0.max(0), c1.min(cols - 1));
                if c0 > c1 {
                    return;
                }
                ctx.hit_registry.register(
                    Rect::new(area.x + c0 as u16, row_y, (c1 - c0 + 1) as u16, 1),
                    GRAPH_ELEMENT_HIT_Z,
                    HitTarget::GraphPrintElement {
                        visible_row,
                        element_idx: idx,
                        commit_oid: row.commit_oid,
                        kind,
                    },
                );
            };
            for (idx, item) in row.elements.iter().enumerate() {
                if let GraphPrintElement::Edge {
                    from_pos,
                    to_pos,
                    direction,
                    up_row,
                    down_row,
                    ..
                } = item.element
                {
                    let down = direction == EdgeDirection::Down;
                    let (lo, hi) = g.edge_row_extent(from_pos, to_pos, down, false);
                    for c in col_of(lo.max(0.0))..=col_of(hi) {
                        let cx = c as f32 * cw + (cw / 2.0).floor();
                        let over = (0..g.row_height as i32)
                            .any(|y| g.is_over_edge(from_pos, to_pos, down, cx, y as f32));
                        if over {
                            register(
                                c,
                                c,
                                idx,
                                GraphHitKind::Edge {
                                    up_row: up_row as usize,
                                    down_row: down_row as usize,
                                },
                            );
                        }
                    }
                }
            }
            for (idx, item) in row.elements.iter().enumerate() {
                if let (
                    GraphPrintElement::TerminalEdge {
                        pos,
                        direction,
                        up_row,
                        down_row,
                        target_row,
                        ..
                    },
                    Some(target_oid),
                ) = (&item.element, item.target_oid)
                {
                    let down = *direction == EdgeDirection::Down;
                    let (lo, hi) = g.edge_row_extent(*pos, *pos, down, true);
                    for c in col_of(lo.max(0.0))..=col_of(hi) {
                        let cx = c as f32 * cw + (cw / 2.0).floor();
                        let over = (0..g.row_height as i32)
                            .any(|y| g.is_over_edge(*pos, *pos, down, cx, y as f32));
                        if over {
                            register(
                                c,
                                c,
                                idx,
                                GraphHitKind::Arrow {
                                    up_row: *up_row as usize,
                                    down_row: *down_row as usize,
                                    target_row: *target_row as usize,
                                    target_oid,
                                    is_down: down,
                                },
                            );
                        }
                    }
                }
            }
            for (idx, item) in row.elements.iter().enumerate() {
                if let GraphPrintElement::Node { pos, .. } = item.element {
                    let center = col_of(g.track_x(pos));
                    for c in center - 1..=center + 1 {
                        let cx = c as f32 * cw + (cw / 2.0).floor();
                        if c >= 0 && g.is_over_node(pos, cx, g.row_center) {
                            register(
                                c,
                                c,
                                idx,
                                GraphHitKind::Node {
                                    commit_oid: row.commit_oid,
                                },
                            );
                        }
                    }
                }
            }
        }
    }

    fn write_cells(
        &self,
        buf: &mut Buffer,
        area: Rect,
        rows: usize,
        occluding_rects: &[Rect],
        kitty_id: u32,
    ) {
        let mut sym = [0u8; 16];
        for (i, row) in self.rows.iter().take(rows).enumerate() {
            let y = area.y + i as u16;
            for x in 0..area.width {
                let cx = area.x + x;
                if is_occluded(cx, y, occluding_rects) {
                    continue;
                }
                let cell = &mut buf[(cx, y)];
                cell.reset();
                match self.protocol {
                    GraphicsProtocol::Kitty => {
                        let s = encode::kitty_placeholder_into(&mut sym, i as u16, x);
                        cell.set_symbol(s);
                        cell.set_fg(kitty_id_color(kitty_id));
                    }
                    GraphicsProtocol::Sixel | GraphicsProtocol::Iterm2 => {
                        cell.set_symbol(" ");
                        cell.set_fg(OVERLAY_MARKER_FG);
                    }
                }
                cell.set_bg(row.row_bg);
            }
        }
    }

    /// Graph cells overwritten after [`Self::try_render`] (menus, popups,
    /// dialogs, tooltips), as 1-row runs.
    fn collect_occlusion(&mut self, buf: &Buffer, p: &PendingFrame) {
        self.occluded.clear();
        for (i, row) in self.rows.iter().take(p.rows).enumerate() {
            let y = p.area.y + i as u16;
            let mut run_start: Option<u16> = None;
            for x in 0..=p.area.width {
                let covered = x < p.area.width && {
                    let cell = &buf[(p.area.x + x, y)];
                    !(cell.symbol() == " " && cell.fg == OVERLAY_MARKER_FG && cell.bg == row.row_bg)
                };
                match (covered, run_start) {
                    (true, None) => run_start = Some(x),
                    (false, Some(s)) => {
                        self.occluded.push(Rect::new(p.area.x + s, y, x - s, 1));
                        run_start = None;
                    }
                    _ => {}
                }
            }
        }
    }

    /// Post-overlay pass: Kitty transmit/delete, or Sixel/iTerm2 occlusion check
    /// (grid fallback around overlays) + positioned image emission.
    pub fn finish_frame(
        &mut self,
        buf: &mut Buffer,
        terminal_area: Rect,
        ctx: &RenderContext<'_>,
        grid: &mut CellGridGraphBackend,
        out: &mut Vec<u8>,
    ) {
        out.append(&mut self.cleanup);
        let Some(p) = self.pending.take() else {
            // Graph not on screen this frame: whatever image was there got overdrawn.
            self.emitted_key = None;
            return;
        };
        let (w, h) = self.strip_size;
        match self.protocol {
            GraphicsProtocol::Kitty => {
                if self.kitty_current.map(|c| c.id) == Some(p.kitty_id) {
                    return;
                }
                if !encode::encode_png(&self.strip, w, h, &mut self.png) {
                    return;
                }
                encode::kitty_transmit(
                    &self.png,
                    p.kitty_id,
                    p.area.width,
                    p.rows as u16,
                    &mut self.b64,
                    out,
                );
                if let Some(old) = self.kitty_current {
                    encode::kitty_delete(old.id, out);
                }
                self.kitty_current = Some(KittyImage {
                    id: p.kitty_id,
                    content_key: p.content_key,
                });
            }
            GraphicsProtocol::Sixel | GraphicsProtocol::Iterm2 => {
                self.collect_occlusion(buf, &p);
                if !self.occluded.is_empty() {
                    self.render_grid_fallback(buf, &p, ctx, grid);
                    self.emitted_key = None;
                    return;
                }
                let mut hasher = FxHasher::default();
                (p.content_key, terminal_area).hash(&mut hasher);
                let key = hasher.finish();
                if self.emitted_key == Some(key) {
                    return;
                }
                encode::cursor_save_and_move(p.area.x, p.area.y, out);
                if self.protocol == GraphicsProtocol::Sixel {
                    encode::encode_sixel(&self.strip, w, h, &mut self.sixel, out);
                } else if encode::encode_png(&self.strip, w, h, &mut self.png) {
                    encode::iterm2_inline(
                        &self.png,
                        p.area.width,
                        p.rows as u16,
                        &mut self.b64,
                        out,
                    );
                }
                encode::cursor_restore(out);
                self.emitted_key = Some(key);
            }
        }
    }

    /// Re-draws this frame's graph with the character grid on every cell that is
    /// not covered by an overlay (hit regions were already registered).
    fn render_grid_fallback(
        &mut self,
        buf: &mut Buffer,
        p: &PendingFrame,
        ctx: &RenderContext<'_>,
        grid: &mut CellGridGraphBackend,
    ) {
        // Drop the marker style so the grid paints on pristine cells (overlay
        // cells never carry the marker and are left untouched).
        for y in p.area.top()..p.area.bottom() {
            for x in p.area.left()..p.area.right() {
                let cell = &mut buf[(x, y)];
                if cell.fg == OVERLAY_MARKER_FG {
                    cell.reset();
                }
            }
        }
        self.scratch_hits.clear();
        let mut scratch_ctx = ctx.rebind_hits(&mut self.scratch_hits);
        for (i, r) in self.rows.iter().take(p.rows).enumerate() {
            let row_slice = [RowGraphSlice {
                visible_row: r.visible_row,
                commit_oid: r.commit_oid,
                row_bg: r.row_bg,
                elements: &r.elements,
            }];
            let slice = GraphViewportSlice {
                rows: &row_slice,
                area: Rect::new(p.area.x, p.area.y + i as u16, p.area.width, 1),
                occluding_rects: &self.occluded,
                color_map: &self.color_map,
            };
            grid.render_viewport(&slice, buf, &mut scratch_ctx);
        }
    }
}

fn other_kitty_id(id: u32) -> u32 {
    if id == KITTY_IMAGE_IDS[0] {
        KITTY_IMAGE_IDS[1]
    } else {
        KITTY_IMAGE_IDS[0]
    }
}

/// Kitty reads the placeholder's image id from its 24-bit foreground color.
pub fn kitty_id_color(id: u32) -> Color {
    Color::Rgb((id >> 16) as u8, (id >> 8) as u8, id as u8)
}

impl Rgb {
    /// Resolves a ratatui color to RGB (xterm defaults for indexed / named colors).
    pub fn from_color(c: Color, fallback: Rgb) -> Rgb {
        const BASE16: [(u8, u8, u8); 16] = [
            (0, 0, 0),
            (205, 0, 0),
            (0, 205, 0),
            (205, 205, 0),
            (0, 0, 238),
            (205, 0, 205),
            (0, 205, 205),
            (229, 229, 229),
            (127, 127, 127),
            (255, 0, 0),
            (0, 255, 0),
            (255, 255, 0),
            (92, 92, 255),
            (255, 0, 255),
            (0, 255, 255),
            (255, 255, 255),
        ];
        let indexed = |i: u8| -> Rgb {
            match i {
                0..=15 => {
                    let (r, g, b) = BASE16[i as usize];
                    Rgb(r, g, b)
                }
                16..=231 => {
                    let i = i - 16;
                    let lvl = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                    Rgb(lvl(i / 36), lvl((i / 6) % 6), lvl(i % 6))
                }
                _ => {
                    let v = 8 + (i - 232) * 10;
                    Rgb(v, v, v)
                }
            }
        };
        match c {
            Color::Rgb(r, g, b) => Rgb(r, g, b),
            Color::Indexed(i) => indexed(i),
            Color::Reset => fallback,
            Color::Black => indexed(0),
            Color::Red => indexed(1),
            Color::Green => indexed(2),
            Color::Yellow => indexed(3),
            Color::Blue => indexed(4),
            Color::Magenta => indexed(5),
            Color::Cyan => indexed(6),
            Color::Gray => indexed(7),
            Color::DarkGray => indexed(8),
            Color::LightRed => indexed(9),
            Color::LightGreen => indexed(10),
            Color::LightYellow => indexed(11),
            Color::LightBlue => indexed(12),
            Color::LightMagenta => indexed(13),
            Color::LightCyan => indexed(14),
            Color::White => indexed(15),
        }
    }
}

impl GraphRenderBackend for PixelVectorGraphBackend {
    fn render_viewport(
        &mut self,
        slice: &GraphViewportSlice<'_>,
        buf: &mut Buffer,
        ctx: &mut RenderContext<'_>,
    ) {
        if !self.try_render(slice, buf, ctx) {
            self.deactivate();
            self.fallback_grid.render_viewport(slice, buf, ctx);
        }
    }
}

#[cfg(test)]
mod tests;
