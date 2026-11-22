//! Right pane: `CommitChangesBrowser` (top), `CommitDetailsPanel` or inline
//! diff preview (bottom), and the full-screen zoomed diff.

mod changes_browser;
mod commit_details;
mod diff_view;

use ratatui::style::Style;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{RenderContext, ScrollPane};

pub use changes_browser::{ChangesBrowserProps, render_changes_browser, render_changes_pane};
pub(crate) use diff_view::{
    DiffLineRenderInput, diff_line_style, render_diff_side_cell, render_diff_unified_spans,
};
pub use diff_view::{
    DiffPreviewProps, FileDiffRenderProps, render_details_or_diff, render_diff_preview,
    render_docked_diff, render_file_diff_body, render_zoomed_diff,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffHost {
    Log,
    Stash,
}

impl DiffHost {
    pub(super) const fn scroll_pane(self) -> ScrollPane {
        match self {
            Self::Log => ScrollPane::LogDiff,
            Self::Stash => ScrollPane::StashDiff,
        }
    }
}

pub(super) fn muted_line(ctx: &RenderContext<'_>, text: &str) -> Paragraph<'static> {
    Paragraph::new(Span::styled(
        text.to_string(),
        Style::default().fg(ctx.theme.fg_muted),
    ))
}

pub(super) fn pad_label(s: &str, width: usize) -> String {
    format!(" {s}{}", " ".repeat(width.saturating_sub(s.width())))
}
