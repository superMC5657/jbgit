pub mod branch_tree;
pub mod conflict;
pub mod console;
pub mod details;
pub mod epoch;
pub mod filter;
pub mod git_ops;
pub mod graph;
pub mod key_token;
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
    BlameBundle, BlameCommit, BlameKey, BlameLine, BlameOptions, ChangeRowKind, ChangeTreeRow,
    CommitChangesBundle, CommitDetails, DetailsKey, DetailsMode, DiffContext, DiffHunk, DiffLine,
    DiffLineKind, EAGER_WORD_DIFF_PAIR_BUDGET, FileChange, FileDiff, FileDiffKey,
    FileHistoryBundle, FileHistoryEntry, FileHistoryKey, MAX_DIFF_FILE_BYTES, MAX_OUTGOING_COMMITS,
    MAX_SELECTION_HISTORY_LINES, MAX_WORD_DIFF_LINE_BYTES, MessageLineKind, OutgoingBundle,
    OutgoingKey, PersonStamp, SideBySideRow, SignatureStatus, apply_word_diff,
    apply_word_diff_with_budget, build_change_rows, classify_message_line, issue_ref_ranges,
    parse_unified_patch_hunks, side_by_side_rows, word_diff,
};
pub use epoch::{EpochToken, RequestEpoch, RequestSlot, RequestSlotId, RequestSlotTable, SlotRegistry};
pub use filter::{
    BranchFilter, DateFilter, FilterError, LogFilterSpec, PathFilter, RangeFilter, RepoUser,
    TextFilter,
    TextMatcher, UserFilter, UserMatcher, glob_match,
};
pub use git_ops::{
    FastForward, GitFailure, GitOp, GitOpOutcome, MergeOptionConflict, MergeOptions, OngoingKind,
    OngoingStep, PatchSource, PushForce, PushOutcomeSummary, PushTagMode, RefNameError, ResetMode,
    RewritePlan, SkippedBranch, StashEntry, TodoAction, TodoEntry, UpdateInfo, UpdateMode,
    UpdatedRange, classify_failure, default_squash_messages, fuzzy_indices, is_protected_branch,
    matches_protected_branch_patterns, parse_push_porcelain, parse_recent_checkouts, render_todo,
    short_rev, validate_ref_name, validate_ref_name_typed,
};
pub use key_token::{KeyToken, KeyTokenError, KeyTokenKind, parse_key_token};
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
    BINARY_SNIFF_BYTES, DiffBlock, LineEnding, LineGutterMark, LineSelection, LineSlice,
    MAX_DIFF_LINES, SlicedDocument, StageDiffHunk, StageDiffLineMeta, StageLineTracker,
    StagedRange, assemble_hunks, build_diff_hunks, compute_diff_blocks, is_binary_bytes,
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
