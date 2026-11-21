use crate::Oid;

/// Interactive-rebase todo verb (`pick / reword / squash / fixup / drop`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TodoAction {
    Pick,
    Reword,
    Squash,
    Fixup,
    Drop,
}

impl TodoAction {
    pub const ALL: [Self; 5] = [
        Self::Pick,
        Self::Reword,
        Self::Squash,
        Self::Fixup,
        Self::Drop,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Reword => "reword",
            Self::Squash => "squash",
            Self::Fixup => "fixup",
            Self::Drop => "drop",
        }
    }

    pub fn from_key(c: char) -> Option<Self> {
        match c.to_ascii_lowercase() {
            'p' => Some(Self::Pick),
            'r' | 'w' => Some(Self::Reword),
            's' => Some(Self::Squash),
            'f' => Some(Self::Fixup),
            'd' => Some(Self::Drop),
            _ => None,
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Pick => Self::Reword,
            Self::Reword => Self::Squash,
            Self::Squash => Self::Fixup,
            Self::Fixup => Self::Drop,
            Self::Drop => Self::Pick,
        }
    }
}

/// `base..HEAD` rewrite plan starting at a commit (oldest first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewritePlan {
    pub base: Option<Oid>,
    pub entries: Vec<TodoEntry>,
    pub messages: Vec<String>,
}

/// One line of a rebase plan, oldest commit first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoEntry {
    pub action: TodoAction,
    pub oid: Oid,
    pub subject: String,
    /// Replacement message for `Reword`, or the combined message applied after
    /// the last `Squash` / `Fixup` of a group. `None` keeps Git's message.
    pub message: Option<String>,
}

impl TodoEntry {
    pub fn pick(oid: Oid, subject: impl Into<String>) -> Self {
        Self {
            action: TodoAction::Pick,
            oid,
            subject: subject.into(),
            message: None,
        }
    }
}

/// Renders an editor-free rebase todo: only `pick` / `fixup` / `drop` / `exec`
/// lines are emitted, rewording via `exec git commit --amend --only -F <file>`
/// so Git never needs to open `GIT_EDITOR`.
pub fn render_todo(entries: &[TodoEntry], mut message_file: impl FnMut(usize) -> String) -> String {
    let mut out = String::new();
    for (idx, e) in entries.iter().enumerate() {
        let verb = match e.action {
            TodoAction::Pick | TodoAction::Reword => "pick",
            TodoAction::Squash | TodoAction::Fixup => "fixup",
            TodoAction::Drop => "drop",
        };
        out.push_str(&format!(
            "{verb} {} {}\n",
            e.oid,
            sanitize_subject(&e.subject)
        ));
        let next_folds = entries
            .get(idx + 1)
            .is_some_and(|n| matches!(n.action, TodoAction::Squash | TodoAction::Fixup));
        let needs_amend = match e.action {
            TodoAction::Reword => e.message.is_some(),
            TodoAction::Squash | TodoAction::Fixup => e.message.is_some() && !next_folds,
            _ => false,
        };
        if needs_amend {
            out.push_str(&format!(
                "exec git commit --amend --only --allow-empty --no-verify -F {}\n",
                message_file(idx)
            ));
        }
    }
    out
}

/// Computes the message each `Squash` group should end with when the user did
/// not supply one.
pub fn default_squash_messages(entries: &mut [TodoEntry], full_message: impl Fn(&Oid) -> String) {
    let mut group_start: Option<usize> = None;
    let mut acc: Vec<String> = Vec::new();
    let len = entries.len();
    for idx in 0..len {
        let action = entries[idx].action;
        match action {
            TodoAction::Pick | TodoAction::Reword => {
                group_start = Some(idx);
                acc = vec![
                    entries[idx]
                        .message
                        .clone()
                        .unwrap_or_else(|| full_message(&entries[idx].oid)),
                ];
            }
            TodoAction::Squash => {
                if group_start.is_some() {
                    acc.push(full_message(&entries[idx].oid));
                }
            }
            TodoAction::Fixup | TodoAction::Drop => {}
        }
        let next_folds = entries
            .get(idx + 1)
            .is_some_and(|n| matches!(n.action, TodoAction::Squash | TodoAction::Fixup));
        if matches!(action, TodoAction::Squash | TodoAction::Fixup)
            && !next_folds
            && entries[idx].message.is_none()
            && group_start.is_some()
        {
            let has_squash = entries[group_start.unwrap_or(0)..=idx]
                .iter()
                .any(|e| e.action == TodoAction::Squash);
            if has_squash {
                entries[idx].message = Some(acc.join("\n\n"));
            }
        }
    }
}

fn sanitize_subject(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}
