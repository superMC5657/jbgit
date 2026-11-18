#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileStatusKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    Typechange,
    Untracked,
    Ignored,
    Conflicted,
}

impl FileStatusKind {
    pub const fn badge_char(self) -> char {
        match self {
            Self::Modified => 'M',
            Self::Added => 'A',
            Self::Deleted => 'D',
            Self::Renamed => 'R',
            Self::Copied => 'C',
            Self::Typechange => 'T',
            Self::Untracked => '?',
            Self::Ignored => 'I',
            Self::Conflicted => '!',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageEntry {
    pub path: String,
    pub old_path: Option<String>,
    pub kind: FileStatusKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StageStatusSnapshot {
    pub staged: Vec<StageEntry>,
    pub unstaged: Vec<StageEntry>,
    pub untracked: Vec<StageEntry>,
    pub conflicted: Vec<StageEntry>,
    pub ignored: Vec<StageEntry>,
}

impl StageStatusSnapshot {
    pub fn total_changes(&self) -> usize {
        self.staged.len() + self.unstaged.len() + self.untracked.len() + self.conflicted.len()
    }
}
