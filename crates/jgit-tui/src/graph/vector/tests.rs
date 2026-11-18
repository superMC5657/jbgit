//! Golden-image, frame-protocol and degradation tests for the vector backend.

use jgit_core::graph::{
    EdgeDirection, FixedBitSet, GraphPrintElement, GraphViewOptions, LineStyle, NodeKind,
    PermanentGraph, PrintElementGenerator, RawCommitNode, SortType, VisibleGraph, VisibleRowIdx,
};
use jgit_core::{BranchColorId, Oid, PaneId, RefInfo, RefKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Clear, Widget};
use rustc_hash::FxHashMap;

use super::*;
use crate::graph::renderer::{
    GraphRenderer, GraphRendererMode, GraphicsCapabilities, ResolvedGraphBackend,
};
use crate::graph::state::build_graph_color_map;
use crate::hit_test::HitTestRegistry;
use crate::theme::{BranchColorManager, GlyphMode, ThemePalette};

const CELL: CellPixelSize = CellPixelSize {
    width: 9,
    height: 18,
};

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

/// Print elements for every visible row of a commit DAG, plus colors.
struct Fixture {
    theme: ThemePalette,
    mgr: BranchColorManager,
    color_map: FxHashMap<BranchColorId, Color>,
    rows: Vec<Vec<RenderGraphElement>>,
    oids: Vec<Oid>,
}

impl Fixture {
    fn graph(
        commits: &[RawCommitNode],
        refs: &[RefInfo],
        head: Option<Oid>,
        options: GraphViewOptions,
        filter: Option<&FixedBitSet>,
    ) -> Self {
        let theme = ThemePalette::darcula(true);
        let mgr = theme.branch_color_manager();
        let pg = PermanentGraph::build(commits, refs, head);
        let vis = VisibleGraph::build(&pg, options, filter);
        let color_map = build_graph_color_map(&pg, &mgr);
        let mut generator = PrintElementGenerator::new(&pg, &vis, head);
        let n = vis.linear.visible_count();
        let rows = (0..n)
            .map(|r| {
                generator
                    .print_elements(r)
                    .into_iter()
                    .map(|el| {
                        let target_oid = match &el {
                            GraphPrintElement::TerminalEdge { target_perm_id, .. } => {
                                Some(pg.linear.commit(*target_perm_id).oid)
                            }
                            _ => None,
                        };
                        RenderGraphElement {
                            element: el,
                            target_oid,
                            is_selected: false,
                        }
                    })
                    .collect()
            })
            .collect();
        let oids = (0..n)
            .map(|r| pg.linear.commit(vis.linear.perm_id(r)).oid)
            .collect();
        Self {
            theme,
            mgr,
            color_map,
            rows,
            oids,
        }
    }

    /// Hand-built rows with explicit colors (ids 1 / 2 → red / blue).
    fn manual(rows: Vec<Vec<GraphPrintElement>>) -> Self {
        let theme = ThemePalette::darcula(true);
        let mgr = theme.branch_color_manager();
        let mut color_map = FxHashMap::default();
        color_map.insert(BranchColorId(1), Color::Rgb(230, 40, 40));
        color_map.insert(BranchColorId(2), Color::Rgb(40, 90, 240));
        let oids = (0..rows.len() as u64).map(oid).collect();
        let rows = rows
            .into_iter()
            .map(|els| {
                els.into_iter()
                    .map(|element| RenderGraphElement {
                        element,
                        target_oid: None,
                        is_selected: false,
                    })
                    .collect()
            })
            .collect();
        Self {
            theme,
            mgr,
            color_map,
            rows,
            oids,
        }
    }

    fn fork_and_merge() -> Self {
        let commits = vec![
            raw(0, &[1], 80),
            raw(1, &[6, 2], 70),
            raw(2, &[3], 60),
            raw(3, &[5, 4], 50),
            raw(4, &[5], 40),
            raw(5, &[6], 30),
            raw(6, &[7], 20),
            raw(7, &[], 10),
        ];
        let refs = vec![branch_ref("main", 0, true), branch_ref("feat/ui", 2, false)];
        Self::graph(&commits, &refs, Some(oid(0)), topo(), None)
    }

    fn long_edge() -> Self {
        let commits: Vec<_> = (0..=35u64)
            .map(|i| {
                let parents = match i {
                    0 => vec![1, 35],
                    35 => vec![],
                    _ => vec![i + 1],
                };
                raw(i, &parents, (100 - i) as i64)
            })
            .collect();
        Self::graph(
            &commits,
            &[branch_ref("main", 0, true)],
            Some(oid(0)),
            topo(),
            None,
        )
    }

    fn filtered_dashed() -> Self {
        let commits = vec![
            raw(0, &[3, 1], 50),
            raw(1, &[4], 40),
            raw(3, &[2], 30),
            raw(2, &[4], 20),
            raw(4, &[], 10),
        ];
        let mut mask = FixedBitSet::with_capacity(5);
        for i in [0, 2, 3, 4] {
            mask.insert(i);
        }
        Self::graph(
            &commits,
            &[branch_ref("main", 0, true)],
            Some(oid(0)),
            topo(),
            Some(&mask),
        )
    }

    fn slices(&self, bg: impl Fn(usize) -> Color) -> Vec<RowGraphSlice<'_>> {
        self.rows
            .iter()
            .enumerate()
            .map(|(i, els)| RowGraphSlice {
                visible_row: i as VisibleRowIdx,
                commit_oid: self.oids[i],
                row_bg: bg(i),
                elements: els,
            })
            .collect()
    }

    fn ctx<'a>(&'a self, hits: &'a mut HitTestRegistry) -> RenderContext<'a> {
        RenderContext {
            theme: &self.theme,
            glyph_mode: GlyphMode::Unicode,
            branch_colors: &self.mgr,
            hit_registry: hits,
            focused_pane: PaneId::LogCommitTable,
            hover_target: None,
            dragging_splitter: None,
            graph_renderer: None,
        }
    }

    fn bg(&self) -> Color {
        self.theme.bg_canvas
    }
}

fn topo() -> GraphViewOptions {
    GraphViewOptions {
        sort_type: SortType::Topo,
        ..Default::default()
    }
}

fn edge(row: u32, from: usize, to: usize, dir: EdgeDirection, color: i32) -> GraphPrintElement {
    GraphPrintElement::Edge {
        row,
        from_pos: from,
        to_pos: to,
        direction: dir,
        style: LineStyle::Solid,
        up_row: 0,
        down_row: 1,
        color_id: BranchColorId(color),
    }
}

/// Rasterizes `fx` directly through the backend; returns it with buffer + hits.
fn rasterize(
    fx: &Fixture,
    protocol: GraphicsProtocol,
    cell: CellPixelSize,
    cols: u16,
) -> (PixelVectorGraphBackend, Buffer, HitTestRegistry) {
    let rows = fx.rows.len() as u16;
    let area = Rect::new(0, 0, cols, rows);
    let mut buf = Buffer::empty(area);
    let mut hits = HitTestRegistry::new();
    let mut backend = PixelVectorGraphBackend::new(protocol, cell);
    let slices = fx.slices(|_| fx.bg());
    let slice = GraphViewportSlice {
        rows: &slices,
        area,
        occluding_rects: &[],
        color_map: &fx.color_map,
    };
    assert!(backend.try_render(&slice, &mut buf, &mut fx.ctx(&mut hits)));
    (backend, buf, hits)
}

fn pixel(backend: &PixelVectorGraphBackend, x: u32, y: u32) -> Rgb {
    let (data, w, _) = backend.strip();
    let o = ((y * w + x) * 4) as usize;
    Rgb(data[o], data[o + 1], data[o + 2])
}

/// Normalized max-channel distance between two colors (0 = equal, 1 = black↔white).
fn diff(a: Rgb, b: Rgb) -> f32 {
    [(a.0, b.0), (a.1, b.1), (a.2, b.2)]
        .iter()
        .map(|&(x, y)| (f32::from(x) - f32::from(y)).abs())
        .fold(0.0, f32::max)
        / 255.0
}

fn row_ink(backend: &PixelVectorGraphBackend, y: u32, bg: Rgb) -> f32 {
    let (_, w, _) = backend.strip();
    (0..w).map(|x| diff(pixel(backend, x, y), bg)).sum()
}

// ── Golden image tests ──────────────────────────────────────────────────────

#[test]
fn strip_is_one_contiguous_pixmap_prefilled_with_each_row_background() {
    let fx = Fixture::fork_and_merge();
    let rows = fx.rows.len();
    let area = Rect::new(0, 0, 12, rows as u16);
    let mut buf = Buffer::empty(area);
    let mut hits = HitTestRegistry::new();
    let mut backend = PixelVectorGraphBackend::new(GraphicsProtocol::Sixel, CELL);
    let bgs = [Color::Rgb(43, 43, 43), Color::Rgb(13, 41, 62)];
    let slices = fx.slices(|i| bgs[i % 2]);
    let slice = GraphViewportSlice {
        rows: &slices,
        area,
        occluding_rects: &[],
        color_map: &fx.color_map,
    };
    assert!(backend.try_render(&slice, &mut buf, &mut fx.ctx(&mut hits)));

    let (data, w, h) = backend.strip();
    assert_eq!((w, h), (12 * 9, rows as u32 * 18));
    assert_eq!(data.len(), (w * h * 4) as usize);
    for i in 0..rows as u32 {
        let want = Rgb::from_color(bgs[i as usize % 2], Rgb(0, 0, 0));
        // Right edge of the strip is track-free: every pixel row shows its row_bg.
        for y in i * 18..(i + 1) * 18 {
            assert_eq!(pixel(&backend, w - 1, y), want, "row {i} y {y}");
        }
    }
}

#[test]
fn slanted_edges_dock_across_row_boundaries_without_gaps() {
    let fx = Fixture::fork_and_merge();
    let (backend, _, _) = rasterize(&fx, GraphicsProtocol::Sixel, CELL, 12);
    let g = *backend.geometry();
    let bg = Rgb::from_color(fx.bg(), Rgb(0, 0, 0));
    let h = CELL.height as u32;

    let mut slanted_checked = 0;
    for (r, row) in fx.rows.iter().enumerate().take(fx.rows.len() - 1) {
        let boundary = (r as u32 + 1) * h;
        // Total ink in the two pixel rows touching the boundary must match: a
        // 1px seam would leave one of them (nearly) empty.
        let above = row_ink(&backend, boundary - 1, bg);
        let below = row_ink(&backend, boundary, bg);
        assert!(
            above > 0.5 && below > 0.5,
            "boundary {boundary}: {above} / {below}"
        );
        assert!(
            (above - below).abs() <= 0.35 * above.max(below),
            "boundary {boundary} seam: {above} vs {below}"
        );

        for el in row {
            if let GraphPrintElement::Edge {
                from_pos,
                to_pos,
                direction: EdgeDirection::Down,
                ..
            } = el.element
                && from_pos != to_pos
            {
                // The double-length segment crosses the boundary at its midpoint.
                let xm = (g.track_x(from_pos) + g.track_x(to_pos)) / 2.0 + 0.5;
                for y in [boundary - 1, boundary] {
                    let best = (xm as u32 - 3..=xm as u32 + 3)
                        .map(|x| diff(pixel(&backend, x, y), bg))
                        .fold(0.0, f32::max);
                    assert!(best > 0.3, "edge {from_pos}->{to_pos} at y={y}: ink {best}");
                }
                slanted_checked += 1;
            }
        }
    }
    assert!(
        slanted_checked >= 3,
        "fixture must exercise forks and merges"
    );
}

#[test]
fn crossing_center_keeps_the_upper_track_antialiased_color() {
    // Track 0 → 2 (red) and 1 → 0 (blue) cross inside row 0 at t = 1/3.
    for (first, second) in [(1, 2), (2, 1)] {
        let fx = Fixture::manual(vec![
            vec![
                edge(
                    0,
                    if first == 1 { 0 } else { 1 },
                    if first == 1 { 2 } else { 0 },
                    EdgeDirection::Down,
                    first,
                ),
                edge(
                    0,
                    if second == 1 { 0 } else { 1 },
                    if second == 1 { 2 } else { 0 },
                    EdgeDirection::Down,
                    second,
                ),
            ],
            vec![],
        ]);
        let (backend, _, _) = rasterize(&fx, GraphicsProtocol::Sixel, CELL, 8);
        let g = *backend.geometry();
        // Red: x = x0 + 2·ew·t, blue: x = x1 − ew·t → t = 1/3.
        let t = 1.0 / 3.0;
        let x = g.track_x(0) + 2.0 * g.element_width * t + 0.5;
        let y = g.row_center + g.row_height * t + 0.5;
        assert!(y < g.row_height, "crossing lies inside row 0");
        let c = pixel(&backend, x as u32, y as u32);
        let red = Rgb(230, 40, 40);
        let blue = Rgb(40, 90, 240);
        let (upper, lower) = if second == 1 {
            (red, blue)
        } else {
            (blue, red)
        };
        assert!(
            diff(c, upper) < diff(c, lower),
            "crossing pixel {c:?} should show upper {upper:?} over {lower:?}"
        );
        assert!(
            diff(c, upper) < 0.35,
            "upper track dominates the crossing: {c:?}"
        );
    }
}

#[test]
fn dashed_edges_alternate_dash_and_gap() {
    let fx = Fixture::filtered_dashed();
    let (backend, _, _) = rasterize(&fx, GraphicsProtocol::Sixel, CELL, 8);
    let g = *backend.geometry();
    let bg = Rgb::from_color(fx.bg(), Rgb(0, 0, 0));
    let (row, pos) = fx
        .rows
        .iter()
        .enumerate()
        .find_map(|(r, els)| {
            els.iter().find_map(|e| match e.element {
                GraphPrintElement::Edge {
                    style: LineStyle::Dashed,
                    from_pos,
                    to_pos,
                    ..
                } if from_pos == to_pos => Some((r, from_pos)),
                _ => None,
            })
        })
        .expect("a vertical dashed edge");
    let x = g.track_x(pos) as u32;
    let y0 = row as u32 * CELL.height as u32;
    let inks: Vec<f32> = (y0..y0 + CELL.height as u32)
        .map(|y| diff(pixel(&backend, x, y), bg))
        .collect();
    assert!(inks.iter().any(|&i| i > 0.3), "dash present: {inks:?}");
    assert!(inks.iter().any(|&i| i < 0.02), "gap present: {inks:?}");
}

#[test]
fn head_node_paints_three_concentric_rings_at_hidpi() {
    let cell = CellPixelSize {
        width: 16,
        height: 44,
    };
    let fx = Fixture::fork_and_merge();
    let (backend, _, _) = rasterize(&fx, GraphicsProtocol::Kitty, cell, 12);
    let g = *backend.geometry();
    assert_eq!(
        (g.head_outer_diameter, g.head_delta),
        (23.0, 4.0),
        "outer Ø23, ring delta 4 at 44px rows"
    );
    let GraphPrintElement::Node {
        pos,
        kind: NodeKind::Head,
        color_id,
        ..
    } = fx.rows[0].iter().last().unwrap().element
    else {
        panic!("row 0 ends with the HEAD node");
    };
    let track = Rgb::from_color(fx.color_map[&color_id], Rgb(0, 0, 0));
    let bg = Rgb::from_color(fx.bg(), Rgb(0, 0, 0));
    let (x0, y0) = (g.track_x(pos) as u32, g.row_center as u32);
    // Pixel (x0 + d, y0) has its center exactly d px from the circle center.
    assert!(diff(pixel(&backend, x0, y0), track) < 0.05, "inner dot");
    assert!(
        diff(pixel(&backend, x0 + 5, y0), bg) < 0.05,
        "middle ring is row_bg"
    );
    assert!(
        diff(pixel(&backend, x0 + 9, y0), track) < 0.05,
        "outer ring"
    );
    assert!(
        diff(pixel(&backend, x0 + 14, y0), bg) < 0.05,
        "outside the node"
    );
}

#[test]
fn normal_and_head_nodes_are_symmetrically_centered_on_track_axis() {
    let fx = Fixture::manual(vec![
        vec![GraphPrintElement::Node {
            row: 0,
            perm_id: 0,
            pos: 1,
            kind: NodeKind::Head,
            color_id: BranchColorId(1),
        }],
        vec![GraphPrintElement::Node {
            row: 1,
            perm_id: 1,
            pos: 1,
            kind: NodeKind::Normal,
            color_id: BranchColorId(2),
        }],
    ]);
    let (backend, _, _) = rasterize(&fx, GraphicsProtocol::Kitty, CELL, 6);
    let g = *backend.geometry();
    let bg = Rgb::from_color(fx.bg(), Rgb(0, 0, 0));

    for row_idx in [0u32, 1u32] {
        let xc = g.track_x(1) as u32;
        let yc = row_idx * CELL.height as u32 + g.row_center as u32;
        // Both horizontal and vertical symmetry around (xc, yc) prove the circle
        // center aligns with the line axis (not shifted by an extra +0.5px).
        for d in 1..=5 {
            assert_eq!(
                pixel(&backend, xc - d, yc),
                pixel(&backend, xc + d, yc),
                "row {row_idx} horizontal symmetry at d={d}"
            );
            assert_eq!(
                pixel(&backend, xc, yc - d),
                pixel(&backend, xc, yc + d),
                "row {row_idx} vertical symmetry at d={d}"
            );
        }
        assert!(diff(pixel(&backend, xc, yc), bg) > 0.2);
    }
}

#[test]
fn golden_pngs_match_tiny_skia_output_pixel_for_pixel() {
    let cases: [(&str, Fixture, CellPixelSize, u16); 4] = [
        ("fork_and_merge", Fixture::fork_and_merge(), CELL, 12),
        (
            "crossing",
            Fixture::manual(vec![
                vec![
                    edge(0, 0, 2, EdgeDirection::Down, 1),
                    edge(0, 1, 0, EdgeDirection::Down, 2),
                ],
                vec![],
            ]),
            CELL,
            8,
        ),
        ("filtered_dashed", Fixture::filtered_dashed(), CELL, 8),
        (
            "head_hidpi",
            Fixture::fork_and_merge(),
            CellPixelSize {
                width: 16,
                height: 44,
            },
            12,
        ),
    ];

    let golden_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/graph/vector/golden");
    let update = std::env::var_os("JGIT_UPDATE_GOLDENS").is_some();
    if update {
        std::fs::create_dir_all(&golden_dir).unwrap();
    }

    for (name, fx, cell, cols) in cases {
        let (backend, _, _) = rasterize(&fx, GraphicsProtocol::Kitty, cell, cols);
        let (rgba, w, h) = backend.strip();
        let path = golden_dir.join(format!("{name}.png"));
        if update {
            let mut png = Vec::new();
            assert!(encode::encode_png(rgba, w, h, &mut png));
            std::fs::write(&path, &png).unwrap();
        }
        let golden_bytes = std::fs::read(&path).unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(&golden_bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut expected = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut expected).unwrap();
        assert_eq!((w, h), (info.width, info.height), "{name} dimensions");
        assert_eq!(rgba, &expected[..info.buffer_size()], "{name} pixels");
    }
}

#[test]
fn terminal_edges_draw_arrow_wings_and_stop_short() {
    let fx = Fixture::long_edge();
    let (backend, _, _) = rasterize(&fx, GraphicsProtocol::Sixel, CELL, 6);
    let g = *backend.geometry();
    let bg = Rgb::from_color(fx.bg(), Rgb(0, 0, 0));
    let pos = fx.rows[1]
        .iter()
        .find_map(|e| match e.element {
            GraphPrintElement::TerminalEdge {
                pos,
                direction: EdgeDirection::Down,
                ..
            } => Some(pos),
            _ => None,
        })
        .expect("row 1 has a ▾ terminal edge");
    let h = CELL.height as u32;
    let x = g.track_x(pos) as u32;
    let tip = h + (g.row_height - (g.circle_radius / 2.0 + 1.0)) as u32;
    let ink = |x: u32, y: u32| diff(pixel(&backend, x, y), bg);
    assert!(ink(x, tip - 1) > 0.3, "shaft reaches the tip");
    assert!(ink(x, 2 * h - 1) < 0.05, "arrow gap above the next row");
    let wing = |dx: i32| {
        (tip - 6..=tip - 2)
            .flat_map(|y| (1..=4).map(move |d| (y, d)))
            .map(|(y, d)| ink((x as i32 + dx * d) as u32, y))
            .fold(0.0, f32::max)
    };
    assert!(wing(-1) > 0.2 && wing(1) > 0.2, "both arrow wings");
}

#[test]
fn hovered_elements_get_thicker_outlined_strokes() {
    let mut fx = Fixture::manual(vec![
        vec![edge(0, 0, 0, EdgeDirection::Down, 1)],
        vec![edge(1, 0, 0, EdgeDirection::Up, 1)],
    ]);
    let (plain, _, _) = rasterize(&fx, GraphicsProtocol::Sixel, CELL, 4);
    for row in &mut fx.rows {
        row[0].is_selected = true;
    }
    let (hovered, _, _) = rasterize(&fx, GraphicsProtocol::Sixel, CELL, 4);
    let bg = Rgb::from_color(fx.bg(), Rgb(0, 0, 0));
    let y = CELL.height as u32 + 4;
    let width =
        |b: &PixelVectorGraphBackend| (0..36).filter(|&x| diff(pixel(b, x, y), bg) > 0.1).count();
    assert!(width(&hovered) > width(&plain), "selected stroke is wider");
}

// ── Hit testing ─────────────────────────────────────────────────────────────

#[test]
fn node_and_arrow_hits_match_the_character_grid() {
    for fx in [Fixture::fork_and_merge(), Fixture::long_edge()] {
        let (_, _, vector_hits) = rasterize(&fx, GraphicsProtocol::Kitty, CELL, 8);
        let mut grid_hits = HitTestRegistry::new();
        let area = Rect::new(0, 0, 8, fx.rows.len() as u16);
        let mut buf = Buffer::empty(area);
        let slices = fx.slices(|_| fx.bg());
        CellGridGraphBackend::new().render_viewport(
            &GraphViewportSlice {
                rows: &slices,
                area,
                occluding_rects: &[],
                color_map: &fx.color_map,
            },
            &mut buf,
            &mut fx.ctx(&mut grid_hits),
        );
        for (r, row) in fx.rows.iter().enumerate() {
            for el in row {
                let pos = match el.element {
                    GraphPrintElement::Node { pos, .. } => pos,
                    GraphPrintElement::TerminalEdge { pos, .. } => pos,
                    _ => continue,
                };
                let at = |reg: &HitTestRegistry| {
                    reg.hit_test(pos as u16 * 2, r as u16)
                        .map(|h| h.target.clone())
                };
                assert_eq!(at(&vector_hits), at(&grid_hits), "row {r} pos {pos}");
            }
        }
    }
}

#[test]
fn slanted_edge_hits_follow_the_drawn_line() {
    // Row 0: node at track 0, fork edge down to track 2.
    let fx = Fixture::manual(vec![vec![edge(0, 0, 2, EdgeDirection::Down, 1)], vec![]]);
    let (_, _, hits) = rasterize(&fx, GraphicsProtocol::Sixel, CELL, 8);
    let hit_cols: Vec<u16> = (0..8)
        .filter(|&c| {
            matches!(
                hits.hit_test(c, 0).map(|h| &h.target),
                Some(HitTarget::GraphPrintElement {
                    kind: GraphHitKind::Edge { .. },
                    ..
                })
            )
        })
        .collect();
    // Visible half of the segment spans tracks 0 → 1 (columns 0..=2).
    assert_eq!(hit_cols.first(), Some(&0));
    assert!(
        hit_cols.contains(&1) && hit_cols.contains(&2),
        "{hit_cols:?}"
    );
    assert!(!hit_cols.contains(&4), "{hit_cols:?}");
}

#[test]
fn subpixel_hit_test_matches_is_over_node_and_edge() {
    let fx = Fixture::long_edge();
    let area = Rect::new(0, 0, 8, fx.rows.len() as u16);
    let (backend, _, _) = rasterize(&fx, GraphicsProtocol::Kitty, CELL, 8);
    let g = *backend.geometry();

    // Row 0 node at track 0: center hits Node, outside circle misses Node.
    let node_x = g.track_x(0) as u16;
    let node_y = g.row_center as u16;
    assert!(matches!(
        backend.hit_test_pixel(area, node_x, node_y),
        Some(HitTarget::GraphPrintElement {
            kind: GraphHitKind::Node { .. },
            ..
        })
    ));

    // Row 1 has TerminalEdge (▾) at track 1.
    let arrow_pos = fx.rows[1]
        .iter()
        .find_map(|e| match e.element {
            GraphPrintElement::TerminalEdge { pos, .. } => Some(pos),
            _ => None,
        })
        .expect("row 1 terminal arrow");
    let arrow_x = g.track_x(arrow_pos) as u16;
    let arrow_y = CELL.height + g.row_center as u16;
    assert!(matches!(
        backend.hit_test_pixel(area, arrow_x, arrow_y),
        Some(HitTarget::GraphPrintElement {
            kind: GraphHitKind::Arrow { is_down: true, .. },
            ..
        })
    ));

    // Far empty right margin misses completely.
    assert_eq!(backend.hit_test_pixel(area, 70, 5), None);
}

// ── Frame protocol: caching, Kitty placeholders, occlusion ─────────────────

/// One full frame through [`GraphRenderer`]: render, optional overlay, finish.
fn frame(
    renderer: &mut GraphRenderer,
    fx: &Fixture,
    hover_row: Option<usize>,
    overlay: Option<Rect>,
) -> (Buffer, Vec<u8>) {
    let rows = fx.rows.len() as u16;
    let area = Rect::new(2, 1, 8, rows);
    let screen = Rect::new(0, 0, 20, rows + 2);
    let mut buf = Buffer::empty(screen);
    let mut hits = HitTestRegistry::new();
    let mut rows_owned = fx.rows.clone();
    if let Some(r) = hover_row {
        for el in &mut rows_owned[r] {
            el.is_selected = true;
        }
    }
    let slices: Vec<RowGraphSlice<'_>> = rows_owned
        .iter()
        .enumerate()
        .map(|(i, els)| RowGraphSlice {
            visible_row: i as VisibleRowIdx,
            commit_oid: fx.oids[i],
            row_bg: fx.bg(),
            elements: els,
        })
        .collect();
    renderer.begin_frame();
    {
        let mut ctx = fx.ctx(&mut hits);
        renderer.render_viewport(
            &GraphViewportSlice {
                rows: &slices,
                area,
                occluding_rects: &[],
                color_map: &fx.color_map,
            },
            &mut buf,
            &mut ctx,
        );
        if let Some(rect) = overlay {
            Clear.render(rect, &mut buf);
            Block::bordered()
                .style(Style::default().bg(Color::Rgb(60, 63, 65)))
                .render(rect, &mut buf);
        }
        renderer.finish_frame(&mut buf, screen, &ctx);
    }
    let mut out = Vec::new();
    renderer.write_emission(&mut out).unwrap();
    (buf, out)
}

fn caps(
    protocol: Option<GraphicsProtocol>,
    cell_px: Option<CellPixelSize>,
) -> GraphicsCapabilities {
    GraphicsCapabilities { protocol, cell_px }
}

fn grid_frame(fx: &Fixture, overlay: Option<Rect>) -> Buffer {
    let mut grid = GraphRenderer::new(
        GraphRendererMode::CharacterGrid,
        GraphicsCapabilities::default(),
    );
    frame(&mut grid, fx, None, overlay).0
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

#[test]
fn zero_pixel_window_size_degrades_to_identical_character_grid() {
    let fx = Fixture::fork_and_merge();
    // window_size() reporting 0×0 pixels → no cell size → Auto picks the grid.
    let probed = GraphicsCapabilities::from_probe(
        |k| (k == "TERM").then(|| "xterm-kitty".to_string()),
        Some((120, 40, 0, 0)),
    );
    assert_eq!(probed.cell_px, None);
    for caps in [probed, caps(None, Some(CELL))] {
        let mut renderer = GraphRenderer::new(GraphRendererMode::Auto, caps);
        assert_eq!(renderer.resolved(), ResolvedGraphBackend::CharacterGrid);
        let (buf, out) = frame(&mut renderer, &fx, None, None);
        assert!(out.is_empty(), "no escape sequences in grid mode");
        assert_eq!(
            buf,
            grid_frame(&fx, None),
            "layout identical to the plain grid"
        );
    }

    // Hit regions are the plain grid's, region for region.
    let area = Rect::new(0, 0, 8, fx.rows.len() as u16);
    let slices = fx.slices(|_| fx.bg());
    let slice = GraphViewportSlice {
        rows: &slices,
        area,
        occluding_rects: &[],
        color_map: &fx.color_map,
    };
    let mut a = HitTestRegistry::new();
    let mut b = HitTestRegistry::new();
    GraphRenderer::new(GraphRendererMode::Auto, probed).render_viewport(
        &slice,
        &mut Buffer::empty(area),
        &mut fx.ctx(&mut a),
    );
    CellGridGraphBackend::new().render_viewport(
        &slice,
        &mut Buffer::empty(area),
        &mut fx.ctx(&mut b),
    );
    for y in 0..area.height {
        for x in 0..area.width {
            assert_eq!(
                a.hit_test(x, y).map(|h| &h.target),
                b.hit_test(x, y).map(|h| &h.target)
            );
        }
    }
}

#[test]
fn kitty_frames_use_placeholders_transmit_once_and_double_buffer_ids() {
    let fx = Fixture::fork_and_merge();
    let mut renderer = GraphRenderer::new(
        GraphRendererMode::Auto,
        caps(Some(GraphicsProtocol::Kitty), Some(CELL)),
    );
    let (buf, out) = frame(&mut renderer, &fx, None, None);
    let id0 = KITTY_IMAGE_IDS[0];
    assert_eq!(count(&out, b"\x1b_Ga=T,f=100"), 1);
    assert!(
        out.windows(9).any(|w| w == b"U=1,c=8,r"),
        "virtual placement over the area"
    );
    let cell = &buf[(2 + 3, 1 + 2)];
    let mut sym = [0u8; 16];
    assert_eq!(
        cell.symbol(),
        encode::kitty_placeholder_into(&mut sym, 2, 3)
    );
    assert_eq!(cell.fg, kitty_id_color(id0));

    // Identical frame: dirty cache hit, nothing re-sent.
    let (_, out) = frame(&mut renderer, &fx, None, None);
    assert!(out.is_empty());

    // Hover changes pixels: transmitted under the other id, old id deleted.
    let (buf, out) = frame(&mut renderer, &fx, Some(1), None);
    let s = String::from_utf8_lossy(&out);
    assert!(
        s.contains(&format!("i={},U=1", KITTY_IMAGE_IDS[1])),
        "{s:.80}"
    );
    assert!(s.contains(&format!("a=d,d=I,i={id0},q=2")));
    assert_eq!(buf[(2, 1)].fg, kitty_id_color(KITTY_IMAGE_IDS[1]));

    // An overlay simply replaces placeholder cells (they are text).
    let menu = Rect::new(4, 2, 10, 4);
    let (buf, _) = frame(&mut renderer, &fx, Some(1), Some(menu));
    let reference = grid_frame(&fx, Some(menu));
    for y in menu.top()..menu.bottom() {
        for x in menu.left()..menu.right() {
            assert_eq!(buf[(x, y)], reference[(x, y)], "menu cell ({x},{y})");
        }
    }

    let release = renderer.shutdown_sequence();
    assert!(String::from_utf8_lossy(&release).contains(&format!("i={}", KITTY_IMAGE_IDS[1])));
}

#[test]
fn switching_to_character_grid_releases_kitty_image() {
    let fx = Fixture::fork_and_merge();
    let mut renderer = GraphRenderer::new(
        GraphRendererMode::Auto,
        caps(Some(GraphicsProtocol::Kitty), Some(CELL)),
    );
    let (_, out) = frame(&mut renderer, &fx, None, None);
    assert!(count(&out, b"\x1b_Ga=T,f=100") == 1);

    renderer.set_mode(GraphRendererMode::CharacterGrid);
    let (buf, out) = frame(&mut renderer, &fx, None, None);
    let s = String::from_utf8_lossy(&out);
    assert!(
        s.contains(&format!("a=d,d=I,i={},q=2", KITTY_IMAGE_IDS[0])),
        "switching to CharacterGrid must delete stale Kitty image: {s}"
    );
    assert_eq!(buf, grid_frame(&fx, None));
}

#[test]
fn slice_occluding_rects_are_honored_directly_by_vector_backend() {
    let fx = Fixture::fork_and_merge();
    let area = Rect::new(0, 0, 12, fx.rows.len() as u16);
    let menu = Rect::new(2, 1, 6, 3);
    let slices = fx.slices(|_| fx.bg());
    let slice = GraphViewportSlice {
        rows: &slices,
        area,
        occluding_rects: &[menu],
        color_map: &fx.color_map,
    };

    // Sixel falls back to CharacterGrid around `menu`.
    let mut sixel_backend = PixelVectorGraphBackend::new(GraphicsProtocol::Sixel, CELL);
    let mut buf = Buffer::empty(area);
    let mut hits = HitTestRegistry::new();
    assert!(!sixel_backend.try_render(&slice, &mut buf, &mut fx.ctx(&mut hits)));
    sixel_backend.render_viewport(&slice, &mut buf, &mut fx.ctx(&mut hits));
    let mut grid_buf = Buffer::empty(area);
    let mut grid_hits = HitTestRegistry::new();
    CellGridGraphBackend::new().render_viewport(&slice, &mut grid_buf, &mut fx.ctx(&mut grid_hits));
    assert_eq!(buf, grid_buf);

    // Kitty preserves pre-existing content inside `menu`.
    let mut kitty_backend = PixelVectorGraphBackend::new(GraphicsProtocol::Kitty, CELL);
    let mut kitty_buf = Buffer::empty(area);
    kitty_buf[(menu.x, menu.y)].set_symbol("M");
    let mut kitty_hits = HitTestRegistry::new();
    assert!(kitty_backend.try_render(&slice, &mut kitty_buf, &mut fx.ctx(&mut kitty_hits)));
    assert_eq!(kitty_buf[(menu.x, menu.y)].symbol(), "M");
}

#[test]
fn sixel_frame_is_positioned_and_cached_until_something_changes() {
    let fx = Fixture::fork_and_merge();
    let mut renderer = GraphRenderer::new(
        GraphRendererMode::Vector,
        caps(Some(GraphicsProtocol::Sixel), Some(CELL)),
    );
    let (buf, out) = frame(&mut renderer, &fx, None, None);
    assert!(
        out.starts_with(b"\x1b7\x1b[2;3H\x1bP0;1;0q\"1;1;72;144"),
        "{:?}",
        &out[..30]
    );
    assert!(out.ends_with(b"\x1b\\\x1b8"));
    // Cells under the image are blank markers on the row background.
    assert_eq!(buf[(2, 1)].symbol(), " ");
    assert_eq!(buf[(2, 1)].fg, OVERLAY_MARKER_FG);

    assert!(frame(&mut renderer, &fx, None, None).1.is_empty(), "cached");
    assert!(
        !frame(&mut renderer, &fx, Some(2), None).1.is_empty(),
        "hover re-sends"
    );

    // Font zoom: new cell pixel size → strip rebuilt and re-sent at the new size.
    renderer.set_capabilities(caps(
        Some(GraphicsProtocol::Sixel),
        Some(CellPixelSize {
            width: 12,
            height: 24,
        }),
    ));
    let (_, out) = frame(&mut renderer, &fx, Some(2), None);
    assert!(out.starts_with(b"\x1b7\x1b[2;3H\x1bP0;1;0q\"1;1;96;192"));
}

#[test]
fn sixel_and_iterm2_never_paint_over_overlays() {
    let fx = Fixture::fork_and_merge();
    for protocol in [GraphicsProtocol::Sixel, GraphicsProtocol::Iterm2] {
        let mut renderer =
            GraphRenderer::new(GraphRendererMode::Auto, caps(Some(protocol), Some(CELL)));
        assert!(!frame(&mut renderer, &fx, None, None).1.is_empty());

        // A context menu over the graph: no bitmap this frame, and the whole
        // screen is exactly what the character grid renders under the menu.
        let menu = Rect::new(5, 2, 9, 4);
        let (buf, out) = frame(&mut renderer, &fx, None, Some(menu));
        assert!(out.is_empty(), "{protocol:?}: image would cover the menu");
        assert_eq!(buf, grid_frame(&fx, Some(menu)), "{protocol:?}");

        // Menu closed: the image comes back immediately.
        let (_, out) = frame(&mut renderer, &fx, None, None);
        let marker: &[u8] = match protocol {
            GraphicsProtocol::Sixel => b"\x1bP",
            _ => b"\x1b]1337;File=inline=1",
        };
        assert_eq!(count(&out, marker), 1, "{protocol:?}");
    }
}

#[test]
fn oversized_viewports_fall_back_to_the_grid() {
    let fx = Fixture::long_edge();
    let huge = CellPixelSize {
        width: 256,
        height: 512,
    };
    let mut renderer = GraphRenderer::new(
        GraphRendererMode::Auto,
        caps(Some(GraphicsProtocol::Sixel), Some(huge)),
    );
    let (buf, out) = frame(&mut renderer, &fx, None, None);
    assert!(out.is_empty());
    assert_eq!(buf, grid_frame(&fx, None));
}

#[test]
fn switching_tabs_away_forces_a_resend() {
    let fx = Fixture::fork_and_merge();
    let mut renderer = GraphRenderer::new(
        GraphRendererMode::Auto,
        caps(Some(GraphicsProtocol::Iterm2), Some(CELL)),
    );
    assert!(!frame(&mut renderer, &fx, None, None).1.is_empty());
    // A frame without the graph (another tab) overwrote the image cells.
    renderer.begin_frame();
    let fx2 = Fixture::manual(vec![]);
    let mut hits = HitTestRegistry::new();
    renderer.finish_frame(
        &mut Buffer::empty(Rect::new(0, 0, 20, 10)),
        Rect::new(0, 0, 20, 10),
        &fx2.ctx(&mut hits),
    );
    assert!(!frame(&mut renderer, &fx, None, None).1.is_empty());
}

/// `cargo test --release -p jgit-tui hidpi_strip_budget -- --ignored --nocapture`
#[test]
#[ignore = "timing probe; run in release"]
fn hidpi_strip_budget() {
    let fx = Fixture::long_edge();
    let cell = CellPixelSize {
        width: 18,
        height: 44,
    };
    let mut renderer = GraphRenderer::new(
        GraphRendererMode::Auto,
        caps(Some(GraphicsProtocol::Sixel), Some(cell)),
    );
    let _ = frame(&mut renderer, &fx, None, None);
    let t = std::time::Instant::now();
    let n = 20;
    for i in 0..n {
        let _ = frame(&mut renderer, &fx, Some(i % 30), None);
    }
    eprintln!(
        "{} rows x 8 cols @ {}x{}px: {:?} per raster+encode frame",
        fx.rows.len(),
        cell.width,
        cell.height,
        t.elapsed() / n as u32
    );
}

// ── App-level: real menus / popups over the vector graph ───────────────────

mod app_level {
    use std::sync::Arc;

    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use jgit_core::{CommitSummary, RepoSnapshot};
    use jgit_engine::EngineEvent;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::app::{Action, AppState, ContextMenuEntryKind, ContextMenuItem, ModalLayer};
    use crate::input::{presentation_menu_items, route_terminal_event};
    use crate::views::render_app;

    fn summary(n: u64, parents: &[u64]) -> CommitSummary {
        CommitSummary {
            oid: oid(n),
            parents: parents.iter().copied().map(oid).collect(),
            author_name: "Alice".into(),
            author_email: "alice@x.io".into(),
            timestamp: 1000 - n as i64,
            author_timestamp: 1000 - n as i64,
            subject: format!("commit {n}"),
            body: String::new(),
        }
    }

    fn state() -> AppState {
        let commits = vec![
            summary(0, &[1]),
            summary(1, &[6, 2]),
            summary(2, &[3]),
            summary(3, &[5, 4]),
            summary(4, &[5]),
            summary(5, &[6]),
            summary(6, &[7]),
            summary(7, &[]),
        ];
        let snap = RepoSnapshot {
            repo_name: "demo".into(),
            head_branch: Some("main".into()),
            head_oid: Some(oid(0)),
            refs: vec![branch_ref("main", 0, true), branch_ref("feat/ui", 2, false)],
            commits,
            ..Default::default()
        };
        let mut state = AppState::new(".".into(), None);
        state.apply_engine_event(EngineEvent::RepoSnapshotLoaded {
            snapshot: Arc::new(snap),
            epoch: 1,
        });
        state
    }

    fn send(state: &mut AppState, ev: Event) {
        for a in route_terminal_event(ev, state) {
            state.dispatch(a);
        }
    }

    fn key(state: &mut AppState, code: KeyCode, mods: KeyModifiers) {
        send(state, Event::Key(KeyEvent::new(code, mods)));
    }

    /// Renders one frame; returns the screen and the bytes sent after the flush.
    fn draw(state: &mut AppState, w: u16, h: u16) -> (Buffer, Vec<u8>) {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render_app(f, state)).unwrap();
        let mut out = Vec::new();
        state.graph_renderer.write_emission(&mut out).unwrap();
        (terminal.backend().buffer().clone(), out)
    }

    /// Same state, rendered with the character grid (the fallback reference).
    fn draw_grid(state: &mut AppState, w: u16, h: u16) -> Buffer {
        let mode = state.graph_renderer.mode();
        state
            .graph_renderer
            .set_mode(GraphRendererMode::CharacterGrid);
        let (buf, _) = draw(state, w, h);
        state.graph_renderer.set_mode(mode);
        buf
    }

    fn graph_cells(state: &AppState) -> Vec<(u16, u16)> {
        let mut cells = Vec::new();
        for y in 0..state.terminal_area.height {
            for x in 0..state.terminal_area.width {
                if matches!(
                    state.hit_registry.hit_test(x, y).map(|r| &r.target),
                    Some(HitTarget::GraphPrintElement { .. })
                ) {
                    cells.push((x, y));
                }
            }
        }
        cells
    }

    #[test]
    fn context_menu_and_branches_popup_are_never_covered_by_sixel_tracks() {
        let (w, h) = (90u16, 24u16);
        let mut state = state();
        state.graph_renderer = GraphRenderer::new(
            GraphRendererMode::Auto,
            caps(Some(GraphicsProtocol::Sixel), Some(CELL)),
        );
        let (_, out) = draw(&mut state, w, h);
        assert_eq!(count(&out, b"\x1bP"), 1, "vector graph is on screen");
        let graph = graph_cells(&state);
        let &(nx, ny) = graph.first().expect("graph hit cells");

        // Right-click a graph node: the menu opens on top of the tracks.
        send(
            &mut state,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: nx,
                row: ny,
                modifiers: KeyModifiers::NONE,
            }),
        );
        assert!(matches!(
            state.modal_stack.last(),
            Some(ModalLayer::ContextMenu { .. })
        ));
        let (buf, out) = draw(&mut state, w, h);
        assert!(out.is_empty(), "no bitmap may be drawn over the open menu");
        assert_eq!(buf, draw_grid(&mut state, w, h), "menu frame == grid frame");

        // Closing the menu brings the image straight back.
        key(&mut state, KeyCode::Esc, KeyModifiers::NONE);
        assert!(state.modal_stack.is_empty());
        let (_, out) = draw(&mut state, w, h);
        assert_eq!(count(&out, b"\x1bP"), 1);

        // BranchesPopup (Ctrl+backtick) over the graph column.
        key(&mut state, KeyCode::Char('`'), KeyModifiers::CONTROL);
        assert!(!state.modal_stack.is_empty());
        let (buf, out) = draw(&mut state, w, h);
        let reference = draw_grid(&mut state, w, h);
        let covered = graph
            .iter()
            .any(|&(x, y)| buf[(x, y)].fg != OVERLAY_MARKER_FG);
        assert!(covered, "fixture: the popup overlaps the graph column");
        assert!(out.is_empty(), "popup overlaps the graph: no bitmap");
        assert_eq!(buf, reference);
    }

    fn renderer_submenu(items: &[ContextMenuItem]) -> &[ContextMenuItem] {
        items
            .iter()
            .find_map(|i| match &i.kind {
                ContextMenuEntryKind::Submenu(sub) if i.label.starts_with("Graph Renderer") => {
                    Some(sub.as_slice())
                }
                _ => None,
            })
            .expect("Graph Renderer submenu in [⚙ Presentation]")
    }

    #[test]
    fn presentation_menu_and_f8_switch_the_graph_renderer_live() {
        let mut state = state();
        let items = presentation_menu_items(&state);
        let labels: Vec<&str> = renderer_submenu(&items)
            .iter()
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(
            labels,
            vec!["● Auto", "○ Vector (Kitty/Sixel)", "○ Character Grid"]
        );

        state.dispatch(Action::SetGraphRenderer(GraphRendererMode::CharacterGrid));
        assert_eq!(
            state.graph_renderer.mode(),
            GraphRendererMode::CharacterGrid
        );

        key(&mut state, KeyCode::F(8), KeyModifiers::NONE);
        assert_eq!(state.graph_renderer.mode(), GraphRendererMode::Auto);
        key(&mut state, KeyCode::F(8), KeyModifiers::NONE);
        assert_eq!(state.graph_renderer.mode(), GraphRendererMode::Vector);

        // Forced vector without a usable terminal stays on the grid, and says why.
        state
            .graph_renderer
            .set_capabilities(GraphicsCapabilities::default());
        assert_eq!(
            state.graph_renderer.resolved(),
            ResolvedGraphBackend::CharacterGrid
        );
        assert!(
            state
                .graph_renderer
                .status_label()
                .contains("no graphics protocol detected")
        );
        let (_, out) = draw(&mut state, 90, 24);
        assert!(out.is_empty());
    }
}
