use std::sync::Arc;
use std::time::Instant;

use jbgit_core::{
    BlameBundle, BlameCommit, BlameKey, BlameLine, DetailsKey, DiffContext, FileDiffKey, Oid,
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
}

impl AnnotateState {
    pub fn new(key: BlameKey, diff_context: DiffContext, side_by_side: bool) -> Self {
        let mut diff = DiffSession::new(diff_context);
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
        }
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
        let mut min_t = i64::MAX;
        let mut max_t = i64::MIN;
        for c in bundle.commits.values() {
            if !c.oid.is_zero() && c.author.time > 0 {
                min_t = min_t.min(c.author.time);
                max_t = max_t.max(c.author.time);
            }
        }
        if min_t < max_t && commit.author.time > 0 {
            let span = (max_t - min_t) as f64;
            let age = (max_t - commit.author.time).max(0) as f64;
            return ((age / span) * 4.99).floor().clamp(0.0, 4.0) as usize;
        }
        let delta = (now - commit.author.time).max(0);
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
