use jbgit_core::{
    BranchTreeNodeId, ChangeNodeId, ModalButton, Oid, PaneId, SplitAxis, SplitterId, TreeId,
    WorkspaceTab,
};
use ratatui::layout::Rect;

use crate::log_view::FilterField;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchesToolbarButton {
    /// `[«]` collapse the Branches pane.
    Collapse,
    /// `[⌖]` navigate the log to the selected branch.
    Locate,
    /// `[⊞]` toggle Group by Directory.
    GroupByDir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangesToolbarButton {
    GroupByDir,
    ExpandAll,
    CollapseAll,
    DiffPreview,
    SwitchParent,
    OnlyAffected,
    SwapCompare,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffToolbarButton {
    SideBySide,
    Context,
    SwapCompare,
    Dock,
    Zoom,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageToolbarButton {
    Refresh,
    StageAllTracked,
    Stash,
    GroupByDir,
    ToggleDiff,
    Options,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageDiffToolbarButton {
    SwitchSide,
    SideBySide,
    Context,
    PrevHunk,
    NextHunk,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffHunkActionKind {
    Stage,
    Unstage,
    Rollback,
    Inspect3Way,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOptionKind {
    Amend,
    Signoff,
    RunHooks,
    AuthorToggle,
    AuthorInput,
    History,
    ExternalEditor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogButtonKind {
    Confirm,
    Cancel,
}

/// Buttons of the ongoing-operation banner (`Git.Ongoing.Rebase.Actions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OngoingButton {
    Resolve,
    Continue,
    Skip,
    Abort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StashButton {
    Apply,
    Pop,
    Branch,
    Drop,
    New,
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictMergeButtonKind {
    ApplyNonConflicting,
    ToggleBase,
    AcceptOurs,
    AcceptTheirs,
    AcceptBoth,
    AcceptNeither,
    InlineEdit,
    ResetBlock,
    AcceptAllOurs,
    AcceptAllTheirs,
    Mergetool,
    ExternalEditor,
    SaveAndStage,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictQueueButtonKind {
    Merge,
    AcceptYours,
    AcceptTheirs,
    RevertResolved,
    Mergetool,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileHistoryToolbarButton {
    FollowRenames,
    CompareLocal,
    SideBySide,
    Context,
    Zoom,
    LocateInLog,
    Annotate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotateToolbarButton {
    IgnoreWhitespace,
    DetectMoves,
    PreviousRev,
    NavBack,
    ShowDiff,
    SelectionHistory,
    LocateInLog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOptionKind {
    Tags,
    Force,
    SetUpstream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushActionButtonKind {
    Confirm,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphHitKind {
    Node {
        commit_oid: Oid,
        /// Fold marker: clicking expands the collapsed branch below.
        collapsed_below: bool,
    },
    Arrow {
        up_row: usize,
        down_row: usize,
        target_row: usize,
        target_oid: Oid,
        is_down: bool,
    },
    Edge {
        up_row: usize,
        down_row: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitTarget {
    ToolTab(WorkspaceTab),
    BranchWidgetPill,
    HeaderRefreshButton,
    /// `↓N` behind counter in the header branch pill (opens Update Project).
    HeaderBehindBadge,
    HeaderGlyphButton,
    HeaderHelpButton,
    Splitter {
        id: SplitterId,
        axis: SplitAxis,
    },
    Pane(PaneId),
    BranchTreeNode {
        pane: PaneId,
        node_id: BranchTreeNodeId,
        row: usize,
    },
    /// `▼` / `▶` expander of a Branches tree folder row (row index).
    BranchTreeToggle {
        row: usize,
    },
    BranchesToolbar(BranchesToolbarButton),
    /// 2-column `»` stripe shown while the Branches pane is collapsed.
    BranchesExpandStripe,
    FilterTextInput,
    FilterRegexToggle,
    FilterCaseToggle,
    FilterChip(FilterField),
    FilterClear(FilterField),
    /// `×` of the read-only Range chip (Update Info "View").
    FilterRangeClear,
    FilterPresentation,
    LogChangeRow {
        idx: usize,
    },
    ChangesToolbar(ChangesToolbarButton),
    /// Clickable `[Filtered: M/N]` badge on the Log Changes header or empty-filter hint.
    ChangesFilteredBadge,
    /// Clickable prompt to load aggregated changes when > MAX_AGGREGATED_COMMITS are selected.
    ChangesLoadMany,
    DetailsHash {
        oid: Oid,
    },
    DetailsParent {
        oid: Oid,
    },
    DetailsArea,
    DiffToolbar(DiffToolbarButton),
    DiffArea,
    GraphPrintElement {
        visible_row: usize,
        element_idx: usize,
        commit_oid: Oid,
        kind: GraphHitKind,
    },
    CommitRefPill {
        visible_row: usize,
        ref_name: String,
        is_detached_head: bool,
    },
    /// Trailing `+N` pill summarising refs that did not fit the row.
    CommitRefOverflow {
        visible_row: usize,
        hidden_refs: Vec<String>,
    },
    CommitTableRow {
        visible_row: usize,
        commit_oid: Oid,
    },
    CommitSignatureCell {
        visible_row: usize,
        commit_oid: Oid,
    },
    LogShallowUnshallow,
    ChangesTreeNode {
        tree_id: TreeId,
        node_id: ChangeNodeId,
        on_hover_action_icon: bool,
    },
    StageToolbar(StageToolbarButton),
    StageResolveConflicts,
    StageDiffToolbar(StageDiffToolbarButton),
    DiffHunkAction {
        hunk_idx: usize,
        action: DiffHunkActionKind,
    },
    StageDiffLine {
        row_idx: usize,
    },
    StageDiffGutter {
        row_idx: usize,
        staged_range_idx: Option<usize>,
    },
    CommitEditorArea,
    CommitOption(CommitOptionKind),
    CommitButton {
        and_push: bool,
    },
    UndoCommitButton,
    ConsoleCommandRow {
        idx: usize,
    },
    ConsoleOutputArea,
    MenuItem {
        menu_depth: u8,
        item_idx: usize,
    },
    /// Menu border / separator area (absorbs clicks).
    MenuBody {
        menu_depth: u8,
    },
    DialogBody,
    DialogOption(usize),
    DialogButton(DialogButtonKind),
    DialogCheckbox,
    BranchPopupRow(usize),
    BranchPopupStar(usize),
    /// Row of the `Configure Remotes…` modal.
    RemotesRow(usize),
    /// Visible row of the repository switcher.
    RepoSwitcherRow(usize),
    /// Row of the `Working Trees` modal.
    WorktreesRow(usize),
    /// Apply Patch mode radio.
    ApplyPatchMode(jbgit_core::ApplyMode),
    /// Apply Patch `[Apply]` (`true`) / `[Cancel]`.
    ApplyPatchButton {
        apply: bool,
    },
    /// Tree Browser row (index into the flattened rows).
    TreeBrowserRow(usize),
    TreeBrowserPreview,
    /// Repository name in the top header (opens the switcher).
    HeaderRepoName,
    BranchPopupFetch,
    BranchPopupSettings,
    OngoingBanner(OngoingButton),
    NotificationAction,
    NotificationDismiss,
    StashRow(usize),
    StashChangeRow(usize),
    StashToolbar(StashButton),
    StashChangesToolbar(ChangesToolbarButton),
    StashDiffToolbar(DiffToolbarButton),
    ConflictMergeBlock(usize),
    ConflictMergeButton(ConflictMergeButtonKind),
    ConflictQueueRow(usize),
    ConflictQueueButton(ConflictQueueButtonKind),
    FileHistoryRow {
        idx: usize,
    },
    FileHistoryToolbar(FileHistoryToolbarButton),
    FileHistoryListArea,
    FileHistoryDiffArea,
    AnnotateLine {
        line_idx: usize,
    },
    AnnotateToolbar(AnnotateToolbarButton),
    AnnotateArea,
    AnnotateDiffArea,
    PushTargetBranch,
    PushRemotePicker,
    PushRemoteOption(usize),
    PushCompletionItem(usize),
    PushCommitRow(usize),
    PushCommitListArea,
    PushFileRow(usize),
    PushFileListArea,
    PushDiffArea,
    PushDiffBackButton,
    PushOption(PushOptionKind),
    PushActionButton(PushActionButtonKind),
    ModalBackdrop {
        layer_z: u8,
    },
    ModalButton(ModalButton),
}

impl HitTarget {
    /// Whether hovering over this target triggers visual styling changes or tooltips.
    /// Used by mouse move handler to avoid spurious full redraws on passive areas.
    pub fn has_visual_hover(&self) -> bool {
        !matches!(
            self,
            HitTarget::Pane(_)
                | HitTarget::DetailsArea
                | HitTarget::DiffArea
                | HitTarget::CommitEditorArea
                | HitTarget::ConsoleOutputArea
                | HitTarget::StageDiffLine { .. }
                | HitTarget::StageDiffGutter { .. }
                | HitTarget::FileHistoryListArea
                | HitTarget::FileHistoryDiffArea
                | HitTarget::AnnotateArea
                | HitTarget::AnnotateDiffArea
                | HitTarget::PushCommitListArea
                | HitTarget::PushFileListArea
                | HitTarget::PushDiffArea
                | HitTarget::DialogBody
                | HitTarget::MenuBody { .. }
                | HitTarget::ModalBackdrop { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HitRegion {
    pub rect: Rect,
    pub z_index: u8,
    pub target: HitTarget,
}

impl HitRegion {
    pub fn contains(&self, col: u16, row: u16) -> bool {
        self.rect.width > 0
            && self.rect.height > 0
            && col >= self.rect.x
            && col < self.rect.x.saturating_add(self.rect.width)
            && row >= self.rect.y
            && row < self.rect.y.saturating_add(self.rect.height)
    }
}

/// Immediate-mode spatial hit-test registry populated during every render pass.
/// Resolves hits by descending `z_index`, breaking ties in favor of later-registered
/// (child/foreground) regions.
#[derive(Debug, Default)]
pub struct HitTestRegistry {
    regions: Vec<HitRegion>,
}

impl HitTestRegistry {
    pub fn new() -> Self {
        Self {
            regions: Vec::with_capacity(128),
        }
    }

    pub fn clear(&mut self) {
        self.regions.clear();
    }

    pub fn register(&mut self, rect: Rect, z_index: u8, target: HitTarget) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        self.regions.push(HitRegion {
            rect,
            z_index,
            target,
        });
    }

    /// Last-registered region whose target satisfies `pred` (keyboard menus).
    pub fn find(&self, pred: impl Fn(&HitTarget) -> bool) -> Option<&HitRegion> {
        self.regions.iter().rev().find(|r| pred(&r.target))
    }

    /// Returns the topmost region containing `(col, row)`:
    /// highest `z_index` wins; ties resolve to the last-registered region.
    pub fn hit_test(&self, col: u16, row: u16) -> Option<&HitRegion> {
        let mut best: Option<(usize, &HitRegion)> = None;
        for (order, region) in self.regions.iter().enumerate() {
            if !region.contains(col, row) {
                continue;
            }
            match best {
                None => best = Some((order, region)),
                Some((best_order, best_reg)) => {
                    if region.z_index > best_reg.z_index
                        || (region.z_index == best_reg.z_index && order > best_order)
                    {
                        best = Some((order, region));
                    }
                }
            }
        }
        best.map(|(_, reg)| reg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn z_index_and_registration_order_occlusion() {
        let mut reg = HitTestRegistry::new();

        // Layer 1: background pane + underlying table row
        reg.register(
            Rect::new(0, 0, 80, 24),
            1,
            HitTarget::Pane(PaneId::LogCommitTable),
        );
        reg.register(
            Rect::new(2, 5, 60, 1),
            1,
            HitTarget::CommitTableRow {
                visible_row: 3,
                commit_oid: Oid::ZERO,
            },
        );

        // Same z_index: child row overrides parent pane
        let hit_row = reg.hit_test(10, 5).unwrap();
        assert_eq!(
            hit_row.target,
            HitTarget::CommitTableRow {
                visible_row: 3,
                commit_oid: Oid::ZERO,
            }
        );

        // Layer 3: modal backdrop + menu item occludes underlying Layer 1 row even if registered later
        reg.register(
            Rect::new(0, 0, 80, 24),
            30,
            HitTarget::ModalBackdrop { layer_z: 30 },
        );
        reg.register(
            Rect::new(8, 4, 20, 1),
            31,
            HitTarget::MenuItem {
                menu_depth: 0,
                item_idx: 0,
            },
        );

        // Even if another Layer 1 item is registered after, Layer 3 still occludes it
        reg.register(
            Rect::new(2, 5, 60, 1),
            1,
            HitTarget::CommitTableRow {
                visible_row: 4,
                commit_oid: Oid::ZERO,
            },
        );

        let occluded = reg.hit_test(10, 4).unwrap();
        assert_eq!(
            occluded.target,
            HitTarget::MenuItem {
                menu_depth: 0,
                item_idx: 0
            }
        );

        let backdrop_hit = reg.hit_test(10, 5).unwrap();
        assert_eq!(
            backdrop_hit.target,
            HitTarget::ModalBackdrop { layer_z: 30 }
        );
    }
}
