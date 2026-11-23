use std::cmp::Ordering;

use jbgit_core::BranchColorId;
use jbgit_core::graph::{EdgeDirection, GraphPrintElement, LineStyle, NodeKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use rustc_hash::FxHashMap;

use crate::app::RenderContext;
use crate::graph::{GraphRenderBackend, GraphViewportSlice, RowGraphSlice};
use crate::hit_test::{GraphHitKind, HitTarget};
use crate::theme::glyphs::GraphGlyphs;

const GRAPH_ELEMENT_HIT_Z: u8 = 2;

#[derive(Debug, Clone, Copy)]
struct ConnSlot {
    color: Color,
    style: LineStyle,
    selected: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct GraphCell {
    up: Option<ConnSlot>,
    down: Option<ConnSlot>,
    left: Option<ConnSlot>,
    right: Option<ConnSlot>,
}

impl GraphCell {
    #[inline]
    fn set_slot(slot: &mut Option<ConnSlot>, incoming: ConnSlot) {
        match slot {
            None => *slot = Some(incoming),
            Some(prev) => {
                if incoming.selected && !prev.selected {
                    *prev = incoming;
                }
            }
        }
    }

    fn compose(&self, glyphs: &GraphGlyphs) -> Option<(&'static str, Color, bool)> {
        let u = self.up.is_some();
        let d = self.down.is_some();
        let l = self.left.is_some();
        let r = self.right.is_some();
        if !(u || d || l || r) {
            return None;
        }

        let dashed = [self.up, self.down, self.left, self.right]
            .into_iter()
            .flatten()
            .all(|s| s.style == LineStyle::Dashed);

        let glyph = match (u, d, l, r) {
            (false, false, false, false) => " ",
            (true, true, false, false)
            | (true, false, false, false)
            | (false, true, false, false) => {
                if dashed {
                    glyphs.vertical_dashed
                } else {
                    glyphs.vertical
                }
            }
            (false, false, true, true)
            | (false, false, true, false)
            | (false, false, false, true) => {
                if dashed {
                    "╌"
                } else {
                    glyphs.horizontal
                }
            }
            (false, true, false, true) => glyphs.arc_down_right, // ╭
            (false, true, true, false) => glyphs.arc_down_left,  // ╮
            (true, false, false, true) => glyphs.arc_up_right,   // ╰
            (true, false, true, false) => glyphs.arc_up_left,    // ╯
            (true, true, false, true) => glyphs.tee_right,       // ├
            (true, true, true, false) => glyphs.tee_left,        // ┤
            (false, true, true, true) => "┬",
            (true, false, true, true) => "┴",
            (true, true, true, true) => glyphs.cross, // ┼
        };

        // Color arbitration: keep continuous vertical pass-through color on ├ / ┤ / ┼
        let color = match (self.up, self.down) {
            (Some(up_s), Some(dn_s)) if up_s.color == dn_s.color => up_s.color,
            (Some(up_s), Some(dn_s)) => {
                if dn_s.selected && !up_s.selected {
                    dn_s.color
                } else {
                    up_s.color
                }
            }
            (Some(s), None) | (None, Some(s)) => s.color,
            (None, None) => match (self.left, self.right) {
                (Some(ls), Some(rs)) => {
                    if rs.selected && !ls.selected {
                        rs.color
                    } else {
                        ls.color
                    }
                }
                (Some(s), None) | (None, Some(s)) => s.color,
                (None, None) => Color::Reset,
            },
        };

        let is_selected = [self.up, self.down, self.left, self.right]
            .into_iter()
            .flatten()
            .any(|s| s.selected);

        Some((glyph, color, is_selected))
    }
}

/// Default universal terminal cell-grid DAG renderer using a 4-way bitmask glyph composer.
#[derive(Debug, Default)]
pub struct CellGridGraphBackend {
    cells: Vec<GraphCell>,
}

impl CellGridGraphBackend {
    pub fn new() -> Self {
        Self {
            cells: Vec::with_capacity(64),
        }
    }

    pub(crate) fn resolve_color(
        color_map: &FxHashMap<BranchColorId, Color>,
        color_id: BranchColorId,
        ctx: &RenderContext<'_>,
    ) -> Color {
        color_map
            .get(&color_id)
            .copied()
            .unwrap_or_else(|| ctx.branch_colors.get_track_color(color_id))
    }

    fn render_row(
        &mut self,
        row_slice: &RowGraphSlice<'_>,
        row_y: u16,
        slice: &GraphViewportSlice<'_>,
        buf: &mut Buffer,
        ctx: &mut RenderContext<'_>,
    ) {
        let area = slice.area;
        let occluding_rects = slice.occluding_rects;
        let color_map = slice.color_map;
        let cols = area.width as usize;
        if cols == 0 {
            return;
        }
        self.cells.clear();
        self.cells.resize(cols, GraphCell::default());

        let glyphs = &ctx.glyph_mode.symbols().graph;
        let visible_row = row_slice.visible_row as usize;
        let mut overflowed = false;

        // 1. Paint background across graph slice first
        for x in 0..cols {
            let cx = area.x + x as u16;
            if is_occluded(cx, row_y, occluding_rects) {
                continue;
            }
            let cell = &mut buf[(cx, row_y)];
            cell.set_symbol(" ");
            cell.set_style(Style::default().bg(row_slice.row_bg));
        }

        // 2. Compose edges into the 4-way bitmask buffer
        for (elem_idx, item) in row_slice.elements.iter().enumerate() {
            if let GraphPrintElement::Edge {
                from_pos,
                to_pos,
                direction,
                style,
                up_row,
                down_row,
                color_id,
                ..
            } = item.element
            {
                let color = Self::resolve_color(color_map, color_id, ctx);
                let conn = ConnSlot {
                    color,
                    style,
                    selected: item.is_selected,
                };
                let from_col = from_pos * 2;
                let to_col = to_pos * 2;
                if from_col >= cols || to_col >= cols {
                    overflowed = true;
                }

                let hit = HitTarget::GraphPrintElement {
                    visible_row,
                    element_idx: elem_idx,
                    commit_oid: row_slice.commit_oid,
                    kind: GraphHitKind::Edge {
                        up_row: up_row as usize,
                        down_row: down_row as usize,
                    },
                };

                match from_pos.cmp(&to_pos) {
                    Ordering::Equal => {
                        if from_col < cols {
                            match direction {
                                EdgeDirection::Up => {
                                    GraphCell::set_slot(&mut self.cells[from_col].up, conn)
                                }
                                EdgeDirection::Down => {
                                    GraphCell::set_slot(&mut self.cells[from_col].down, conn)
                                }
                            }
                            ctx.hit_registry.register(
                                Rect::new(area.x + from_col as u16, row_y, 1, 1),
                                GRAPH_ELEMENT_HIT_Z,
                                hit,
                            );
                        }
                    }
                    Ordering::Less => {
                        if from_col < cols {
                            GraphCell::set_slot(&mut self.cells[from_col].right, conn);
                        }
                        let end = to_col.min(cols);
                        let start = (from_col + 1).min(end);
                        for cell in &mut self.cells[start..end] {
                            GraphCell::set_slot(&mut cell.left, conn);
                            GraphCell::set_slot(&mut cell.right, conn);
                        }
                        if to_col < cols {
                            GraphCell::set_slot(&mut self.cells[to_col].left, conn);
                            match direction {
                                EdgeDirection::Down => {
                                    GraphCell::set_slot(&mut self.cells[to_col].down, conn)
                                }
                                EdgeDirection::Up => {
                                    GraphCell::set_slot(&mut self.cells[to_col].up, conn)
                                }
                            }
                        }
                        let span_start = from_col.min(cols);
                        let span_end = (to_col + 1).min(cols);
                        if span_end > span_start {
                            ctx.hit_registry.register(
                                Rect::new(
                                    area.x + span_start as u16,
                                    row_y,
                                    (span_end - span_start) as u16,
                                    1,
                                ),
                                GRAPH_ELEMENT_HIT_Z,
                                hit,
                            );
                        }
                    }
                    Ordering::Greater => {
                        if from_col < cols {
                            match direction {
                                EdgeDirection::Down => {
                                    GraphCell::set_slot(&mut self.cells[from_col].down, conn)
                                }
                                EdgeDirection::Up => {
                                    GraphCell::set_slot(&mut self.cells[from_col].up, conn)
                                }
                            }
                            ctx.hit_registry.register(
                                Rect::new(area.x + from_col as u16, row_y, 1, 1),
                                GRAPH_ELEMENT_HIT_Z,
                                hit,
                            );
                        }
                    }
                }
            }
        }

        // 3. Flush composed edge glyphs into buffer
        for (x, gcell) in self.cells.iter().enumerate() {
            let cx = area.x + x as u16;
            if is_occluded(cx, row_y, occluding_rects) {
                continue;
            }
            if let Some((sym, fg, is_sel)) = gcell.compose(glyphs) {
                let mut style = Style::default().fg(fg).bg(row_slice.row_bg);
                if is_sel {
                    style = style.add_modifier(Modifier::BOLD);
                }
                let cell = &mut buf[(cx, row_y)];
                cell.set_symbol(sym);
                cell.set_style(style);
            }
        }

        // 4. Overlay TerminalEdge arrows (▾ / ▴) and ShallowParent indicators
        for (elem_idx, item) in row_slice.elements.iter().enumerate() {
            match item.element {
                GraphPrintElement::TerminalEdge {
                    pos,
                    direction,
                    up_row,
                    down_row,
                    target_row,
                    color_id,
                    ..
                } => {
                    let col = pos * 2;
                    if col >= cols {
                        overflowed = true;
                        continue;
                    }
                    let cx = area.x + col as u16;
                    if is_occluded(cx, row_y, occluding_rects) {
                        continue;
                    }
                    let fg = Self::resolve_color(color_map, color_id, ctx);
                    let sym = match direction {
                        EdgeDirection::Down => glyphs.arrow_down,
                        EdgeDirection::Up => glyphs.arrow_up,
                    };
                    let mut style = Style::default().fg(fg).bg(row_slice.row_bg);
                    if item.is_selected {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    let cell = &mut buf[(cx, row_y)];
                    cell.set_symbol(sym);
                    cell.set_style(style);

                    if let Some(target_oid) = item.target_oid {
                        ctx.hit_registry.register(
                            Rect::new(cx, row_y, 1, 1),
                            GRAPH_ELEMENT_HIT_Z,
                            HitTarget::GraphPrintElement {
                                visible_row,
                                element_idx: elem_idx,
                                commit_oid: row_slice.commit_oid,
                                kind: GraphHitKind::Arrow {
                                    up_row: up_row as usize,
                                    down_row: down_row as usize,
                                    target_row: target_row as usize,
                                    target_oid,
                                    is_down: direction == EdgeDirection::Down,
                                },
                            },
                        );
                    }
                }
                GraphPrintElement::ShallowParent { pos, color_id, .. } => {
                    let tail_col = pos * 2 + 1;
                    if tail_col < cols && self.cells[tail_col].compose(glyphs).is_none() {
                        let cx = area.x + tail_col as u16;
                        if !is_occluded(cx, row_y, occluding_rects) {
                            let fg = Self::resolve_color(color_map, color_id, ctx);
                            let cell = &mut buf[(cx, row_y)];
                            cell.set_symbol(glyphs.vertical_dashed);
                            cell.set_style(Style::default().fg(fg).bg(row_slice.row_bg));
                        }
                    }
                }
                _ => {}
            }
        }

        // 5. Overlay Node glyphs (● / ◎) with highest draw priority
        for (elem_idx, item) in row_slice.elements.iter().enumerate() {
            if let GraphPrintElement::Node {
                pos,
                color_id,
                kind,
                collapsed_below,
                ..
            } = item.element
            {
                let col = pos * 2;
                if col >= cols {
                    overflowed = true;
                    continue;
                }
                let cx = area.x + col as u16;
                if is_occluded(cx, row_y, occluding_rects) {
                    continue;
                }
                let track_fg = Self::resolve_color(color_map, color_id, ctx);
                let (sym, fg, bold) = match kind {
                    // Fold marker (a collapsed linear branch hangs below) wins
                    // the shape; HEAD keeps its color.
                    NodeKind::Head if collapsed_below => {
                        (glyphs.node_hollow, ctx.theme.ref_head.icon, true)
                    }
                    NodeKind::Head => (glyphs.node_head, ctx.theme.ref_head.icon, true),
                    NodeKind::Merge | NodeKind::Normal if collapsed_below => {
                        (glyphs.node_hollow, track_fg, true)
                    }
                    NodeKind::Merge | NodeKind::Normal => (glyphs.node, track_fg, item.is_selected),
                };
                let mut style = Style::default().fg(fg).bg(row_slice.row_bg);
                if bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                let cell = &mut buf[(cx, row_y)];
                cell.set_symbol(sym);
                cell.set_style(style);

                ctx.hit_registry.register(
                    Rect::new(cx, row_y, 1, 1),
                    GRAPH_ELEMENT_HIT_Z,
                    HitTarget::GraphPrintElement {
                        visible_row,
                        element_idx: elem_idx,
                        commit_oid: row_slice.commit_oid,
                        kind: GraphHitKind::Node {
                            commit_oid: row_slice.commit_oid,
                            collapsed_below,
                        },
                    },
                );
            }
        }

        // 6. Soft horizontal overflow ellipsis when tracks exceed graph budget
        if overflowed && cols >= 1 {
            let cx = area.x + (cols - 1) as u16;
            if !is_occluded(cx, row_y, occluding_rects) {
                let cell = &mut buf[(cx, row_y)];
                cell.set_symbol("…");
                cell.set_style(Style::default().fg(ctx.theme.fg_muted).bg(row_slice.row_bg));
            }
        }
    }
}

#[inline]
pub(crate) fn is_occluded(col: u16, row: u16, occluding_rects: &[Rect]) -> bool {
    occluding_rects.iter().any(|r| {
        r.width > 0
            && r.height > 0
            && col >= r.x
            && col < r.x.saturating_add(r.width)
            && row >= r.y
            && row < r.y.saturating_add(r.height)
    })
}

impl GraphRenderBackend for CellGridGraphBackend {
    fn render_viewport(
        &mut self,
        slice: &GraphViewportSlice<'_>,
        buf: &mut Buffer,
        ctx: &mut RenderContext<'_>,
    ) {
        if slice.area.width == 0 || slice.area.height == 0 {
            return;
        }
        let max_rows = (slice.area.height as usize).min(slice.rows.len());
        for (i, row_slice) in slice.rows.iter().take(max_rows).enumerate() {
            let y = slice.area.y.saturating_add(i as u16);
            self.render_row(row_slice, y, slice, buf, ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use jbgit_core::graph::{
        FixedBitSet, GraphViewOptions, PermanentGraph, PrintElementGenerator, RawCommitNode,
        SortType, VisibleGraph, VisibleRowIdx,
    };
    use jbgit_core::{Oid, RefInfo, RefKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::graph::state::build_graph_color_map;
    use crate::hit_test::HitTestRegistry;
    use crate::theme::{GlyphMode, ThemePalette};

    fn oid(n: u64) -> Oid {
        let mut bytes = [0u8; 20];
        bytes[..8].copy_from_slice(&n.to_le_bytes());
        Oid::from_bytes(bytes)
    }

    fn raw(id: u64, parents: &[u64], ts: i64) -> RawCommitNode {
        RawCommitNode {
            oid: oid(id),
            parents: parents.iter().copied().map(oid).collect(),
            timestamp: ts,
        }
    }

    fn branch_ref(name: &str, target: u64, is_head: bool) -> RefInfo {
        RefInfo {
            name: name.to_string(),
            full_name: format!("refs/heads/{name}"),
            target_oid: oid(target),
            kind: RefKind::LocalBranch,
            upstream: None,
            ahead_behind: None,
            is_head,
        }
    }

    fn render_ascii_snapshot(
        commits: &[RawCommitNode],
        refs: &[RefInfo],
        head_oid: Option<Oid>,
        options: GraphViewOptions,
        filter: Option<&FixedBitSet>,
        graph_width: u16,
    ) -> (Vec<String>, String, Buffer, HitTestRegistry) {
        let theme = ThemePalette::darcula(true);
        let mgr = theme.branch_color_manager();
        let pg = PermanentGraph::build(commits, refs, head_oid);
        let vis = VisibleGraph::build(&pg, options, filter);
        let color_map = build_graph_color_map(&pg, &mgr);

        let n = vis.linear.visible_count();
        let mut elem_gen = PrintElementGenerator::new(&pg, &vis, head_oid);
        let mut rows_elems: Vec<Vec<crate::graph::RenderGraphElement>> =
            Vec::with_capacity(n as usize);
        for r in 0..n {
            let els = elem_gen
                .print_elements(r)
                .into_iter()
                .map(|el| {
                    let target_oid = match &el {
                        GraphPrintElement::TerminalEdge { target_perm_id, .. } => {
                            Some(pg.linear.commit(*target_perm_id).oid)
                        }
                        _ => None,
                    };
                    crate::graph::RenderGraphElement {
                        element: el,
                        target_oid,
                        is_selected: false,
                    }
                })
                .collect();
            rows_elems.push(els);
        }

        let slices: Vec<RowGraphSlice<'_>> = (0..n)
            .map(|r| RowGraphSlice {
                visible_row: r as VisibleRowIdx,
                commit_oid: pg.linear.commit(vis.linear.perm_id(r)).oid,
                row_bg: theme.bg_canvas,
                elements: &rows_elems[r as usize],
            })
            .collect();

        let h = n.max(1) as u16;
        let mut terminal = Terminal::new(TestBackend::new(graph_width, h)).unwrap();
        let mut hits = HitTestRegistry::new();
        let mut backend = CellGridGraphBackend::new();

        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, graph_width, h);
                let mut ctx = RenderContext::minimal(
                    &theme,
                    GlyphMode::Unicode,
                    &mgr,
                    &mut hits,
                );
                let slice = GraphViewportSlice {
                    rows: &slices,
                    area,
                    occluding_rects: &[],
                    color_map: &color_map,
                };
                backend.render_viewport(&slice, f.buffer_mut(), &mut ctx);
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let lines: Vec<String> = (0..h)
            .map(|y| {
                (0..graph_width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        let ansi_snapshot = format_ansi_rows(&buf, graph_width, h);
        (lines, ansi_snapshot, buf, hits)
    }

    fn format_ansi_rows(buf: &Buffer, width: u16, height: u16) -> String {
        let mut out = Vec::with_capacity(height as usize);
        for y in 0..height {
            let mut end_col = width;
            while end_col > 0 && buf[(end_col - 1, y)].symbol() == " " {
                end_col -= 1;
            }
            let mut row = String::new();
            let mut prev_fg: Option<Color> = None;
            for x in 0..end_col {
                let cell = &buf[(x, y)];
                let sym = cell.symbol();
                if sym == " " {
                    if prev_fg.take().is_some() {
                        row.push_str("\x1b[0m");
                    }
                    row.push(' ');
                    continue;
                }
                if prev_fg != Some(cell.fg) {
                    if let Color::Rgb(r, g, b) = cell.fg {
                        row.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
                    }
                    prev_fg = Some(cell.fg);
                }
                row.push_str(sym);
            }
            if prev_fg.is_some() {
                row.push_str("\x1b[0m");
            }
            out.push(row);
        }
        out.join("\n")
    }

    #[test]
    fn snapshot_linear_chain() {
        let commits = vec![raw(0, &[1], 30), raw(1, &[2], 20), raw(2, &[], 10)];
        let refs = vec![branch_ref("main", 0, true)];
        let (lines, ansi, _, _) = render_ascii_snapshot(
            &commits,
            &refs,
            Some(oid(0)),
            GraphViewOptions::default(),
            None,
            6,
        );
        assert_eq!(lines, vec!["◎", "●", "●"]);
        insta::assert_snapshot!(ansi, @r"
        [38;2;225;199;49m◎[0m
        [38;2;255;98;133m●[0m
        [38;2;255;98;133m●[0m
        ");
    }

    #[test]
    fn snapshot_fork_and_merge_nested_topology() {
        // Exact 8-row nested merge topology.
        let commits = vec![
            raw(0, &[1], 80),    // main HEAD
            raw(1, &[6, 2], 70), // Merge PR #42
            raw(2, &[3], 60),    // feat/ui
            raw(3, &[5, 4], 50), // Merge feat/parser
            raw(4, &[5], 40),    // feat(parser)
            raw(5, &[6], 30),    // feat(ui)
            raw(6, &[7], 20),    // fix: memory race
            raw(7, &[], 10),     // init
        ];
        let refs = vec![branch_ref("main", 0, true), branch_ref("feat/ui", 2, false)];
        let (lines, ansi, buf, _) = render_ascii_snapshot(
            &commits,
            &refs,
            Some(oid(0)),
            GraphViewOptions {
                sort_type: SortType::Topo,
                ..Default::default()
            },
            None,
            8,
        );
        assert_eq!(
            lines,
            vec!["◎", "●─╮", "│ ●", "│ ●─╮", "│ │ ●", "│ ●─╯", "●─╯", "●",]
        );

        // Verify color arbitration on Row 1 (`●─╮`):
        // col 1 (`─`) and col 2 (`╮`) take feat/ui's color, while main's track stays main's color!
        let main_color = buf[(0, 7)].fg;
        let feat_color = buf[(2, 2)].fg;
        assert_ne!(main_color, feat_color);
        assert_eq!(buf[(1, 1)].fg, feat_color);
        assert_eq!(buf[(2, 1)].fg, feat_color);
        assert_eq!(buf[(2, 6)].fg, feat_color);
        assert_eq!(buf[(1, 6)].fg, feat_color);
        insta::assert_snapshot!(ansi);
    }

    #[test]
    fn snapshot_two_branches_merging_at_same_commit() {
        // 0 (main) -> [3, 1, 2], 1 -> [3], 2 -> [3], 3 -> []
        let commits = vec![
            raw(0, &[3, 1, 2], 40),
            raw(1, &[3], 30),
            raw(2, &[3], 20),
            raw(3, &[], 10),
        ];
        let refs = vec![branch_ref("main", 0, true)];
        let (lines, ansi, _, _) = render_ascii_snapshot(
            &commits,
            &refs,
            Some(oid(0)),
            GraphViewOptions {
                sort_type: SortType::Topo,
                ..Default::default()
            },
            None,
            8,
        );
        assert_eq!(lines, vec!["◎─┬─╮", "│ ● │", "│ │ ●", "●─┴─╯"]);
        insta::assert_snapshot!(ansi);
    }

    #[test]
    fn snapshot_criss_cross_merge() {
        let commits = vec![
            raw(0, &[2, 3], 50),
            raw(1, &[3, 2], 45),
            raw(2, &[4], 30),
            raw(3, &[4], 25),
            raw(4, &[], 10),
        ];
        let refs = vec![branch_ref("main", 0, true), branch_ref("dev", 1, false)];
        let (lines, ansi, _, _) = render_ascii_snapshot(
            &commits,
            &refs,
            Some(oid(0)),
            GraphViewOptions {
                sort_type: SortType::Topo,
                ..Default::default()
            },
            None,
            8,
        );
        assert_eq!(lines.len(), 5);
        insta::assert_snapshot!(lines.join("\n"));
        insta::assert_snapshot!(ansi);
    }

    #[test]
    fn snapshot_long_edge_arrows_and_filtered_dashed_edges() {
        // Long edge (0 -> 35) spanning 35 rows
        let mut commits = Vec::new();
        for i in 0..=35u64 {
            let parents = if i == 0 {
                vec![1, 35]
            } else if i < 35 {
                vec![i + 1]
            } else {
                vec![]
            };
            commits.push(raw(i, &parents, (100 - i) as i64));
        }
        let refs = vec![branch_ref("main", 0, true)];
        let (lines, ansi_long, _, hits) = render_ascii_snapshot(
            &commits,
            &refs,
            Some(oid(0)),
            GraphViewOptions {
                sort_type: SortType::Topo,
                show_long_edges: false,
                ..Default::default()
            },
            None,
            6,
        );
        assert_eq!(lines[0], "◎─╮");
        assert!(lines[1].contains('▾'), "row 1: {}", lines[1]);
        assert_eq!(lines[15], "●");
        assert!(lines[34].contains('▴'), "row 34: {}", lines[34]);
        assert_eq!(lines[35], "●─╯");
        insta::assert_snapshot!(format!(
            "{}\n{}\n{}\n{}\n{}",
            lines[0], lines[1], lines[15], lines[34], lines[35]
        ));
        insta::assert_snapshot!(ansi_long);

        let arrow_x = lines[1].chars().position(|c| c == '▾').unwrap() as u16;
        let hit = hits.hit_test(arrow_x, 1).unwrap();
        assert_eq!(
            hit.target,
            HitTarget::GraphPrintElement {
                visible_row: 1,
                element_idx: 2,
                commit_oid: oid(1),
                kind: GraphHitKind::Arrow {
                    up_row: 0,
                    down_row: 35,
                    target_row: 35,
                    target_oid: oid(35),
                    is_down: true,
                },
            }
        );

        // Filtered dashed edge spanning across intervening visible commits (renders ╌ and ┆)
        // 0 -> [3, 1], 3 -> [2], 2 -> [4], 1 -> [4], 4 -> []
        // Filter keeps {0, 3, 2, 4} (hides 1), so 0 -> 4 is a Dotted edge passing rows 1 and 2!
        let small = vec![
            raw(0, &[3, 1], 50),
            raw(1, &[4], 40),
            raw(3, &[2], 30),
            raw(2, &[4], 20),
            raw(4, &[], 10),
        ];
        let mut mask = FixedBitSet::with_capacity(5);
        mask.insert(0);
        mask.insert(2);
        mask.insert(3);
        mask.insert(4);
        let (filtered_lines, ansi_filtered, _, _) = render_ascii_snapshot(
            &small,
            &[branch_ref("main", 0, true)],
            Some(oid(0)),
            GraphViewOptions {
                sort_type: SortType::Topo,
                ..Default::default()
            },
            Some(&mask),
            6,
        );
        assert_eq!(filtered_lines, vec!["◎─╮", "┆ ●", "┆ ●", "●─╯"]);
        insta::assert_snapshot!(ansi_filtered);
    }

    #[test]
    fn narrow_graph_width_truncates_with_ellipsis_without_panic() {
        let commits = vec![
            raw(0, &[3, 1, 2], 40),
            raw(1, &[3], 30),
            raw(2, &[3], 20),
            raw(3, &[], 10),
        ];
        let refs = vec![branch_ref("main", 0, true)];
        let (lines, _, _, _) = render_ascii_snapshot(
            &commits,
            &refs,
            Some(oid(0)),
            GraphViewOptions {
                sort_type: SortType::Topo,
                ..Default::default()
            },
            None,
            2,
        );
        assert_eq!(lines[0], "◎…");
    }
}
