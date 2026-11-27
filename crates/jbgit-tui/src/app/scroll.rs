//! Self-clamping bounded scroll viewport shared across all scrollable panes
//! (Log Diff, Log Details, Stash Diff, Stage Diff, File History Diff, Git Console,
//! 3-Way Conflict Resolver, and Commit Message Editor).

use super::apply_scroll_delta;

/// Bounded vertical scroll state that remembers its latest rendered geometry
/// (`viewport_h`, `max_scroll`) and clamps every scroll/resize/cursor update
/// immediately so overscroll dead-zones cannot occur.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScrollViewport {
    offset: usize,
    viewport_h: Option<usize>,
    max_scroll: Option<usize>,
}

impl ScrollViewport {
    pub const fn new() -> Self {
        Self {
            offset: 0,
            viewport_h: None,
            max_scroll: None,
        }
    }

    #[inline]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    #[inline]
    pub const fn viewport_height(&self) -> Option<usize> {
        self.viewport_h
    }

    #[inline]
    pub const fn max_scroll(&self) -> Option<usize> {
        self.max_scroll
    }

    /// Updates the rendered viewport geometry (`viewport_h`, `max_scroll`) and
    /// immediately clamps `offset` within `0..=max_scroll`.
    pub fn bind_bound(&mut self, viewport_h: usize, max_scroll: usize) {
        self.viewport_h = Some(viewport_h);
        self.max_scroll = Some(max_scroll);
        self.offset = self.offset.min(max_scroll);
    }

    /// Updates geometry from `(total_lines, viewport_h)` and clamps `offset`.
    pub fn bind_geometry(&mut self, total_lines: usize, viewport_h: usize) {
        self.bind_bound(viewport_h, total_lines.saturating_sub(viewport_h));
    }

    /// Applies `delta` clamped against known `max_scroll` (or `fallback_max`
    /// before the first rendered frame), returning the new offset.
    pub fn scroll_by(&mut self, delta: isize, fallback_max: usize) -> usize {
        self.scroll_by_with(delta, || fallback_max)
    }

    /// Like [`Self::scroll_by`], but only evaluates `fallback_max` when no
    /// `max_scroll` has been bound yet, so callers can pass expensive
    /// whole-content measurements that are skipped after the first frame.
    pub fn scroll_by_with(&mut self, delta: isize, fallback_max: impl FnOnce() -> usize) -> usize {
        let limit = self.max_scroll.unwrap_or_else(fallback_max);
        self.offset = apply_scroll_delta(self.offset, delta, limit);
        self.offset
    }

    /// Sets `offset` directly, clamping against known `max_scroll` when bound.
    pub fn set_offset(&mut self, offset: usize) {
        self.offset = match self.max_scroll {
            Some(max) => offset.min(max),
            None => offset,
        };
    }

    pub fn reset(&mut self) {
        self.offset = 0;
        self.max_scroll = None;
    }

    /// Adjusts `offset` so `cursor_row` is visible within `viewport_h` (when > 0)
    /// and clamped to `total_lines.saturating_sub(viewport_h)`.
    pub fn ensure_visible(&mut self, cursor_row: usize, total_lines: usize, viewport_h: usize) {
        if viewport_h == 0 {
            self.offset = 0;
            return;
        }
        let max_scroll = total_lines.saturating_sub(viewport_h);
        self.viewport_h = Some(viewport_h);
        self.max_scroll = Some(max_scroll);
        if cursor_row < self.offset {
            self.offset = cursor_row;
        } else if cursor_row >= self.offset + viewport_h {
            self.offset = cursor_row + 1 - viewport_h;
        }
        self.offset = self.offset.min(max_scroll);
    }

    pub fn page_step(&self, fallback: isize) -> isize {
        self.viewport_h
            .map_or(fallback, |h| h.saturating_sub(1).max(1) as isize)
    }

    pub fn half_page_step(&self, fallback: isize) -> isize {
        self.viewport_h
            .map_or(fallback, |h| (h / 2).max(1) as isize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{SCROLL_GOTO_BOTTOM, SCROLL_GOTO_TOP};

    #[test]
    fn scroll_viewport_clamps_short_empty_and_long_contents_with_zero_dead_zone() {
        let mut vp = ScrollViewport::new();
        // Empty content: max_scroll = 0
        vp.bind_geometry(0, 10);
        assert_eq!(vp.scroll_by(50, 99), 0);
        assert_eq!(vp.scroll_by(-10, 99), 0);

        // Short content (< viewport): max_scroll = 0
        vp.bind_geometry(5, 10);
        assert_eq!(vp.scroll_by(SCROLL_GOTO_BOTTOM, 99), 0);

        // Long content (50 lines in 10-line viewport => max_scroll = 40)
        vp.bind_geometry(50, 10);
        assert_eq!(vp.scroll_by(100, 999), 40);
        assert_eq!(vp.scroll_by(100, 999), 40);
        // Pressing Up once immediately moves to 39 (zero dead-zone!)
        assert_eq!(vp.scroll_by(-1, 999), 39);
        assert_eq!(vp.scroll_by(SCROLL_GOTO_TOP, 999), 0);
        assert_eq!(vp.page_step(10), 9);
        assert_eq!(vp.half_page_step(5), 5);
    }

    #[test]
    fn scroll_by_with_skips_fallback_once_max_scroll_is_bound() {
        let mut vp = ScrollViewport::new();
        let mut calls = 0;
        assert_eq!(
            vp.scroll_by_with(5, || {
                calls += 1;
                3
            }),
            3
        );
        assert_eq!(calls, 1);

        vp.bind_geometry(50, 10);
        let offset = vp.scroll_by_with(100, || panic!("fallback must stay lazy"));
        assert_eq!(offset, 40);
    }

    #[test]
    fn scroll_viewport_ensure_visible_tracks_cursor_up_and_down() {
        let mut vp = ScrollViewport::new();
        // 15 lines in 5-line viewport
        for row in 0..15 {
            vp.ensure_visible(row, 15, 5);
            assert!(row >= vp.offset() && row < vp.offset() + 5);
        }
        assert_eq!(vp.offset(), 10);
        for row in (0..15).rev() {
            vp.ensure_visible(row, 15, 5);
            assert!(row >= vp.offset() && row < vp.offset() + 5);
        }
        assert_eq!(vp.offset(), 0);
    }
}
