pub mod cell_grid;
pub mod renderer;
pub mod state;
#[cfg(feature = "graphics-protocol")]
pub mod vector;

use jbgit_core::graph::{GraphPrintElement, VisibleRowIdx};
use jbgit_core::{BranchColorId, Oid};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use rustc_hash::FxHashMap;

use crate::app::RenderContext;

pub use cell_grid::CellGridGraphBackend;
pub use renderer::{GraphRenderer, GraphRendererMode, GraphicsCapabilities};
pub use state::LogGraphState;

/// One renderable graph primitive in a visible row, paired with resolved metadata.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderGraphElement {
    pub element: GraphPrintElement,
    pub target_oid: Option<Oid>,
    pub is_selected: bool,
}

/// All graph elements and row background for a single visible terminal row.
pub struct RowGraphSlice<'a> {
    pub visible_row: VisibleRowIdx,
    pub commit_oid: Oid,
    pub row_bg: Color,
    pub elements: &'a [RenderGraphElement],
}

/// Viewport-sliced graph rendering payload passed to [`GraphRenderBackend`].
pub struct GraphViewportSlice<'a> {
    pub rows: &'a [RowGraphSlice<'a>],
    pub area: Rect,
    pub occluding_rects: &'a [Rect],
    pub color_map: &'a FxHashMap<BranchColorId, Color>,
}

/// Pluggable DAG track rendering backend (default: [`CellGridGraphBackend`];
/// optional high-DPI vector extension: `vector::PixelVectorGraphBackend`, selected
/// per frame by [`GraphRenderer`]).
pub trait GraphRenderBackend {
    fn render_viewport(
        &mut self,
        slice: &GraphViewportSlice<'_>,
        buf: &mut Buffer,
        ctx: &mut RenderContext<'_>,
    );
}
