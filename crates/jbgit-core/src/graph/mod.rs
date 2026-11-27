pub mod collapse;
pub mod edges_in_row;
pub mod joiner;
pub mod linear_bek;
pub mod permanent;
pub mod print_elements;
pub mod visible;

pub use collapse::{
    CollapseInput, CollapseMode, CollapseState, LinearSegment, LinearSegments, MIN_SEGMENT_LEN,
    SegmentId,
};
pub use edges_in_row::{BLOCK_SIZE, CACHE_SIZE, EdgesInRowCache, EdgesInRowGenerator, WALK_SIZE};
pub use fixedbitset::FixedBitSet;
pub use joiner::{
    JoinCommit, JoinError, JoinResult, join_commits, join_commits_indexed,
    join_commits_indexed_if_changed,
};
pub use linear_bek::{
    FoldedBekMerge, LINEAR_BEK_MAGIC_SET_SIZE, LINEAR_BEK_MAX_BLOCK_SIZE, LinearBekState,
    apply_linear_bek,
};
pub use permanent::{
    BranchLayoutPriority, CommitMetaLite, DownTarget, GraphLayoutBuilder, LayoutIndex, PermNodeId,
    PermanentGraph, PermanentGraphLayout, PermanentLinearGraph, PermanentLinearGraphBuilder,
    RawCommitNode, ReachableNodes, VisibleRowIdx, compare_refs_for_layout, ref_layout_priority,
    sort_head_nodes,
};
pub use print_elements::{
    EdgeDirection, GraphElement, GraphPrintElement, LONG_EDGE_SIZE, LineStyle, NodeKind,
    PrintElementGenerator, PrintElementPresentationManager, RECOMMENDED_WIDTH_K,
    RECOMMENDED_WIDTH_SAMPLE_SIZE, VERY_LONG_EDGE_PART_SIZE, VERY_LONG_EDGE_SIZE,
    VISIBLE_PART_SIZE, compare_edge_and_node, compare_elements, edge_is_visible_in_gap,
};
pub use visible::{
    GraphEdge, GraphEdgeType, GraphViewOptions, SortType, VisibleGraph, VisibleGraphBuildArgs,
    VisibleLinearGraph,
};
