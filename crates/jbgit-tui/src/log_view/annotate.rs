use std::sync::Arc;
use std::time::Instant;

use jbgit_core::{
    BlameBundle, BlameCommit, BlameKey, BlameLine, DetailsKey, DiffContext, FileDiffKey, Oid,
    WhitespaceMode,
};

use crate::app::ScrollViewport;
use crate::diff_session::{DiffCacheStore, DiffSession};
use crate::log_view::FILE_HISTORY_G_WINDOW;
use crate::syntax::HighlightedFile;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotateNavFrame {
    pub key: BlameKey,
    pub cursor: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnotateError {
    TooLarge { limit: usize },
    Binary,
    Message(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AnnotateFocus {
    #[default]
    Code,
    Diff,
}

/// Gutter coloring strategy in the Annotate view.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AnnotateColorMode {
    /// 5-bucket age heatmap (newer = warmer/brighter).
    #[default]
    Age,
    /// Deterministic color per author name.
    Author,
    /// Monochrome / neutral gutter.
    None,
}

impl AnnotateColorMode {
    pub const fn next(self) -> Self {
        match self {
            Self::Age => Self::Author,
            Self::Author => Self::None,
            Self::None => Self::Age,
        }
    }
}

#[derive(Debug)]
pub struct AnnotateState {
    pub key: BlameKey,
    pub bundle: Option<Arc<BlameBundle>>,
    pub syntax: Option<Arc<HighlightedFile>>,
    pub loading: bool,
    pub error: Option<AnnotateError>,
    pub cursor: usize,
    pub visual_anchor: Option<usize>,
    pub scroll: ScrollViewport,
    pub nav_back: Vec<AnnotateNavFrame>,
    pub pending_g: Option<Instant>,
    pub goto_line_input: Option<String>,
    pub pending_focus_line: Option<u32>,
    pub show_diff: bool,
    pub focus: AnnotateFocus,
    pub diff: DiffSession,
    /// Gutter column toggles (independently toggleable).
    pub show_hash: bool,
    pub show_author: bool,
    pub show_date: bool,
    /// Gutter coloring mode (`Age`, `Author`, `None`).
    pub color_mode: AnnotateColorMode,
    /// Full commit message & affected files panel (`i` or clicking detail strip).
    pub show_commit_details: bool,
    pub details_requested: Option<DetailsKey>,
}

impl AnnotateState {
    pub fn new(key: BlameKey, diff_context: DiffContext, side_by_side: bool) -> Self {
        Self::with_diff_prefs(key, diff_context, WhitespaceMode::Default, false, side_by_side)
    }

    pub fn with_diff_prefs(
        key: BlameKey,
        diff_context: DiffContext,
        diff_whitespace: WhitespaceMode,
        diff_soft_wrap: bool,
        side_by_side: bool,
    ) -> Self {
        let mut diff = DiffSession::from_prefs(diff_context, diff_whitespace, diff_soft_wrap);
        diff.side_by_side = side_by_side;
        Self {
            key,
            bundle: None,
            syntax: None,
            loading: true,
            error: None,
            cursor: 0,
            visual_anchor: None,
            scroll: ScrollViewport::new(),
            nav_back: Vec::new(),
            pending_g: None,
            goto_line_input: None,
            pending_focus_line: None,
            show_diff: false,
            focus: AnnotateFocus::Code,
            diff,
            show_hash: true,
            show_author: true,
            show_date: true,
            color_mode: AnnotateColorMode::Age,
            show_commit_details: false,
            details_requested: None,
        }
    }

    /// Applies a loaded (or cache-hit) [`BlameBundle`], resolving any pending
    /// target line and clamping the cursor.
    pub fn apply_loaded_bundle(&mut self, bundle: Arc<BlameBundle>) {
        self.loading = false;
        self.error = None;
        let focus_line = self.pending_focus_line.take();
        let target_cursor = focus_line
            .and_then(|ln| {
                bundle
                    .lines
                    .iter()
                    .position(|l| l.final_line == ln)
                    .or_else(|| usize::try_from(ln.saturating_sub(1)).ok())
            })
            .unwrap_or(self.cursor);
        self.bundle = Some(bundle);
        self.syntax = None;
        self.visual_anchor = None;
        self.cursor = target_cursor.min(self.line_count().saturating_sub(1));
        self.ensure_cursor_visible();
    }

    /// Desired commit details key when the expanded commit details & affected
    /// files panel is open on a committed line.
    pub fn desired_details_key(&self) -> Option<DetailsKey> {
        if !self.show_commit_details {
            return None;
        }
        let line = self.selected_line()?;
        if line.oid.is_zero() {
            return None;
        }
        Some(DetailsKey::single(line.oid))
    }

    pub fn line_count(&self) -> usize {
        self.bundle.as_ref().map_or(0, |b| b.lines.len())
    }

    pub fn selected_line(&self) -> Option<&BlameLine> {
        self.bundle.as_ref()?.lines.get(self.cursor)
    }

    pub fn selected_commit(&self) -> Option<&BlameCommit> {
        let bundle = self.bundle.as_ref()?;
        let line = bundle.lines.get(self.cursor)?;
        bundle.commits.get(&line.oid)
    }

    pub fn selected_visual_bounds(&self) -> Option<(usize, usize)> {
        let len = self.line_count();
        if len == 0 {
            return None;
        }
        let anchor = self.visual_anchor?.min(len - 1);
        let cur = self.cursor.min(len - 1);
        Some((anchor.min(cur), anchor.max(cur)))
    }

    pub fn selected_line_range(&self) -> Option<(u32, u32)> {
        let bundle = self.bundle.as_ref()?;
        if let Some((lo, hi)) = self.selected_visual_bounds() {
            let start = bundle.lines.get(lo)?.final_line;
            let end = bundle.lines.get(hi)?.final_line;
            return Some((start.min(end), start.max(end)));
        }
        let line = self.selected_line()?;
        Some((line.final_line, line.final_line))
    }

    pub fn ensure_cursor_visible(&mut self) {
        let len = self.line_count();
        if len == 0 {
            self.cursor = 0;
            self.scroll.reset();
            return;
        }
        self.cursor = self.cursor.min(len - 1);
        let vh = self.scroll.viewport_height().filter(|&h| h > 0).unwrap_or(24);
        self.scroll.ensure_visible(self.cursor, len, vh);
    }

    pub fn move_cursor(&mut self, delta: isize, extend_visual: bool) {
        let len = self.line_count();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        if extend_visual && self.visual_anchor.is_none() {
            self.visual_anchor = Some(self.cursor);
        }
        self.cursor = (self.cursor as isize + delta).clamp(0, len as isize - 1) as usize;
        self.diff.scroll.reset();
        self.ensure_cursor_visible();
    }

    pub fn select_line(&mut self, idx: usize, extend_visual: bool) {
        let len = self.line_count();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        if extend_visual {
            if self.visual_anchor.is_none() {
                self.visual_anchor = Some(self.cursor);
            }
        } else {
            self.visual_anchor = None;
        }
        self.cursor = idx.min(len - 1);
        self.diff.scroll.reset();
        self.ensure_cursor_visible();
    }

    pub fn toggle_visual(&mut self) {
        if self.line_count() == 0 {
            self.visual_anchor = None;
            return;
        }
        if self.visual_anchor.is_some() {
            self.visual_anchor = None;
        } else {
            self.visual_anchor = Some(self.cursor);
        }
    }

    pub fn clear_visual(&mut self) {
        self.visual_anchor = None;
    }

    pub fn desired_diff_key(&self, head_oid: Option<Oid>) -> Option<FileDiffKey> {
        if !self.show_diff {
            return None;
        }
        let bundle = self.bundle.as_ref()?;
        let line = bundle.lines.get(self.cursor)?;
        if line.oid.is_zero() {
            let head = head_oid?;
            return Some(FileDiffKey {
                details: DetailsKey::compare_with_local(head),
                path: bundle.key.path.clone(),
                old_path: (!line.orig_path.is_empty() && line.orig_path != bundle.key.path)
                    .then(|| line.orig_path.clone()),
                context: self.diff.context,
                whitespace: self.diff.whitespace,
            });
        }
        let commit = bundle.commits.get(&line.oid);
        let path = if line.orig_path.is_empty() {
            bundle.key.path.clone()
        } else {
            line.orig_path.clone()
        };
        let old_path = commit
            .and_then(|c| c.previous.as_ref())
            .map(|(_, prev_path)| prev_path)
            .filter(|prev_path| !prev_path.is_empty() && *prev_path != &path)
            .cloned();
        Some(FileDiffKey {
            details: DetailsKey::single(line.oid),
            path,
            old_path,
            context: self.diff.context,
            whitespace: self.diff.whitespace,
        })
    }

    pub fn commit_age_bucket(&self, oid: Oid, now: i64) -> usize {
        if oid.is_zero() {
            return 0;
        }
        let Some(bundle) = self.bundle.as_ref() else {
            return 4;
        };
        let Some(commit) = bundle.commits.get(&oid) else {
            return 4;
        };
        let prefer_commit = self.key.options.prefer_commit_date;
        let commit_ts = commit.effective_time(prefer_commit);
        let mut min_t = i64::MAX;
        let mut max_t = i64::MIN;
        for c in bundle.commits.values() {
            let ct = c.effective_time(prefer_commit);
            if !c.oid.is_zero() && ct > 0 {
                min_t = min_t.min(ct);
                max_t = max_t.max(ct);
            }
        }
        if min_t < max_t && commit_ts > 0 {
            let span = (max_t - min_t) as f64;
            let age = (max_t - commit_ts).max(0) as f64;
            return ((age / span) * 4.99).floor().clamp(0.0, 4.0) as usize;
        }
        let delta = (now - commit_ts).max(0);
        if delta < 86_400 {
            0
        } else if delta < 7 * 86_400 {
            1
        } else if delta < 30 * 86_400 {
            2
        } else if delta < 180 * 86_400 {
            3
        } else {
            4
        }
    }

    pub fn fallback_diff_max_scroll(&self, head_oid: Option<Oid>, store: &DiffCacheStore) -> usize {
        let diff = self
            .desired_diff_key(head_oid)
            .and_then(|k| store.peek_diff(&k));
        self.diff.fallback_max_scroll(diff)
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        [
            self.diff.next_deadline(),
            self.pending_g.map(|t| t + FILE_HISTORY_G_WINDOW),
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jbgit_core::{BlameOptions, PersonStamp};
    use std::collections::HashMap;

    #[test]
    fn commit_age_bucket_recomputes_from_committer_time_when_prefer_commit_date_flips() {
        let c1 = Oid::from_bytes([1u8; 20]);
        let c2 = Oid::from_bytes([2u8; 20]);
        let mut commits = HashMap::new();
        // c1 was authored earlier (1000) than c2 (2000), but rebased/committed LATER (4000 vs 2000).
        commits.insert(
            c1,
            BlameCommit {
                oid: c1,
                author: PersonStamp {
                    name: "Alice".into(),
                    email: "a@x".into(),
                    time: 1000,
                    offset_minutes: 0,
                },
                committer_time: 4000,
                committer_offset_minutes: 0,
                summary: "rebased older-authored commit".into(),
                previous: None,
                is_boundary: false,
            },
        );
        commits.insert(
            c2,
            BlameCommit {
                oid: c2,
                author: PersonStamp {
                    name: "Bob".into(),
                    email: "b@x".into(),
                    time: 2000,
                    offset_minutes: 0,
                },
                committer_time: 2000,
                committer_offset_minutes: 0,
                summary: "newer-authored commit".into(),
                previous: None,
                is_boundary: false,
            },
        );
        let key = BlameKey {
            path: "src/a.rs".into(),
            rev: None,
            options: BlameOptions::default(),
        };
        let mut ann = AnnotateState::new(key.clone(), DiffContext::Three, false);
        ann.apply_loaded_bundle(Arc::new(BlameBundle {
            key,
            lines: vec![
                BlameLine {
                    final_line: 1,
                    orig_line: 1,
                    oid: c1,
                    orig_path: "src/a.rs".into(),
                },
                BlameLine {
                    final_line: 2,
                    orig_line: 2,
                    oid: c2,
                    orig_path: "src/a.rs".into(),
                },
            ],
            commits,
            content: Arc::from("line1\nline2"),
            uncommitted_oid_present: false,
        }));

        // By author date: c1 (1000) is oldest (bucket 4), c2 (2000) is newest (bucket 0).
        assert_eq!(ann.commit_age_bucket(c1, 5000), 4);
        assert_eq!(ann.commit_age_bucket(c2, 5000), 0);

        // Flip to commit date: c1 (4000) becomes newest (bucket 0), c2 (2000) becomes oldest (bucket 4)!
        ann.key.options.prefer_commit_date = true;
        assert_eq!(ann.commit_age_bucket(c1, 5000), 0);
        assert_eq!(ann.commit_age_bucket(c2, 5000), 4);
    }
}
