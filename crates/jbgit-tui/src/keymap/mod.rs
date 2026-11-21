//! Dynamic keymap parser, typed actions, sequence matcher, Which-Key completions,
//! and searchable/executable command-help builder.

pub mod actions;
pub mod help;
pub(crate) mod resolve;

use std::fmt;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::config::keymap_config::KeyBindingConfig;

pub use actions::{
    check_scopes, execute_action, is_action_applicable, is_diff_focused, route_dynamic_key,
    route_editor_dynamic_key, scopes_for_context,
};
pub use help::{
    HelpRow, build_help_rows, first_help_entry, localized_action_desc, pending_completions,
    pending_prefix_display, resolve_help_selection, step_help_selection,
};

pub const KEY_CHORD_WINDOW: Duration = Duration::from_millis(1200);

/// Typed keymap action identifiers replacing raw string dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyAction {
    Noop,
    Quit,
    Help,
    ContextMenu,
    Refresh,
    TabCommit,
    TabLog,
    TabBranches,
    TabStash,
    TabConsole,
    FocusNext,
    FocusPrev,
    ToggleBranchesPopup,
    ToggleBranchesPane,
    GitFetch,
    GitPull,
    GitPush,
    Palette,
    ToggleGlyphMode,
    CycleGraphRenderer,
    GitStatusProbe,
    GotoRevision,
    CursorDown,
    CursorUp,
    GotoTop,
    GotoBottom,
    PageDownHalf,
    PageUpHalf,
    CompareOrDiff,
    CompareVersions,
    CopyHash,
    Checkout,
    NewBranch,
    ChooseParent,
    FocusFilter,
    FileHistory,
    Uncommit,
    Reword,
    DropCommit,
    BranchSearch,
    ToggleFavorite,
    RenameBranch,
    DeleteBranch,
    NavigateBranch,
    FocusCommitMsg,
    ToggleStage,
    StageSelection,
    UnstageSelection,
    Commit,
    CommitAndPush,
    ToggleAmend,
    ToggleSignoff,
    CommitHistory,
    SelectAll,
    DiscardChanges,
    ToggleGroup,
    EditFile,
    ToggleVisual,
    RollbackSelection,
    UndoRollback,
    SwitchDiffSide,
    ThreeWayInspector,
    PrevHunk,
    NextHunk,
    PrevFile,
    NextFile,
    ToggleSideBySide,
    CycleDiffContext,
    CycleDiffDock,
    ZoomDiff,
    StashNew,
    StashApply,
    StashPop,
    StashDrop,
    StashBranch,
    StashClear,
    Unknown(String),
}

impl KeyAction {
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "noop" => Self::Noop,
            "quit" => Self::Quit,
            "help" => Self::Help,
            "context_menu" => Self::ContextMenu,
            "refresh" => Self::Refresh,
            "tab_commit" => Self::TabCommit,
            "tab_log" => Self::TabLog,
            "tab_branches" => Self::TabBranches,
            "tab_stash" => Self::TabStash,
            "tab_console" => Self::TabConsole,
            "focus_next" => Self::FocusNext,
            "focus_prev" => Self::FocusPrev,
            "toggle_branches_popup" => Self::ToggleBranchesPopup,
            "toggle_branches_pane" => Self::ToggleBranchesPane,
            "git_fetch" => Self::GitFetch,
            "git_pull" => Self::GitPull,
            "git_push" => Self::GitPush,
            "palette" => Self::Palette,
            "toggle_glyph_mode" => Self::ToggleGlyphMode,
            "cycle_graph_renderer" => Self::CycleGraphRenderer,
            "git_status_probe" => Self::GitStatusProbe,
            "goto_revision" => Self::GotoRevision,
            "cursor_down" => Self::CursorDown,
            "cursor_up" => Self::CursorUp,
            "goto_top" => Self::GotoTop,
            "goto_bottom" => Self::GotoBottom,
            "page_down_half" => Self::PageDownHalf,
            "page_up_half" => Self::PageUpHalf,
            "compare_or_diff" => Self::CompareOrDiff,
            "compare_versions" => Self::CompareVersions,
            "copy_hash" => Self::CopyHash,
            "checkout" => Self::Checkout,
            "new_branch" => Self::NewBranch,
            "choose_parent" => Self::ChooseParent,
            "focus_filter" => Self::FocusFilter,
            "file_history" => Self::FileHistory,
            "uncommit" => Self::Uncommit,
            "reword" => Self::Reword,
            "drop_commit" => Self::DropCommit,
            "branch_search" => Self::BranchSearch,
            "toggle_favorite" => Self::ToggleFavorite,
            "rename_branch" => Self::RenameBranch,
            "delete_branch" => Self::DeleteBranch,
            "navigate_branch" => Self::NavigateBranch,
            "focus_commit_msg" => Self::FocusCommitMsg,
            "toggle_stage" => Self::ToggleStage,
            "stage_selection" => Self::StageSelection,
            "unstage_selection" => Self::UnstageSelection,
            "commit" => Self::Commit,
            "commit_and_push" => Self::CommitAndPush,
            "toggle_amend" => Self::ToggleAmend,
            "toggle_signoff" => Self::ToggleSignoff,
            "commit_history" => Self::CommitHistory,
            "select_all" => Self::SelectAll,
            "discard_changes" => Self::DiscardChanges,
            "toggle_group" => Self::ToggleGroup,
            "edit_file" => Self::EditFile,
            "toggle_visual" => Self::ToggleVisual,
            "rollback_selection" => Self::RollbackSelection,
            "undo_rollback" => Self::UndoRollback,
            "switch_diff_side" => Self::SwitchDiffSide,
            "three_way_inspector" => Self::ThreeWayInspector,
            "prev_hunk" => Self::PrevHunk,
            "next_hunk" => Self::NextHunk,
            "prev_file" => Self::PrevFile,
            "next_file" => Self::NextFile,
            "toggle_side_by_side" => Self::ToggleSideBySide,
            "cycle_diff_context" => Self::CycleDiffContext,
            "cycle_diff_dock" => Self::CycleDiffDock,
            "zoom_diff" => Self::ZoomDiff,
            "stash_new" => Self::StashNew,
            "stash_apply" => Self::StashApply,
            "stash_pop" => Self::StashPop,
            "stash_drop" => Self::StashDrop,
            "stash_branch" => Self::StashBranch,
            "stash_clear" => Self::StashClear,
            other => Self::Unknown(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Noop => "noop",
            Self::Quit => "quit",
            Self::Help => "help",
            Self::ContextMenu => "context_menu",
            Self::Refresh => "refresh",
            Self::TabCommit => "tab_commit",
            Self::TabLog => "tab_log",
            Self::TabBranches => "tab_branches",
            Self::TabStash => "tab_stash",
            Self::TabConsole => "tab_console",
            Self::FocusNext => "focus_next",
            Self::FocusPrev => "focus_prev",
            Self::ToggleBranchesPopup => "toggle_branches_popup",
            Self::ToggleBranchesPane => "toggle_branches_pane",
            Self::GitFetch => "git_fetch",
            Self::GitPull => "git_pull",
            Self::GitPush => "git_push",
            Self::Palette => "palette",
            Self::ToggleGlyphMode => "toggle_glyph_mode",
            Self::CycleGraphRenderer => "cycle_graph_renderer",
            Self::GitStatusProbe => "git_status_probe",
            Self::GotoRevision => "goto_revision",
            Self::CursorDown => "cursor_down",
            Self::CursorUp => "cursor_up",
            Self::GotoTop => "goto_top",
            Self::GotoBottom => "goto_bottom",
            Self::PageDownHalf => "page_down_half",
            Self::PageUpHalf => "page_up_half",
            Self::CompareOrDiff => "compare_or_diff",
            Self::CompareVersions => "compare_versions",
            Self::CopyHash => "copy_hash",
            Self::Checkout => "checkout",
            Self::NewBranch => "new_branch",
            Self::ChooseParent => "choose_parent",
            Self::FocusFilter => "focus_filter",
            Self::FileHistory => "file_history",
            Self::Uncommit => "uncommit",
            Self::Reword => "reword",
            Self::DropCommit => "drop_commit",
            Self::BranchSearch => "branch_search",
            Self::ToggleFavorite => "toggle_favorite",
            Self::RenameBranch => "rename_branch",
            Self::DeleteBranch => "delete_branch",
            Self::NavigateBranch => "navigate_branch",
            Self::FocusCommitMsg => "focus_commit_msg",
            Self::ToggleStage => "toggle_stage",
            Self::StageSelection => "stage_selection",
            Self::UnstageSelection => "unstage_selection",
            Self::Commit => "commit",
            Self::CommitAndPush => "commit_and_push",
            Self::ToggleAmend => "toggle_amend",
            Self::ToggleSignoff => "toggle_signoff",
            Self::CommitHistory => "commit_history",
            Self::SelectAll => "select_all",
            Self::DiscardChanges => "discard_changes",
            Self::ToggleGroup => "toggle_group",
            Self::EditFile => "edit_file",
            Self::ToggleVisual => "toggle_visual",
            Self::RollbackSelection => "rollback_selection",
            Self::UndoRollback => "undo_rollback",
            Self::SwitchDiffSide => "switch_diff_side",
            Self::ThreeWayInspector => "three_way_inspector",
            Self::PrevHunk => "prev_hunk",
            Self::NextHunk => "next_hunk",
            Self::PrevFile => "prev_file",
            Self::NextFile => "next_file",
            Self::ToggleSideBySide => "toggle_side_by_side",
            Self::CycleDiffContext => "cycle_diff_context",
            Self::CycleDiffDock => "cycle_diff_dock",
            Self::ZoomDiff => "zoom_diff",
            Self::StashNew => "stash_new",
            Self::StashApply => "stash_apply",
            Self::StashPop => "stash_pop",
            Self::StashDrop => "stash_drop",
            Self::StashBranch => "stash_branch",
            Self::StashClear => "stash_clear",
            Self::Unknown(s) => s.as_str(),
        }
    }

    pub fn is_active(&self) -> bool {
        !matches!(self, Self::Noop | Self::Unknown(_))
    }

    /// Returns the default i18n key for this action's footer pill label, if any.
    pub fn footer_pill_key(&self) -> Option<&'static str> {
        Some(match self {
            Self::Noop | Self::Unknown(_) => return None,
            Self::Quit => "view.footer.pill.quit",
            Self::Help => "view.footer.pill.help",
            Self::ContextMenu => "view.footer.pill.menu",
            Self::Refresh => "view.footer.pill.refresh",
            Self::TabCommit => "view.footer.pill.tab_commit",
            Self::TabLog => "view.footer.pill.tab_log",
            Self::TabBranches => "view.footer.pill.tab_branches",
            Self::TabStash => "view.footer.pill.tab_stash",
            Self::TabConsole => "view.footer.pill.tab_console",
            Self::FocusNext => "view.footer.pill.focus_next",
            Self::FocusPrev => "view.footer.pill.focus_prev",
            Self::ToggleBranchesPopup => "view.footer.pill.branches",
            Self::ToggleBranchesPane => "view.footer.pill.branches_pane",
            Self::GitFetch => "view.footer.pill.fetch",
            Self::GitPull => "view.footer.pill.pull",
            Self::GitPush => "view.footer.pill.push",
            Self::Palette => "view.footer.pill.palette",
            Self::ToggleGlyphMode => "view.footer.pill.glyph_mode",
            Self::CycleGraphRenderer => "view.footer.pill.graph_renderer",
            Self::GitStatusProbe => "view.footer.pill.status_probe",
            Self::GotoRevision => "view.footer.pill.goto",
            Self::CursorDown => "view.footer.pill.cursor_down",
            Self::CursorUp => "view.footer.pill.cursor_up",
            Self::GotoTop => "view.footer.pill.goto_top",
            Self::GotoBottom => "view.footer.pill.goto_bottom",
            Self::PageDownHalf => "view.footer.pill.page_down_half",
            Self::PageUpHalf => "view.footer.pill.page_up_half",
            Self::CompareOrDiff | Self::CompareVersions => "view.footer.pill.diff",
            Self::CopyHash => "view.footer.pill.copy_hash",
            Self::Checkout => "view.footer.pill.checkout",
            Self::NewBranch => "view.footer.pill.new_branch",
            Self::ChooseParent => "view.footer.pill.parent",
            Self::FocusFilter => "view.footer.pill.filter",
            Self::FileHistory => "view.footer.pill.file_history",
            Self::Uncommit => "view.footer.pill.uncommit",
            Self::Reword => "view.footer.pill.reword",
            Self::DropCommit => "view.footer.pill.drop_commit",
            Self::BranchSearch => "view.footer.pill.search",
            Self::ToggleFavorite => "view.footer.pill.favorite",
            Self::RenameBranch => "view.footer.pill.rename",
            Self::DeleteBranch => "view.footer.pill.delete",
            Self::NavigateBranch => "view.footer.pill.navigate_branch",
            Self::FocusCommitMsg => "view.footer.pill.msg",
            Self::ToggleStage | Self::StageSelection => "view.footer.pill.stage",
            Self::UnstageSelection => "view.footer.pill.unstage",
            Self::Commit => "view.footer.pill.commit",
            Self::CommitAndPush => "view.footer.pill.commit_push",
            Self::ToggleAmend => "view.footer.pill.amend",
            Self::ToggleSignoff => "view.footer.pill.signoff",
            Self::CommitHistory => "view.footer.pill.history",
            Self::SelectAll => "view.footer.pill.select_all",
            Self::DiscardChanges => "view.footer.pill.discard",
            Self::ToggleGroup => "view.footer.pill.group",
            Self::EditFile => "view.footer.pill.edit",
            Self::ToggleVisual => "view.footer.pill.visual",
            Self::RollbackSelection => "view.footer.pill.rollback",
            Self::UndoRollback => "view.footer.pill.undo_rollback",
            Self::SwitchDiffSide => "view.footer.pill.switch_side",
            Self::ThreeWayInspector => "view.footer.pill.three_way",
            Self::PrevHunk => "view.footer.pill.prev_hunk",
            Self::NextHunk => "view.footer.pill.next_hunk",
            Self::PrevFile => "view.footer.pill.prev_file",
            Self::NextFile => "view.footer.pill.next_file",
            Self::ToggleSideBySide => "view.footer.pill.side_by_side",
            Self::CycleDiffContext => "view.footer.pill.context",
            Self::CycleDiffDock => "view.footer.pill.dock",
            Self::ZoomDiff => "view.footer.pill.zoom",
            Self::StashNew => "view.footer.pill.stash_new",
            Self::StashApply => "view.footer.pill.apply",
            Self::StashPop => "view.footer.pill.pop",
            Self::StashDrop => "view.footer.pill.drop",
            Self::StashBranch => "view.footer.pill.new_branch",
            Self::StashClear => "view.footer.pill.stash_clear",
        })
    }
}

impl fmt::Display for KeyAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for KeyAction {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for KeyAction {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::parse(&s))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyStroke {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

fn parse_named_key(name: &str) -> Option<KeyCode> {
    match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => Some(KeyCode::Enter),
        "esc" | "escape" => Some(KeyCode::Esc),
        "space" => Some(KeyCode::Char(' ')),
        "tab" => Some(KeyCode::Tab),
        "backtab" => Some(KeyCode::BackTab),
        "backspace" => Some(KeyCode::Backspace),
        "del" | "delete" => Some(KeyCode::Delete),
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "pageup" | "pgup" => Some(KeyCode::PageUp),
        "pagedown" | "pgdn" => Some(KeyCode::PageDown),
        f if f.starts_with('f') && f.len() > 1 && f[1..].chars().all(|c| c.is_ascii_digit()) => {
            f[1..].parse::<u8>().ok().map(KeyCode::F)
        }
        _ => None,
    }
}

impl KeyStroke {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }

        // Bracketed notation, e.g. "<C-d>", "<A-1>", "<Space>", "<C-Enter>", "<F2>", "<C-S-k>"
        if s.starts_with('<') && s.ends_with('>') && s.len() >= 3 {
            let inner = &s[1..s.len() - 1];
            let parts: Vec<&str> = inner.split('-').collect();
            if parts.is_empty() {
                return None;
            }

            let mut ctrl = false;
            let mut alt = false;
            let mut shift = false;

            for &m in &parts[..parts.len() - 1] {
                match m.to_ascii_lowercase().as_str() {
                    "c" | "ctrl" | "control" => ctrl = true,
                    "a" | "alt" => alt = true,
                    "s" | "shift" => shift = true,
                    _ => {}
                }
            }

            let key_part = parts[parts.len() - 1];
            let parsed_code = parse_named_key(key_part).or_else(|| {
                let mut chars = key_part.chars();
                let first = chars.next()?;
                chars.next().is_none().then_some(KeyCode::Char(first))
            })?;

            // Normalize <S-Tab>/<BackTab> and standalone <S-x> (to uppercase 'X' + shift)
            // so equality, matching, and display agree.
            let (code, shift) =
                if (parsed_code == KeyCode::Tab && shift) || parsed_code == KeyCode::BackTab {
                    (KeyCode::BackTab, false)
                } else if !ctrl
                    && !alt
                    && shift
                    && let KeyCode::Char(c) = parsed_code
                    && c.is_ascii_lowercase()
                {
                    (KeyCode::Char(c.to_ascii_uppercase()), true)
                } else if !ctrl
                    && !alt
                    && let KeyCode::Char(c) = parsed_code
                    && c.is_ascii_uppercase()
                {
                    (KeyCode::Char(c), true)
                } else {
                    (parsed_code, shift)
                };

            Some(KeyStroke {
                code,
                ctrl,
                alt,
                shift,
            })
        } else {
            // Non-bracketed representation: "q", "G", "j", "k", "r", "?", etc.
            let mut chars = s.chars();
            let first = chars.next()?;
            if chars.next().is_none() {
                let shift = first.is_ascii_uppercase();
                Some(KeyStroke {
                    code: KeyCode::Char(first),
                    ctrl: false,
                    alt: false,
                    shift,
                })
            } else {
                let code = parse_named_key(s)?;
                Some(KeyStroke {
                    code,
                    ctrl: false,
                    alt: false,
                    shift: false,
                })
            }
        }
    }

    pub fn is_editor_chord(&self) -> bool {
        self.ctrl
            || self.alt
            || matches!(
                self.code,
                KeyCode::Tab | KeyCode::BackTab | KeyCode::F(_)
            )
    }

    pub fn matches(&self, event: &KeyEvent) -> bool {
        let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
        let alt = event.modifiers.contains(KeyModifiers::ALT);
        let shift = event.modifiers.contains(KeyModifiers::SHIFT);

        if self.ctrl != ctrl || self.alt != alt {
            return false;
        }

        match (self.code, event.code) {
            (KeyCode::Char(a), KeyCode::Char(b)) => {
                if self.ctrl || self.alt {
                    if a.is_ascii_alphabetic() {
                        a.eq_ignore_ascii_case(&b) && self.shift == shift
                    } else {
                        a == b
                    }
                } else if a.is_ascii_uppercase() {
                    a == b || (a.to_ascii_lowercase() == b && shift)
                } else if a.is_ascii_lowercase() {
                    if self.shift {
                        a.eq_ignore_ascii_case(&b) && (shift || b.is_ascii_uppercase())
                    } else {
                        a == b && !shift
                    }
                } else if a == ' ' {
                    b == ' ' && self.shift == shift
                } else {
                    // Shifted punctuation like '?', '{', '}', '+' may include SHIFT in crossterm.
                    a == b
                }
            }
            (KeyCode::BackTab, KeyCode::BackTab) => true,
            (KeyCode::BackTab, KeyCode::Tab) => shift,
            (KeyCode::Tab, KeyCode::BackTab) => self.shift,
            (a, b) => a == b && self.shift == shift,
        }
    }

    pub fn to_display_string(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        let suppress_shift = matches!(self.code, KeyCode::BackTab)
            || matches!(self.code, KeyCode::Char(c) if c.is_ascii_uppercase())
            || (self.code == KeyCode::Tab && self.shift);
        if self.shift && !suppress_shift {
            s.push_str("Shift+");
        }
        match self.code {
            KeyCode::Char(' ') => s.push_str("Space"),
            KeyCode::Char(c) if (self.ctrl || self.alt) && c.is_ascii_alphabetic() => {
                s.push(c.to_ascii_uppercase());
            }
            KeyCode::Char(c) => s.push(c),
            KeyCode::Enter => s.push_str("Enter"),
            KeyCode::Esc => s.push_str("Esc"),
            KeyCode::Backspace => s.push_str("Backspace"),
            KeyCode::Delete => s.push_str("Del"),
            KeyCode::Up => s.push('↑'),
            KeyCode::Down => s.push('↓'),
            KeyCode::Left => s.push('←'),
            KeyCode::Right => s.push('→'),
            KeyCode::Home => s.push_str("Home"),
            KeyCode::End => s.push_str("End"),
            KeyCode::PageUp => s.push_str("PgUp"),
            KeyCode::PageDown => s.push_str("PgDn"),
            KeyCode::Tab if self.shift => s.push_str("Shift+Tab"),
            KeyCode::Tab => s.push_str("Tab"),
            KeyCode::BackTab => s.push_str("Shift+Tab"),
            KeyCode::F(n) => s.push_str(&format!("F{n}")),
            _ => s.push('?'),
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySequence {
    pub strokes: Vec<KeyStroke>,
}

impl KeySequence {
    pub fn parse(keys: &[String]) -> Option<Self> {
        if keys.is_empty() {
            return None;
        }
        let mut strokes = Vec::with_capacity(keys.len());
        for k in keys {
            strokes.push(KeyStroke::parse(k)?);
        }
        Some(Self { strokes })
    }

    pub fn starts_with_editor_chord(&self) -> bool {
        self.strokes.first().is_some_and(KeyStroke::is_editor_chord)
    }

    pub fn matches_exact(&self, events: &[KeyEvent]) -> bool {
        if self.strokes.len() != events.len() {
            return false;
        }
        self.strokes
            .iter()
            .zip(events.iter())
            .all(|(stroke, ev)| stroke.matches(ev))
    }

    pub fn matches_prefix(&self, events: &[KeyEvent]) -> bool {
        if events.is_empty() || events.len() >= self.strokes.len() {
            return false;
        }
        self.strokes[..events.len()]
            .iter()
            .zip(events.iter())
            .all(|(stroke, ev)| stroke.matches(ev))
    }

    pub fn to_display_string(&self) -> String {
        self.strokes
            .iter()
            .map(|s| s.to_display_string())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub enum MatchResult<'a> {
    Exact(&'a KeyBindingConfig),
    Prefix,
    None,
}

pub fn match_bindings<'a>(
    bindings: &'a [KeyBindingConfig],
    events: &[KeyEvent],
) -> MatchResult<'a> {
    match_bindings_filtered(bindings, events, |_| true)
}

pub fn match_bindings_filtered<'a, F>(
    bindings: &'a [KeyBindingConfig],
    events: &[KeyEvent],
    mut allow: F,
) -> MatchResult<'a>
where
    F: FnMut(&KeyBindingConfig) -> bool,
{
    let mut prefix_found = false;

    for binding in bindings {
        if !allow(binding) {
            continue;
        }
        if let Some(seq) = binding.parsed_seq() {
            if seq.matches_exact(events) {
                return MatchResult::Exact(binding);
            }
            if seq.matches_prefix(events) {
                prefix_found = true;
            }
        }
    }

    if prefix_found {
        MatchResult::Prefix
    } else {
        MatchResult::None
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use jbgit_core::{GitOp, PaneId, WorkspaceTab};

    use super::*;
    use crate::app::{Action, AppState};
    use crate::config::{AppConfig, KeymapConfigOverlay};

    #[test]
    fn parse_and_match_keystrokes() {
        let k = KeyStroke::parse("<C-d>").unwrap();
        assert_eq!(k.code, KeyCode::Char('d'));
        assert!(k.ctrl);
        assert!(!k.alt);
        assert_eq!(k.to_display_string(), "Ctrl+D");

        let ev_ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(k.matches(&ev_ctrl_d));

        let ev_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(!k.matches(&ev_d));

        let k_space = KeyStroke::parse("<Space>").unwrap();
        let ev_space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(k_space.matches(&ev_space));

        let k_f2 = KeyStroke::parse("<F2>").unwrap();
        let ev_f2 = KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE);
        assert!(k_f2.matches(&ev_f2));

        let k_a1 = KeyStroke::parse("<A-1>").unwrap();
        let ev_a1 = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT);
        assert!(k_a1.matches(&ev_a1));

        // Shifted punctuation on Windows/Kitty must match even when SHIFT modifier is present.
        let k_qmark = KeyStroke::parse("?").unwrap();
        let ev_qmark_shift = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert!(k_qmark.matches(&ev_qmark_shift));
        let k_lbrace = KeyStroke::parse("{").unwrap();
        let ev_lbrace_shift = KeyEvent::new(KeyCode::Char('{'), KeyModifiers::SHIFT);
        assert!(k_lbrace.matches(&ev_lbrace_shift));

        // <S-x> (lowercase or uppercase) must match Shift+X and equal "X".
        let k_sx = KeyStroke::parse("<S-x>").unwrap();
        let k_x_upper = KeyStroke::parse("X").unwrap();
        assert_eq!(k_sx, k_x_upper);
        assert!(k_sx.matches(&KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT)));
        assert!(k_sx.matches(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::SHIFT)));
        assert!(k_sx.matches(&KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE)));
        assert!(!k_sx.matches(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)));
        let k_sx_raw = KeyStroke {
            code: KeyCode::Char('x'),
            ctrl: false,
            alt: false,
            shift: true,
        };
        assert!(k_sx_raw.matches(&KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT)));
        assert!(k_sx_raw.matches(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::SHIFT)));

        // Ctrl+Shift+K must stay distinct from Ctrl+K.
        let k_csk = KeyStroke::parse("<C-S-k>").unwrap();
        let k_ck = KeyStroke::parse("<C-k>").unwrap();
        let ev_csk = KeyEvent::new(KeyCode::Char('K'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        let ev_ck = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert!(k_csk.matches(&ev_csk));
        assert!(!k_ck.matches(&ev_csk));
        assert!(k_ck.matches(&ev_ck));
        assert!(!k_csk.matches(&ev_ck));
        assert_eq!(k_csk.to_display_string(), "Ctrl+Shift+K");

        // BackTab matches with or without SHIFT modifier.
        let k_bt = KeyStroke::parse("<BackTab>").unwrap();
        assert!(k_bt.matches(&KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)));
        assert_eq!(k_bt.to_display_string(), "Shift+Tab");
    }

    #[test]
    fn default_keymap_has_no_unknown_actions() {
        let cfg = AppConfig::default();
        for scope in [
            crate::config::KeymapScope::Global,
            crate::config::KeymapScope::Log,
            crate::config::KeymapScope::Commit,
            crate::config::KeymapScope::Branch,
            crate::config::KeymapScope::Diff,
            crate::config::KeymapScope::Stash,
            crate::config::KeymapScope::Console,
        ] {
            for b in cfg.keymap.bindings_for(scope) {
                assert!(
                    !matches!(b.run, KeyAction::Unknown(_)),
                    "unknown action in default keymap ({scope:?}): {:?}",
                    b.run
                );
            }
        }
    }

    #[test]
    fn sequence_exact_and_prefix_matching() {
        let seq = KeySequence::parse(&["g".to_string(), "g".to_string()]).unwrap();
        let ev_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);

        assert!(seq.matches_prefix(&[ev_g]));
        assert!(seq.matches_exact(&[ev_g, ev_g]));

        let ev_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        assert!(!seq.matches_prefix(&[ev_j]));
        assert!(!seq.matches_exact(&[ev_g, ev_j]));
    }

    #[test]
    fn dynamic_keymap_routing_and_remapping() {
        let config = AppConfig::default();
        let mut state = AppState::new_with_config(PathBuf::from("."), None, config);

        // 1. Default routing: "r" refreshes
        let ev_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        let actions = route_dynamic_key(&ev_r, &mut state);
        assert!(matches!(actions.as_deref(), Some([Action::RefreshAll])));

        // 2. Default routing: multi-key sequence "g" then "g" + Which-Key completions
        let ev_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
        let first = route_dynamic_key(&ev_g, &mut state);
        assert!(matches!(first.as_deref(), Some([]))); // prefix consumed
        let completions = pending_completions(&state);
        assert!(completions.iter().any(|(k, _)| k == "g"));
        assert!(completions.iter().any(|(k, _)| k == "r"));
        assert!(completions.iter().any(|(k, _)| k == "b"));
        let second = route_dynamic_key(&ev_g, &mut state);
        assert!(matches!(
            second.as_deref(),
            Some([Action::LogEdge { bottom: false }])
        ));

        // 2b. Esc cancels pending chord cleanly
        let first_esc = route_dynamic_key(&ev_g, &mut state);
        assert!(matches!(first_esc.as_deref(), Some([])));
        assert!(!state.pending_keys.is_empty());
        let cancel = route_dynamic_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);
        assert!(matches!(cancel.as_deref(), Some([])));
        assert!(state.pending_keys.is_empty());

        // 3. Diff scope routing: 'S' toggles side-by-side
        state.active_tab = WorkspaceTab::Log;
        state.focused_pane = PaneId::LogDetailsDiff;
        state.details.diff_preview = true;
        let ev_s_upper = KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT);
        let actions_s = route_dynamic_key(&ev_s_upper, &mut state);
        assert!(matches!(
            actions_s.as_deref(),
            Some([Action::DiffToggleSideBySide])
        ));

        // 3b. Global remote keys: b (branches popup), f (fetch), P (push)
        state.focused_pane = PaneId::LogCommitTable;
        let ev_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE);
        assert!(matches!(
            route_dynamic_key(&ev_b, &mut state).as_deref(),
            Some([Action::OpenBranchesPopup { .. }])
        ));
        let ev_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(matches!(
            route_dynamic_key(&ev_f, &mut state).as_deref(),
            Some([Action::RunGitOp(GitOp::Fetch)])
        ));

        // 4. Commit Diff: 'r' refreshes (does NOT rollback), while 'd' rolls back!
        state.active_tab = WorkspaceTab::Commit;
        state.focused_pane = PaneId::CommitDiffPreview;
        let actions_r_diff = route_dynamic_key(&ev_r, &mut state);
        assert!(matches!(actions_r_diff.as_deref(), Some([Action::RefreshAll])));
        let ev_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        let actions_d_diff = route_dynamic_key(&ev_d, &mut state);
        assert!(matches!(
            actions_d_diff.as_deref(),
            Some([Action::StageDiffRollbackSelection])
        ));

        // 4b. Ctrl+S submits commit in Commit view
        let ev_cs = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(matches!(
            route_dynamic_key(&ev_cs, &mut state).as_deref(),
            Some([Action::SubmitCommit { and_push: false }])
        ));

        // 5. Custom keymap: remap refresh to F5 via overlay (key_bindings mode)
        let mut custom_config = AppConfig::default();
        let ov: KeymapConfigOverlay = toml::from_str(
            r#"
[global]
key_bindings = [{ on = ["<F5>"], run = "refresh" }]
"#,
        )
        .unwrap();
        custom_config.keymap.merge_overlay(&ov);
        let mut state_custom = AppState::new_with_config(PathBuf::from("."), None, custom_config);

        let ev_f5 = KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE);
        let actions_f5 = route_dynamic_key(&ev_f5, &mut state_custom);
        assert!(matches!(actions_f5.as_deref(), Some([Action::RefreshAll])));

        // Pressing r does NOT refresh when replaced via key_bindings
        let actions_r = route_dynamic_key(&ev_r, &mut state_custom);
        assert!(actions_r.is_none());

        // Existing global bindings like 'q' are preserved
        let ev_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let actions_q = route_dynamic_key(&ev_q, &mut state_custom);
        assert!(matches!(actions_q.as_deref(), Some([Action::Quit])));
    }

    #[test]
    fn commit_editor_honors_dynamic_keymap_and_ignores_plain_keys() {
        let mut cfg = AppConfig::default();
        let ov: KeymapConfigOverlay = toml::from_str(
            r#"
[commit]
prepend_keymap = [
    { on = "<C-s>", run = "noop" },
    { on = "<F9>", run = "commit" },
]
"#,
        )
        .unwrap();
        cfg.keymap.merge_overlay(&ov);
        let mut state = AppState::new_with_config(PathBuf::from("."), None, cfg);
        state.active_tab = WorkspaceTab::Commit;
        state.focused_pane = PaneId::CommitMessageEditor;

        // Unbound Ctrl+S no longer triggers SubmitCommit
        let ev_cs = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(route_editor_dynamic_key(&ev_cs, &mut state).is_none());

        // Custom <F9> triggers SubmitCommit inside editor
        let ev_f9 = KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE);
        assert!(matches!(
            route_editor_dynamic_key(&ev_f9, &mut state).as_deref(),
            Some([Action::SubmitCommit { and_push: false }])
        ));

        // Plain 'A' or 'q' must NOT fire inside editor
        let ev_upper_a = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT);
        assert!(route_editor_dynamic_key(&ev_upper_a, &mut state).is_none());
        let ev_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(route_editor_dynamic_key(&ev_q, &mut state).is_none());
    }

    #[test]
    fn help_rows_merge_aliases_and_filter_search() {
        let config = AppConfig::default();
        let state = AppState::new_with_config(PathBuf::from("."), None, config);
        let rows = build_help_rows(&state, "");
        assert!(matches!(rows.first(), Some(HelpRow::Section(_))));
        assert!(rows.iter().any(|r| matches!(
            r,
            HelpRow::Entry { keys, run: Some(run), .. } if *run == KeyAction::CursorDown && keys == "j / ↓"
        )));
        // Palette is available under advanced section and executable
        let palette_rows = build_help_rows(&state, "palette");
        assert!(palette_rows.iter().any(|r| matches!(
            r,
            HelpRow::Entry { run: Some(run), .. } if *run == KeyAction::Palette
        )));
    }

    #[test]
    fn console_default_navigation_and_context_help() {
        let mut state = AppState::new_with_config(PathBuf::from("."), None, AppConfig::default());
        state.active_tab = WorkspaceTab::Console;
        state.focused_pane = PaneId::ConsoleCommandList;

        let ev_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        let ev_k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        let ev_down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        let ev_up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        let ev_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
        let ev_upper_g = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);

        assert!(matches!(
            route_dynamic_key(&ev_j, &mut state).as_deref(),
            Some([Action::ScrollConsoleList(1)])
        ));
        assert!(matches!(
            route_dynamic_key(&ev_k, &mut state).as_deref(),
            Some([Action::ScrollConsoleList(-1)])
        ));
        assert!(matches!(
            route_dynamic_key(&ev_down, &mut state).as_deref(),
            Some([Action::ScrollConsoleList(1)])
        ));
        assert!(matches!(
            route_dynamic_key(&ev_up, &mut state).as_deref(),
            Some([Action::ScrollConsoleList(-1)])
        ));
        assert!(matches!(
            route_dynamic_key(&ev_upper_g, &mut state).as_deref(),
            Some([Action::SelectConsoleRow(usize::MAX)])
        ));
        assert!(matches!(
            route_dynamic_key(&ev_g, &mut state).as_deref(),
            Some([])
        ));
        assert!(matches!(
            route_dynamic_key(&ev_g, &mut state).as_deref(),
            Some([Action::SelectConsoleRow(0)])
        ));
        assert!(matches!(
            crate::input::route_terminal_event(
                crossterm::event::Event::Key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)),
                &mut state,
            )
            .as_slice(),
            [Action::ScrollConsoleList(10)]
        ));
        assert!(matches!(
            crate::input::route_terminal_event(
                crossterm::event::Event::Key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)),
                &mut state,
            )
            .as_slice(),
            [Action::ScrollConsoleList(-10)]
        ));

        let list_help = build_help_rows(&state, "");
        let list_ctx: Vec<&HelpRow> = list_help
            .iter()
            .skip(1)
            .take_while(|r| !matches!(r, HelpRow::Section(_)))
            .collect();
        assert!(list_ctx.iter().any(|r| matches!(
            r,
            HelpRow::Entry { run: Some(KeyAction::GitStatusProbe), .. }
        )));
        assert!(list_ctx.iter().any(|r| matches!(
            r,
            HelpRow::Entry { run: Some(KeyAction::CursorDown), .. }
        )));
        assert!(list_ctx.iter().any(|r| matches!(
            r,
            HelpRow::Entry { run: Some(KeyAction::GotoTop), .. }
        )));

        state.focused_pane = PaneId::ConsoleOutputDetail;
        assert!(matches!(
            route_dynamic_key(&ev_j, &mut state).as_deref(),
            Some([Action::ScrollConsoleOutput(1)])
        ));
        assert!(matches!(
            route_dynamic_key(&ev_k, &mut state).as_deref(),
            Some([Action::ScrollConsoleOutput(-1)])
        ));

        let detail_help = build_help_rows(&state, "");
        let detail_ctx: Vec<&HelpRow> = detail_help
            .iter()
            .skip(1)
            .take_while(|r| !matches!(r, HelpRow::Section(_)))
            .collect();
        assert!(!detail_ctx.iter().any(|r| matches!(
            r,
            HelpRow::Entry { run: Some(KeyAction::GitStatusProbe), .. }
        )));
        assert!(detail_ctx.iter().any(|r| matches!(
            r,
            HelpRow::Entry { run: Some(KeyAction::CursorDown), .. }
        )));
        assert!(detail_ctx.iter().any(|r| matches!(
            r,
            HelpRow::Entry { run: Some(KeyAction::PageDownHalf), .. }
        )));
        assert!(detail_ctx.iter().any(|r| matches!(
            r,
            HelpRow::Entry { keys, run: None, .. } if keys == "Esc"
        )));
    }

    #[test]
    fn zoomed_diff_routes_cursor_and_goto_to_diff_even_from_log_table_focus() {
        let mut state = AppState::new_with_config(PathBuf::from("."), None, AppConfig::default());
        state.active_tab = WorkspaceTab::Log;
        state.focused_pane = PaneId::LogCommitTable;
        state.details.diff_zoomed = true;

        let ev_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        let ev_k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        let ev_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
        let ev_upper_g = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);

        assert!(matches!(
            route_dynamic_key(&ev_j, &mut state).as_deref(),
            Some([Action::ScrollDiff(1)])
        ));
        assert!(matches!(
            route_dynamic_key(&ev_k, &mut state).as_deref(),
            Some([Action::ScrollDiff(-1)])
        ));
        assert!(matches!(
            route_dynamic_key(&ev_upper_g, &mut state).as_deref(),
            Some([Action::ScrollDiff(crate::app::SCROLL_GOTO_BOTTOM)])
        ));
        assert!(matches!(
            route_dynamic_key(&ev_g, &mut state).as_deref(),
            Some([])
        ));
        assert!(matches!(
            route_dynamic_key(&ev_g, &mut state).as_deref(),
            Some([Action::ScrollDiff(crate::app::SCROLL_GOTO_TOP)])
        ));

        // Switching away from Log clears diff_zoomed and never hijacks the destination tab.
        state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
        assert!(!state.details.diff_zoomed);
        assert!(matches!(
            route_dynamic_key(&ev_j, &mut state).as_deref(),
            Some([Action::ScrollCommitStage(1)])
        ));

        // Even if diff_zoomed is set directly while on another tab, it must not hijack routing.
        state.details.diff_zoomed = true;
        assert!(matches!(
            route_dynamic_key(&ev_j, &mut state).as_deref(),
            Some([Action::ScrollCommitStage(1)])
        ));
    }
}
