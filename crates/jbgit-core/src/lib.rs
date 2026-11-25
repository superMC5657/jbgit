pub mod branch_tree;
pub mod commit_msg;
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
pub mod patch;
pub mod reflog;
pub mod refs;
pub mod repo;
pub mod stage;
pub mod stage_tracker;
pub mod tree;
pub mod ui;
pub mod worktree;

pub use branch_tree::{
    BranchCategory, BranchNodeKind, BranchTreeExpansion, BranchTreeNode, BranchTreeOptions,
    BranchTreeRow, BranchesPanelSettings, BranchesTreeModel, FavoriteBranches, fuzzy_match,
    natural_cmp,
};
pub use commit_msg::{
    COMMIT_BODY_MAX_WIDTH, parse_comment_char, reformat_commit_message,
    resolve_push_refspec_target, strip_commit_comments,
};
pub use conflict::{
    ConflictBlock, ConflictChoice, ConflictDocument, ConflictPaneRow, ConflictParseOptions,
    ConflictRowPlaceholder, ConflictSegment, ConflictSideInfo, ConflictSidePresence,
    ConflictSideStatus, LoadedConflict, SpecialConflictInfo, SpecialConflictKind,
    ThreeWayBlockKind, ThreeWayHunk, conflict_side_statuses, is_reverse_conflict_state,
    three_way_merge_blocks,
};
pub use console::{
    ConsoleLine, ConsoleLineSeverity, ConsoleStream, GitConsoleEntry, classify_console_line,
};
pub use details::{
    BinaryDiffInfo, BlameBundle, BlameCommit, BlameKey, BlameLine, BlameOptions, ChangeRowKind,
    ChangeTreeRow, CommitChangesBundle, CommitDetails, CommitSignature,
    DEFAULT_FILE_HISTORY_LIMIT, DetailsKey, DetailsMode, DiffContext, DiffHunk, DiffLine,
    DiffLineKind, EAGER_WORD_DIFF_PAIR_BUDGET, FileChange, FileDiff, FileDiffKey,
    FileHistoryBundle, FileHistoryEntry, FileHistoryKey, FileModeChange, GIT_MODE_EXECUTABLE,
    GIT_MODE_GITLINK, GIT_MODE_REGULAR, GIT_MODE_SYMLINK, HistoryBranchScope, LfsDiff, LfsPointer,
    MAX_DIFF_FILE_BYTES, MAX_OUTGOING_COMMITS, MAX_SELECTION_HISTORY_LINES,
    MAX_WORD_DIFF_LINE_BYTES, MessageLineKind, MoveDetection, NO_NEWLINE_LINE_TEXT, OutgoingBundle,
    OutgoingKey, PersonStamp, SideBySideRow, SignatureStatus, WhitespaceMode, apply_word_diff,
    apply_word_diff_with_budget, build_change_rows, build_change_rows_filtered,
    classify_message_line, format_byte_size, format_signed_byte_delta,
    has_lfs_filter_in_gitattributes, issue_ref_ranges, paired_line_in_hunk, parse_lfs_pointer,
    parse_unified_patch_hunks, side_by_side_rows, word_diff,
};
pub use epoch::{EpochToken, RequestEpoch, RequestSlot, RequestSlotId, RequestSlotTable, SlotRegistry};
pub use filter::{
    BranchFilter, DateFilter, FilterError, LogFilterSpec, PathFilter, RangeFilter, RepoUser,
    TextFilter,
    TextMatcher, UserFilter, UserMatcher, glob_match,
};
pub use git_ops::{
    FastForward, FixupKind, GitFailure, GitOp, GitOpOutcome, IgnoreRuleKind, MergeOptionConflict,
    MergeOptions, OngoingKind, OngoingStep, PatchSource, PullOptionConflict, PullOptions,
    PushForce, PushOutcomeSummary, PushTagMode, RebaseOptionError, RebaseOptions, RefNameError,
    ResetMode, RewritePlan, SkippedBranch, SquashTodoPlan, StashEntry, TodoAction, TodoEntry,
    UpdateInfo, UpdateMode, UpdatedRange, build_ignore_patterns, classify_failure,
    default_squash_messages, fuzzy_filter_paths, fuzzy_indices, infer_clone_dir_name,
    is_gpg_sign_failure, is_protected_branch, match_worktreeinclude_paths,
    matches_protected_branch_patterns, parse_push_porcelain, parse_recent_checkouts,
    plan_squash_todo, render_todo, sanitize_branch_name, sanitize_branch_name_on_typing, short_rev,
    validate_ref_name,
    validate_ref_name_typed,
};
pub use key_token::{KeyToken, KeyTokenError, KeyTokenKind, parse_key_token};
pub use lru::LruCache;
pub use oid::Oid;
pub use patch::{
    ApplyMode, ApplyOptions, PatchCheck, PatchCheckKey, PatchFileSection, PatchFileStat,
    PatchInput, apply_strip_and_directory, build_untracked_file_patch, first_apply_error,
    needs_three_way_index_fallback, parse_apply_check, parse_apply_conflicts, parse_patch_sections,
    parse_patch_sections_with_options,
};
pub use reflog::{
    CherryPickedKey, DEFAULT_REFLOG_LIMIT, REFLOG_GIT_FORMAT, ReflogActionKind, ReflogBundle,
    ReflogEntry, ReflogKey, parse_git_cherry_output, parse_reflog_output,
};
pub use refs::{
    BranchColorId, GroupOptions, LabelDisplay, RefGroup, RefGroupKind, branch_color_id,
    group_refs_for_table, shorten_group_label, shorten_ref_name,
};
pub use repo::{
    BisectInfo, BranchCompareDirection, CleanupBranchCandidate, CommitSummary, RefInfo, RefKind,
    RefreshScope, RemoteInfo, RepoSnapshot, RepositoryState, RewritePlanError, redact_url,
    redact_urls_in,
};
pub use stage::{
    AmendCommitPreview, FileStatusKind, StageEntry, StageNodeKind, StageStatusSnapshot,
    StageTarget, StageTreeOptions, StageTreeRow, StageTreeRowKind, SubmoduleState,
    UNTRACKED_COLLAPSE_THRESHOLD, build_stage_tree_rows, collect_targets_under_row,
    next_anchor_after_mutation,
};
pub use stage_tracker::{
    BINARY_SNIFF_BYTES, DiffBlock, LineEnding, LineGutterMark, LineSelection, LineSlice,
    MAX_DIFF_LINES, SlicedDocument, StageDiffHunk, StageDiffLineMeta, StageLineTracker,
    StagedRange, assemble_hunks, build_diff_hunks, build_diff_hunks_with_whitespace,
    compute_diff_blocks, compute_diff_blocks_with_whitespace, is_binary_bytes, merge_stage_blocks,
};
pub use tree::{
    TreeBlob, TreeBlobContent, TreeBlobKey, TreeEntry, TreeEntryKind, TreeKey, TreeListing,
    sort_tree_entries,
};
pub use ui::{
    BranchTreeNodeId, ChangeNodeId, ModalButton, PaneId, SplitAxis, SplitterId, TreeId,
    WorkspaceTab,
};
pub use worktree::{
    GitVersion, WorktreeBranch, WorktreeInfo, default_worktree_path, parse_worktree_list_porcelain,
    same_path,
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
