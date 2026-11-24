use crate::Oid;

/// Interactive-rebase todo verb (`pick / reword / edit / squash / fixup / drop / update-ref`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TodoAction {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
    /// Read-only preserved `update-ref <ref>` line from `git rebase --update-refs`.
    UpdateRef,
}

impl TodoAction {
    pub const ALL: [Self; 6] = [
        Self::Pick,
        Self::Reword,
        Self::Edit,
        Self::Squash,
        Self::Fixup,
        Self::Drop,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Reword => "reword",
            Self::Edit => "edit",
            Self::Squash => "squash",
            Self::Fixup => "fixup",
            Self::Drop => "drop",
            Self::UpdateRef => "update-ref",
        }
    }

    pub const fn is_editable(self) -> bool {
        !matches!(self, Self::UpdateRef)
    }

    pub const fn is_commit(self) -> bool {
        !matches!(self, Self::UpdateRef)
    }

    pub fn from_key(c: char) -> Option<Self> {
        match c.to_ascii_lowercase() {
            'p' => Some(Self::Pick),
            'r' | 'w' => Some(Self::Reword),
            'e' => Some(Self::Edit),
            's' => Some(Self::Squash),
            'f' => Some(Self::Fixup),
            'x' => Some(Self::Drop),
            _ => None,
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Pick => Self::Reword,
            Self::Reword => Self::Edit,
            Self::Edit => Self::Squash,
            Self::Squash => Self::Fixup,
            Self::Fixup => Self::Drop,
            Self::Drop => Self::Pick,
            Self::UpdateRef => Self::UpdateRef,
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

/// Planned squash rewrite (supports non-contiguous commits by reordering them
/// adjacent to the oldest selected commit, matching `GitRebaseTodoModel.unite`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SquashTodoPlan {
    pub base: Option<Oid>,
    pub todo: Vec<TodoEntry>,
    pub combined_message: String,
    /// Index of the final `Squash` entry in `todo` where the combined message belongs.
    pub message_entry_idx: usize,
    /// True when unselected commits existed between the selected commits and had to be reordered.
    pub reordered: bool,
}

/// Builds a squash todo plan from `plan` for `selected_oids` (any order).
///
/// When `selected_oids` are non-contiguous within `plan.entries`, the non-root
/// selected commits are moved immediately after the oldest selected commit
/// (preserving chronological order within both the squashed group and the
/// remaining commits). Returns `None` if fewer than 2 commits are selected or
/// any selected commit is missing from `plan.entries`.
pub fn plan_squash_todo(plan: &RewritePlan, selected_oids: &[Oid]) -> Option<SquashTodoPlan> {
    if selected_oids.len() < 2 {
        return None;
    }
    let positions: Vec<usize> = plan
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.action.is_commit() && selected_oids.contains(&e.oid))
        .map(|(i, _)| i)
        .collect();
    if positions.len() != selected_oids.len() {
        return None;
    }
    let first = positions[0];
    let last = *positions.last()?;
    let reordered = last - first + 1 != positions.len();
    let combined_message = positions
        .iter()
        .map(|&i| plan.messages[i].as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut todo = Vec::with_capacity(plan.entries.len());
    // 1. Entries strictly before the oldest selected commit stay unchanged.
    todo.extend(plan.entries[..first].iter().cloned());
    // 2. Oldest selected commit stays as the squash group root (`Pick`).
    let mut root = plan.entries[first].clone();
    root.action = TodoAction::Pick;
    todo.push(root);
    // 3. Remaining selected commits move immediately after root as `Squash`.
    for &idx in &positions[1..] {
        let mut child = plan.entries[idx].clone();
        child.action = TodoAction::Squash;
        todo.push(child);
    }
    let message_entry_idx = first + positions.len() - 1;
    // 4. All unselected entries after `first` follow in their original order.
    for (idx, entry) in plan.entries.iter().enumerate().skip(first + 1) {
        if !positions.contains(&idx) {
            todo.push(entry.clone());
        }
    }

    Some(SquashTodoPlan {
        base: plan.base,
        todo,
        combined_message,
        message_entry_idx,
        reordered,
    })
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

    pub fn update_ref(ref_name: impl Into<String>) -> Self {
        Self {
            action: TodoAction::UpdateRef,
            oid: Oid::ZERO,
            subject: ref_name.into(),
            message: None,
        }
    }
}

/// Renders an editor-free rebase todo: only `pick` / `edit` / `fixup` / `drop` /
/// `update-ref` / `exec` lines are emitted, rewording via
/// `exec git commit --amend --only --allow-empty [--no-verify] -F <file>`
/// so Git never needs to open `GIT_EDITOR`.
///
/// By default (`no_verify = false`), repository hooks are respected.
pub fn render_todo(
    entries: &[TodoEntry],
    no_verify: bool,
    mut message_file: impl FnMut(usize) -> String,
) -> String {
    let mut out = String::new();
    for (idx, e) in entries.iter().enumerate() {
        if e.action == TodoAction::UpdateRef {
            out.push_str(&format!("update-ref {}\n", sanitize_subject(&e.subject)));
            continue;
        }
        let verb = match e.action {
            TodoAction::Pick | TodoAction::Reword => "pick",
            TodoAction::Edit => "edit",
            TodoAction::Squash | TodoAction::Fixup => "fixup",
            TodoAction::Drop => "drop",
            TodoAction::UpdateRef => unreachable!(),
        };
        out.push_str(&format!(
            "{verb} {} {}\n",
            e.oid,
            sanitize_subject(&e.subject)
        ));
        let next_folds = next_commit_folds(entries, idx);
        let needs_amend = match e.action {
            TodoAction::Reword => e.message.is_some(),
            TodoAction::Squash | TodoAction::Fixup => e.message.is_some() && !next_folds,
            _ => false,
        };
        if needs_amend {
            if no_verify {
                out.push_str(&format!(
                    "exec git commit --amend --only --allow-empty --no-verify -F {}\n",
                    message_file(idx)
                ));
            } else {
                out.push_str(&format!(
                    "exec git commit --amend --only --allow-empty -F {}\n",
                    message_file(idx)
                ));
            }
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
            TodoAction::Pick | TodoAction::Reword | TodoAction::Edit => {
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
            TodoAction::Fixup | TodoAction::Drop | TodoAction::UpdateRef => {}
        }
        let next_folds = next_commit_folds(entries, idx);
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

/// True when the next non-`Drop`, non-`UpdateRef` entry after `idx` folds into
/// the preceding commit (`Squash` or `Fixup`).
fn next_commit_folds(entries: &[TodoEntry], idx: usize) -> bool {
    entries
        .get(idx + 1..)
        .and_then(|rest| {
            rest.iter()
                .find(|e| !matches!(e.action, TodoAction::Drop | TodoAction::UpdateRef))
        })
        .is_some_and(|n| matches!(n.action, TodoAction::Squash | TodoAction::Fixup))
}

fn sanitize_subject(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}
