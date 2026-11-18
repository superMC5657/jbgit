/// Top-level tool window tabs matching IDEA Git ToolWindow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkspaceTab {
    Commit,
    Log,
    Stash,
    Console,
}

impl WorkspaceTab {
    pub const ALL: [Self; 4] = [Self::Commit, Self::Log, Self::Stash, Self::Console];

    pub const fn index(self) -> usize {
        match self {
            Self::Commit => 1,
            Self::Log => 2,
            Self::Stash => 3,
            Self::Console => 4,
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::Commit => "Commit",
            Self::Log => "Log",
            Self::Stash => "Stash",
            Self::Console => "Console",
        }
    }

    pub fn from_digit(digit: char) -> Option<Self> {
        match digit {
            '1' => Some(Self::Commit),
            '2' => Some(Self::Log),
            '3' => Some(Self::Stash),
            '4' => Some(Self::Console),
            _ => None,
        }
    }

    pub fn default_pane(self) -> PaneId {
        match self {
            Self::Commit => PaneId::CommitChangesTree,
            Self::Log => PaneId::LogCommitTable,
            Self::Stash => PaneId::StashList,
            Self::Console => PaneId::ConsoleCommandList,
        }
    }

    pub fn focus_ring(self) -> &'static [PaneId] {
        match self {
            Self::Commit => &[
                PaneId::CommitChangesTree,
                PaneId::CommitMessageEditor,
                PaneId::CommitDiffPreview,
            ],
            Self::Log => &[
                PaneId::LogBranchesTree,
                PaneId::LogCommitTable,
                PaneId::LogDetailsDiff,
            ],
            Self::Stash => &[PaneId::StashList, PaneId::StashDiffPreview],
            Self::Console => &[PaneId::ConsoleCommandList, PaneId::ConsoleOutputDetail],
        }
    }
}

/// Logical focusable panes across all tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneId {
    CommitChangesTree,
    CommitMessageEditor,
    CommitDiffPreview,
    LogBranchesTree,
    LogCommitTable,
    LogDetailsDiff,
    StashList,
    StashDiffPreview,
    ConsoleCommandList,
    ConsoleOutputDetail,
}

impl PaneId {
    pub const fn label(self) -> &'static str {
        match self {
            Self::CommitChangesTree => "Changes",
            Self::CommitMessageEditor => "Commit Message",
            Self::CommitDiffPreview => "Diff Preview",
            Self::LogBranchesTree => "Branches",
            Self::LogCommitTable => "Commits & Graph",
            Self::LogDetailsDiff => "Commit Details",
            Self::StashList => "Stashes",
            Self::StashDiffPreview => "Stash Diff",
            Self::ConsoleCommandList => "Commands",
            Self::ConsoleOutputDetail => "Output",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SplitAxis {
    /// Side-by-side panes separated by a 1-column vertical `│` line.
    Horizontal,
    /// Stacked panes separated by a 1-row horizontal `─` line.
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SplitterId {
    CommitMainHorizontal,
    CommitLeftVertical,
    LogLeftHorizontal,
    LogRightHorizontal,
    StashMainHorizontal,
    ConsoleMainHorizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BranchTreeNodeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TreeId(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChangeNodeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModalButton {
    Confirm,
    Cancel,
}

impl ModalButton {
    pub const fn toggle(self) -> Self {
        match self {
            Self::Confirm => Self::Cancel,
            Self::Cancel => Self::Confirm,
        }
    }
}
