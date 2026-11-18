pub mod branch_tree;
pub mod console;
pub mod details;
pub mod epoch;
pub mod filter;
pub mod graph;
pub mod lru;
pub mod oid;
pub mod refs;
pub mod repo;
pub mod stage;
pub mod ui;

pub use branch_tree::{
    BranchCategory, BranchNodeKind, BranchTreeExpansion, BranchTreeNode, BranchTreeOptions,
    BranchTreeRow, BranchesPanelSettings, BranchesTreeModel, FavoriteBranches, fuzzy_match,
    natural_cmp,
};
pub use console::GitConsoleEntry;
pub use details::{
    ChangeRowKind, ChangeTreeRow, CommitChangesBundle, CommitDetails, DetailsKey, DiffContext,
    DiffHunk, DiffLine, DiffLineKind, FileChange, FileDiff, FileDiffKey, MessageLineKind,
    PersonStamp, SideBySideRow, SignatureStatus, apply_word_diff, build_change_rows,
    classify_message_line, issue_ref_ranges, side_by_side_rows, word_diff,
};
pub use epoch::{EpochToken, EpochTracker, RequestEpoch};
pub use filter::{
    BranchFilter, DateFilter, FilterError, LogFilterSpec, PathFilter, RepoUser, TextFilter,
    TextMatcher, UserFilter, glob_match,
};
pub use lru::LruCache;
pub use oid::Oid;
pub use refs::{
    BranchColorId, GroupOptions, LabelDisplay, RefGroup, RefGroupKind, branch_color_id,
    group_refs_for_table, shorten_group_label, shorten_ref_name,
};
pub use repo::{CommitSummary, RefInfo, RefKind, RefreshScope, RepoSnapshot, RepositoryState};
pub use stage::{FileStatusKind, StageEntry, StageStatusSnapshot};
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
