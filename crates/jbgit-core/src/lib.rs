pub mod branch_tree;
pub mod conflict;
pub mod console;
pub mod details;
pub mod epoch;
pub mod filter;
pub mod git_ops;
pub mod graph;
pub mod lru;
pub mod oid;
pub mod refs;
pub mod repo;
pub mod stage;
pub mod stage_tracker;
pub mod ui;

pub use branch_tree::{
    BranchCategory, BranchNodeKind, BranchTreeExpansion, BranchTreeNode, BranchTreeOptions,
    BranchTreeRow, BranchesPanelSettings, BranchesTreeModel, FavoriteBranches, fuzzy_match,
    natural_cmp,
};
pub use conflict::{ConflictBlock, ConflictChoice, ConflictDocument, ConflictPaneRow, ConflictSegment};
pub use console::{
    ConsoleLine, ConsoleLineSeverity, ConsoleStream, GitConsoleEntry, classify_console_line,
};
pub use details::{
    ChangeRowKind, ChangeTreeRow, CommitChangesBundle, CommitDetails, DetailsKey, DetailsMode,
    DiffContext, DiffHunk, DiffLine, DiffLineKind, EAGER_WORD_DIFF_PAIR_BUDGET, FileChange,
    FileDiff, FileDiffKey, FileHistoryBundle, FileHistoryEntry, FileHistoryKey,
    MAX_DIFF_FILE_BYTES, MAX_WORD_DIFF_LINE_BYTES, MessageLineKind, PersonStamp, SideBySideRow,
    SignatureStatus, apply_word_diff, apply_word_diff_with_budget, build_change_rows,
    classify_message_line, issue_ref_ranges, side_by_side_rows, word_diff,
};
pub use epoch::{EpochToken, EpochTracker, RequestEpoch};
pub use filter::{
    BranchFilter, DateFilter, FilterError, LogFilterSpec, PathFilter, RepoUser, TextFilter,
    TextMatcher, UserFilter, glob_match,
};
pub use git_ops::{
    GitFailure, GitOp, GitOpOutcome, GitStep, OngoingKind, OngoingStep, RefNameError, ResetMode,
    RewritePlan, StashEntry, TodoAction, TodoEntry, classify_failure, default_squash_messages,
    fuzzy_indices, is_protected_branch, parse_recent_checkouts, render_todo, short_rev,
    validate_ref_name, validate_ref_name_typed,
};
pub use lru::LruCache;
pub use oid::Oid;
pub use refs::{
    BranchColorId, GroupOptions, LabelDisplay, RefGroup, RefGroupKind, branch_color_id,
    group_refs_for_table, shorten_group_label, shorten_ref_name,
};
pub use repo::{
    CommitSummary, RefInfo, RefKind, RefreshScope, RepoSnapshot, RepositoryState, RewritePlanError,
};
pub use stage::{
    AmendCommitPreview, FileStatusKind, StageEntry, StageNodeKind, StageStatusSnapshot,
    StageTarget, StageTreeOptions, StageTreeRow, StageTreeRowKind, UNTRACKED_COLLAPSE_THRESHOLD,
    build_stage_tree_rows, collect_targets_under_row, next_anchor_after_mutation,
};
pub use stage_tracker::{
    DiffBlock, LineEnding, LineGutterMark, LineSelection, LineSlice, SlicedDocument,
    StageDiffHunk, StageDiffLineMeta, StageLineTracker, StagedRange, compute_diff_blocks,
    merge_stage_blocks,
};
pub use ui::{
    BranchTreeNodeId, ChangeNodeId, ModalButton, PaneId, SplitAxis, SplitterId, TreeId,
    WorkspaceTab,
};

/// Compacts single-child directory chains (`a` -> `b` -> `c` becomes `a/b/c`).
pub(crate) fn compact_dir_chain<T>(
    mut label: String,
    mut node: T,
    mut next_single: impl FnMut(&mut T) -> Option<(String, T)>,
) -> (String, T) {
    while let Some((child_name, child)) = next_single(&mut node) {
        label.push('/');
        label.push_str(&child_name);
        node = child;
    }
    (label, node)
}
