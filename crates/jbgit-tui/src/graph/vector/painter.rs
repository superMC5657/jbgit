//! Pixel-exact port of IntelliJ's `SimpleGraphCellPainter` + `HeadNodePainter`
//! (`reference/platform/vcs-log/impl/src/com/intellij/vcs/log/paint/`) on top of `tiny-skia`.
//!
//! Every row is painted in **row-local** coordinates into a `row_height`-tall
//! slice of the viewport strip, so the slice bounds act as the per-row clip of
//! IntelliJ's `JTable` cell renderer. Non-vertical edges are drawn twice their
//! visible length (`(x1, rowCenter) → (x2, rowCenter ± rowHeight)`) exactly like
//! the reference so neighbouring rows dock with identical slope and identical
//! anti-aliased coverage at every `y = k * row_height` boundary.

use crate::graph::RenderGraphElement;
use jbgit_core::graph::{EdgeDirection, GraphPrintElement, LineStyle, NodeKind};
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, PixmapMut, Stroke, Transform};

/// `PaintParameters.ROW_HEIGHT`: the reference row height all sizes scale from.
pub const REFERENCE_ROW_HEIGHT: f32 = 22.0;
const CIRCLE_RADIUS: f32 = 4.0;
const THICK_LINE: f32 = 1.5;
const SELECT_THICK_LINE: f32 = 2.5;
/// `HeadNodePainter.RADIUS_DELTA`.
const HEAD_RADIUS_DELTA: f32 = 2.0;
const ARROW_ANGLE_COS2: f32 = 0.7;
const ARROW_LENGTH: f32 = 0.3;

/// `PaintParameters.scaleWithRowHeight`.
#[inline]
pub fn scale_with_row_height(value: f32, row_height: f32) -> f32 {
    value * row_height / REFERENCE_ROW_HEIGHT
}

/// `PaintUtil.alignToInt(v, ctx, FLOOR, ODD)` at device scale 1: the largest odd
/// integer `<= v` (never below 1).
#[inline]
fn align_floor_odd(v: f32) -> f32 {
    let f = v.floor().max(1.0);
    if (f as i64) % 2 == 0 { f - 1.0 } else { f }
}

/// Row-local pixel coordinate.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Pt(f32, f32);

/// Stroke attributes for [`RowPainter::paint_line`].
#[derive(Debug, Clone, Copy)]
struct LinePaint {
    is_solid: bool,
    selected: bool,
    color: Rgb,
}

/// Target surface + geometry shared across row-painting helpers.
struct PaintCanvas<'a, 'b> {
    pixmap: &'a mut PixmapMut<'b>,
    geom: &'a PaintGeometry,
}

/// Pre-computed per-row-height geometry (`SimpleGraphCellPainter.MyPainter` fields).
///
/// All positions are integer device pixels like the Java2D reference; painting
/// shifts lines by `+0.5` so odd-width strokes land on pixel centers, while
/// circles are centered at `(xc - floor(d/2)) + d/2 = xc + 0.5` for odd `d`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaintGeometry {
    pub row_height: f32,
    pub cell_width: f32,
    /// One graph track = two terminal columns, so the image lines up with the
    /// character-grid layout (`graph_cols = tracks * 2`) and its hit columns.
    pub element_width: f32,
    pub element_center: f32,
    pub row_center: f32,
    pub line_thickness: f32,
    pub selected_line_thickness: f32,
    pub circle_diameter: f32,
    pub selected_circle_diameter: f32,
    pub circle_radius: f32,
    pub selected_circle_radius: f32,
    pub head_outer_diameter: f32,
    pub head_outer_radius: f32,
    pub head_selected_outer_diameter: f32,
    pub head_selected_outer_radius: f32,
    pub head_delta: f32,
}

impl PaintGeometry {
    pub fn new(cell_width: u16, row_height: u16) -> Self {
        let h = f32::from(row_height.max(1));
        let cw = f32::from(cell_width.max(1));
        let element_width = 2.0 * cw;
        let element_center = (cw / 2.0).floor();
        let row_center = align_floor_odd(h / 2.0);

        let line_thickness = align_floor_odd(scale_with_row_height(THICK_LINE, h)).max(1.0);
        let selected_line_thickness =
            align_floor_odd(scale_with_row_height(SELECT_THICK_LINE, h)).max(line_thickness + 2.0);

        let circle_diameter =
            align_floor_odd(2.0 * scale_with_row_height(CIRCLE_RADIUS, h)).max(3.0);
        let selected_circle_diameter = circle_diameter + selected_line_thickness - line_thickness;
        let circle_radius = (circle_diameter / 2.0).floor();
        let selected_circle_radius = (selected_circle_diameter / 2.0).floor();

        let head_outer_diameter =
            align_floor_odd(2.0 * scale_with_row_height(CIRCLE_RADIUS + HEAD_RADIUS_DELTA, h))
                .max(5.0);
        let head_outer_radius = (head_outer_diameter / 2.0).floor();
        let head_selected_outer_diameter =
            head_outer_diameter + selected_line_thickness - line_thickness;
        let head_selected_outer_radius = (head_selected_outer_diameter / 2.0).floor();
        let head_delta = scale_with_row_height(HEAD_RADIUS_DELTA, h).floor().max(1.0);

        Self {
            row_height: h,
            cell_width: cw,
            element_width,
            element_center,
            row_center,
            line_thickness,
            selected_line_thickness,
            circle_diameter,
            selected_circle_diameter,
            circle_radius,
            selected_circle_radius,
            head_outer_diameter,
            head_outer_radius,
            head_selected_outer_diameter,
            head_selected_outer_radius,
            head_delta,
        }
    }

    /// Integer x of the track center for `pos` (row-local == strip-global x).
    #[inline]
    pub fn track_x(&self, pos: usize) -> f32 {
        self.element_width * pos as f32 + self.element_center
    }

    /// Double-length endpoint in the adjacent row (`y = row_center ± row_height`).
    #[inline]
    fn adjacent_row_end(&self, to: usize, down: bool) -> Pt {
        let y2 = if down {
            self.row_height + self.row_center
        } else {
            self.row_center - self.row_height
        };
        Pt(self.track_x(to), y2)
    }

    /// Row-local end point of an edge (`paintEdge`): `(x2, y2)` for the
    /// double-length slanted segment or the half-row vertical stub.
    fn edge_end(&self, from: usize, to: usize, down: bool, terminal: bool) -> Pt {
        if from == to {
            let arrow_gap = if terminal {
                self.circle_radius / 2.0 + 1.0
            } else {
                0.0
            };
            let y2 = if down {
                self.row_height - arrow_gap
            } else {
                arrow_gap
            };
            Pt(self.track_x(from), y2)
        } else {
            self.adjacent_row_end(to, down)
        }
    }

    /// `MyPainter.isOverNode` (row-local integer pixel coordinates).
    pub fn is_over_node(&self, pos: usize, x: f32, y: f32) -> bool {
        let x0 = self.track_x(pos);
        let y0 = self.row_center;
        (x0 - x).hypot(y0 - y) <= self.circle_radius
    }

    /// `MyPainter.isOverEdge`: the point lies inside the thin ellipse whose foci
    /// are the edge's double-length end points.
    pub fn is_over_edge(&self, from: usize, to: usize, down: bool, x: f32, y: f32) -> bool {
        let x1 = self.track_x(from);
        let y1 = self.row_center;
        let Pt(x2, y2) = self.adjacent_row_end(to, down);
        (x1 - x).hypot(y1 - y) + (x2 - x).hypot(y2 - y)
            < (x1 - x2).hypot(y1 - y2) + self.line_thickness
    }

    /// Horizontal pixel extent `[min_x, max_x]` an edge actually covers inside its
    /// own row (the visible half of the double-length segment, plus stroke width).
    pub fn edge_row_extent(
        &self,
        from: usize,
        to: usize,
        down: bool,
        terminal: bool,
    ) -> (f32, f32) {
        let x1 = self.track_x(from);
        let Pt(x2, y2) = self.edge_end(from, to, down, terminal);
        let (xa, xb) = if from == to {
            (x1, x1)
        } else {
            // Clip the double-length segment at the row boundary (y = 0 or h).
            let y_edge = if down { self.row_height } else { 0.0 };
            let t = (y_edge - self.row_center) / (y2 - self.row_center);
            (x1, x1 + (x2 - x1) * t)
        };
        let half = self.line_thickness / 2.0;
        (xa.min(xb) - half, xa.max(xb) + half)
    }
}

/// `SimpleGraphCellPainter.getDashLength`.
pub fn dash_lengths(edge_length: f32, row_height: f32) -> (f32, f32) {
    let dash_count = ((edge_length / row_height).floor() as i32).max(1) as f32;
    let space = (row_height / 2.0 - 2.0).max(1.0);
    let dash = (edge_length / dash_count - space).max(1.0);
    (dash, space)
}

/// `SimpleGraphCellPainter.rotate`.
fn rotate(p: Pt, center: Pt, cos: f32, sin: f32, len: f32) -> Pt {
    let tx = p.0 - center.0;
    let ty = p.1 - center.1;
    let d = tx.hypot(ty).max(f32::EPSILON);
    let sx = len * tx / d;
    let sy = len * ty / d;
    Pt(sx * cos - sy * sin + center.0, sx * sin + sy * cos + center.1)
}

/// Appends dash sub-segments along `start -> end` matching `BasicStroke` with
/// `dash_phase = dash / 2.0`, without allocating a `StrokeDash` `Vec`.
fn push_dashed_segments(
    pb: &mut PathBuilder,
    start: Pt,
    end: Pt,
    geom_len: f32,
    dash: f32,
    space: f32,
) {
    if geom_len <= f32::EPSILON {
        return;
    }
    let ux = (end.0 - start.0) / geom_len;
    let uy = (end.1 - start.1) / geom_len;
    let mut emit = |a: f32, b: f32| {
        let a = a.max(0.0);
        let b = b.min(geom_len);
        if b > a {
            pb.move_to(start.0 + ux * a + 0.5, start.1 + uy * a + 0.5);
            pb.line_to(start.0 + ux * b + 0.5, start.1 + uy * b + 0.5);
        }
    };
    let half_dash = dash / 2.0;
    emit(0.0, half_dash);
    let mut pos = half_dash + space;
    let step = (dash + space).max(1.0);
    while pos < geom_len {
        emit(pos, pos + dash);
        pos += step;
    }
}

/// Opaque 8-bit RGB paint color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn to_skia(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.0, self.1, self.2, 255)
    }
}

/// Re-usable `tiny-skia` painter (keeps the path builder allocation alive).
pub struct RowPainter {
    builder: Option<PathBuilder>,
}

impl Default for RowPainter {
    fn default() -> Self {
        Self::new()
    }
}

impl RowPainter {
    pub fn new() -> Self {
        Self {
            builder: Some(PathBuilder::with_capacity(8, 8)),
        }
    }

    /// `SimpleGraphCellPainter.paint`: unselected elements first, then every
    /// selected element twice — a thick `hover_outline` underlay, then its real
    /// color on top — so hovered tracks get a contrasting outline.
    pub fn paint_row(
        &mut self,
        pixmap: &mut PixmapMut<'_>,
        geom: &PaintGeometry,
        elements: &[RenderGraphElement],
        colors: &[Rgb],
        row_bg: Rgb,
        hover_outline: Rgb,
    ) {
        let mut canvas = PaintCanvas { pixmap, geom };
        for (el, &color) in elements.iter().zip(colors).filter(|(e, _)| !e.is_selected) {
            self.paint_element(&mut canvas, &el.element, color, false, row_bg);
        }
        for el in elements.iter().filter(|e| e.is_selected) {
            self.paint_element(&mut canvas, &el.element, hover_outline, true, row_bg);
        }
        for (el, &color) in elements.iter().zip(colors).filter(|(e, _)| e.is_selected) {
            self.paint_element(&mut canvas, &el.element, color, false, row_bg);
        }
    }

    fn paint_element(
        &mut self,
        canvas: &mut PaintCanvas<'_, '_>,
        element: &GraphPrintElement,
        color: Rgb,
        selected: bool,
        row_bg: Rgb,
    ) {
        let geom = canvas.geom;
        match *element {
            GraphPrintElement::Edge {
                from_pos,
                to_pos,
                direction,
                style,
                ..
            } => {
                let down = direction == EdgeDirection::Down;
                let start = Pt(geom.track_x(from_pos), geom.row_center);
                let end = geom.edge_end(from_pos, to_pos, down, false);
                self.paint_line(
                    canvas,
                    start,
                    end,
                    None,
                    LinePaint {
                        is_solid: style == LineStyle::Solid,
                        selected,
                        color,
                    },
                );
            }
            GraphPrintElement::TerminalEdge { pos, direction, .. } => {
                let down = direction == EdgeDirection::Down;
                let start = Pt(geom.track_x(pos), geom.row_center);
                let end = geom.edge_end(pos, pos, down, true);
                self.paint_line(
                    canvas,
                    start,
                    end,
                    Some(end),
                    LinePaint {
                        is_solid: true,
                        selected,
                        color,
                    },
                );
            }
            GraphPrintElement::ShallowParent { pos, .. } => {
                // Dangling dashed stub toward the unloaded parent below.
                let x = geom.track_x(pos);
                self.paint_line(
                    canvas,
                    Pt(x, geom.row_center),
                    Pt(x, geom.row_height),
                    None,
                    LinePaint {
                        is_solid: false,
                        selected,
                        color,
                    },
                );
            }
            GraphPrintElement::Node { pos, kind, .. } => {
                let center = Pt(geom.track_x(pos), geom.row_center);
                match kind {
                    NodeKind::Head => {
                        self.paint_head_node(canvas, center, selected, color, row_bg);
                    }
                    NodeKind::Normal | NodeKind::Merge => {
                        let (r, d) = if selected {
                            (geom.selected_circle_radius, geom.selected_circle_diameter)
                        } else {
                            (geom.circle_radius, geom.circle_diameter)
                        };
                        self.fill_circle(canvas.pixmap, center.0 - r, center.1 - r, d, color);
                    }
                }
            }
        }
    }

    /// `HeadNodePainter.paint`: outer ring in track color, middle disc punched
    /// out with the row background, inner dot in track color.
    fn paint_head_node(
        &mut self,
        canvas: &mut PaintCanvas<'_, '_>,
        center: Pt,
        selected: bool,
        color: Rgb,
        row_bg: Rgb,
    ) {
        let geom = canvas.geom;
        if selected {
            let r = geom.head_selected_outer_radius;
            self.fill_circle(
                canvas.pixmap,
                center.0 - r,
                center.1 - r,
                geom.head_selected_outer_diameter,
                color,
            );
            return;
        }
        let mut x = center.0 - geom.head_outer_radius;
        let mut y = center.1 - geom.head_outer_radius;
        let mut d = geom.head_outer_diameter;
        self.fill_circle(canvas.pixmap, x, y, d, color);
        let delta = geom.head_delta;
        d -= 2.0 * delta;
        x += delta;
        y += delta;
        self.fill_circle(canvas.pixmap, x, y, d, row_bg);
        d -= 2.0 * delta;
        x += delta;
        y += delta;
        if d > 0.0 {
            self.fill_circle(canvas.pixmap, x, y, d, color);
        }
    }

    /// `Ellipse2D.Double(x, y, d, d)` filled (`x` is `xc - floor(d/2)`, so for
    /// odd `d` `x + d/2` lands directly on the pixel center `xc + 0.5`).
    fn fill_circle(&mut self, pixmap: &mut PixmapMut<'_>, x: f32, y: f32, d: f32, color: Rgb) {
        if d <= 0.0 {
            return;
        }
        let mut pb = self.builder.take().unwrap_or_default();
        let r = d / 2.0;
        pb.push_circle(x + r, y + r, r);
        let Some(path) = pb.finish() else {
            self.builder = Some(PathBuilder::new());
            return;
        };
        let paint = solid_paint(color);
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
        self.builder = Some(path.clear());
    }

    /// `MyPainter.paintLine`.
    fn paint_line(
        &mut self,
        canvas: &mut PaintCanvas<'_, '_>,
        start: Pt,
        end: Pt,
        arrow_tip: Option<Pt>,
        style: LinePaint,
    ) {
        let geom = canvas.geom;
        let width = if style.selected {
            geom.selected_line_thickness
        } else {
            geom.line_thickness
        };
        let stroke = Stroke {
            width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };

        let mut pb = self.builder.take().unwrap_or_default();
        if style.is_solid || arrow_tip.is_some() {
            pb.move_to(start.0 + 0.5, start.1 + 0.5);
            pb.line_to(end.0 + 0.5, end.1 + 0.5);
        } else {
            let geom_len = (start.0 - end.0).hypot(start.1 - end.1);
            let edge_len = if start.0 == end.0 {
                geom.row_height
            } else {
                geom_len
            };
            let (dash, space) = dash_lengths(edge_len, geom.row_height);
            push_dashed_segments(&mut pb, start, end, geom_len, dash, space);
        }
        if let Some(tip) = arrow_tip {
            let cos = ARROW_ANGLE_COS2.sqrt();
            let sin = (1.0 - ARROW_ANGLE_COS2).sqrt();
            let len = ARROW_LENGTH * geom.row_height;
            let w1 = rotate(start, tip, cos, sin, len);
            let w2 = rotate(start, tip, cos, -sin, len);
            pb.move_to(tip.0 + 0.5, tip.1 + 0.5);
            pb.line_to(w1.0 + 0.5, w1.1 + 0.5);
            pb.move_to(tip.0 + 0.5, tip.1 + 0.5);
            pb.line_to(w2.0 + 0.5, w2.1 + 0.5);
        }
        let Some(path) = pb.finish() else {
            self.builder = Some(PathBuilder::new());
            return;
        };
        let paint = solid_paint(style.color);
        canvas
            .pixmap
            .stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        self.builder = Some(path.clear());
    }
}

fn solid_paint(color: Rgb) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color(color.to_skia());
    paint.anti_alias = true;
    paint
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_scales_with_row_height_like_paint_parameters() {
        // Reference 22px row: 1.5 → 1, 2.5 → max(1, 1 + 2) = 3, radius 4 → Ø7.
        let g = PaintGeometry::new(8, 22);
        assert_eq!(g.line_thickness, 1.0);
        assert_eq!(g.selected_line_thickness, 3.0);
        assert_eq!(g.circle_diameter, 7.0);
        assert_eq!(g.circle_radius, 3.0);
        assert_eq!(g.row_center, 11.0);
        assert_eq!(g.head_outer_diameter, 11.0);
        assert_eq!(g.head_delta, 2.0);

        // HiDPI 200 %: 44px rows → 3px lines, 5px selected, Ø15 dots.
        let g = PaintGeometry::new(16, 44);
        assert_eq!(g.line_thickness, 3.0);
        assert_eq!(g.selected_line_thickness, 5.0);
        assert_eq!(g.circle_diameter, 15.0);
        assert_eq!(g.element_width, 32.0);
    }

    #[test]
    fn dash_length_fits_whole_number_of_dashes() {
        let (dash, space) = dash_lengths(22.0, 22.0);
        assert_eq!(space, 9.0);
        assert!((dash + space - 22.0).abs() < 1e-4);
        let (dash, space) = dash_lengths(50.0, 22.0);
        assert!(((dash + space) * 2.0 - 50.0).abs() < 1e-4);
    }

    #[test]
    fn hit_test_formulas_match_reference() {
        let g = PaintGeometry::new(8, 22);
        let x0 = g.track_x(1);
        assert!(g.is_over_node(1, x0, g.row_center));
        assert!(g.is_over_node(1, x0 + g.circle_radius, g.row_center));
        assert!(!g.is_over_node(1, x0 + g.circle_radius + 1.0, g.row_center));
        // Slanted edge from track 0 down to track 1: its midpoint is on the row boundary.
        let mid_x = (g.track_x(0) + g.track_x(1)) / 2.0;
        assert!(g.is_over_edge(0, 1, true, mid_x, g.row_height));
        assert!(!g.is_over_edge(0, 1, true, mid_x, 0.0));
        let (lo, hi) = g.edge_row_extent(0, 1, true, false);
        assert!(lo < g.track_x(0) && hi > mid_x - 1.0 && hi < g.track_x(1));
    }
}
