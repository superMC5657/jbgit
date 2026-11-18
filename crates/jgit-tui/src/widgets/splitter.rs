use jgit_core::{SplitAxis, SplitterId};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use rustc_hash::FxHashMap;

use crate::hit_test::{HitTarget, HitTestRegistry};
use crate::theme::ThemePalette;

#[derive(Debug, Clone, Copy)]
pub struct SplitterSpec {
    pub default_ratio: f32,
    pub min_first: u16,
    pub min_second: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct SplitterLayout {
    pub container: Rect,
    pub first: Rect,
    pub divider: Rect,
    pub second: Rect,
    pub collapsed: bool,
}

#[derive(Debug, Clone)]
pub struct SplitterState {
    ratios: FxHashMap<SplitterId, f32>,
    specs: FxHashMap<SplitterId, SplitterSpec>,
}

impl Default for SplitterState {
    fn default() -> Self {
        let mut specs = FxHashMap::default();
        specs.insert(
            SplitterId::CommitMainHorizontal,
            SplitterSpec {
                default_ratio: 0.35,
                min_first: 22,
                min_second: 24,
            },
        );
        specs.insert(
            SplitterId::CommitLeftVertical,
            SplitterSpec {
                default_ratio: 0.62,
                min_first: 6,
                min_second: 5,
            },
        );
        specs.insert(
            SplitterId::LogLeftHorizontal,
            SplitterSpec {
                default_ratio: 0.20,
                min_first: 16,
                min_second: 36,
            },
        );
        specs.insert(
            SplitterId::LogRightHorizontal,
            SplitterSpec {
                default_ratio: 0.62,
                min_first: 24,
                min_second: 20,
            },
        );
        specs.insert(
            SplitterId::LogDetailsVertical,
            SplitterSpec {
                default_ratio: 0.62,
                min_first: 4,
                min_second: 4,
            },
        );
        specs.insert(
            SplitterId::LogDiffRightHorizontal,
            SplitterSpec {
                default_ratio: 0.65,
                min_first: 40,
                min_second: 24,
            },
        );
        specs.insert(
            SplitterId::LogDiffBottomVertical,
            SplitterSpec {
                default_ratio: 0.60,
                min_first: 8,
                min_second: 6,
            },
        );
        specs.insert(
            SplitterId::StashMainHorizontal,
            SplitterSpec {
                default_ratio: 0.36,
                min_first: 20,
                min_second: 24,
            },
        );
        specs.insert(
            SplitterId::ConsoleMainHorizontal,
            SplitterSpec {
                default_ratio: 0.42,
                min_first: 24,
                min_second: 24,
            },
        );

        let ratios = specs.iter().map(|(&id, s)| (id, s.default_ratio)).collect();
        Self { ratios, specs }
    }
}

impl SplitterState {
    pub fn new_with_layout(layout: &crate::config::jgit_config::LayoutSection) -> Self {
        let mut state = Self::default();
        state.apply_layout_config(layout);
        state
    }

    pub fn apply_layout_config(&mut self, layout: &crate::config::jgit_config::LayoutSection) {
        // log_ratio: [branches, commits, details] (len >= 3) or [branches, commits] (len == 2)
        if layout.log_ratio.len() >= 3 {
            let a = layout.log_ratio[0] as f32;
            let b = layout.log_ratio[1] as f32;
            let c = layout.log_ratio[2] as f32;
            let total = a + b + c;
            if total > 0.0 {
                let r_left = (a / total).clamp(0.05, 0.90);
                self.ratios.insert(SplitterId::LogLeftHorizontal, r_left);
                if let Some(spec) = self.specs.get_mut(&SplitterId::LogLeftHorizontal) {
                    spec.default_ratio = r_left;
                }
                if b + c > 0.0 {
                    let r_right = (b / (b + c)).clamp(0.05, 0.90);
                    self.ratios.insert(SplitterId::LogRightHorizontal, r_right);
                    if let Some(spec) = self.specs.get_mut(&SplitterId::LogRightHorizontal) {
                        spec.default_ratio = r_right;
                    }
                }
            }
        } else if layout.log_ratio.len() == 2 {
            let a = layout.log_ratio[0] as f32;
            let b = layout.log_ratio[1] as f32;
            let total = a + b;
            if total > 0.0 {
                let r = (a / total).clamp(0.05, 0.90);
                self.ratios.insert(SplitterId::LogLeftHorizontal, r);
                if let Some(spec) = self.specs.get_mut(&SplitterId::LogLeftHorizontal) {
                    spec.default_ratio = r;
                }
            }
        }

        // commit_ratio: [changes, diff]
        if layout.commit_ratio.len() >= 2 {
            let a = layout.commit_ratio[0] as f32;
            let b = layout.commit_ratio[1] as f32;
            let total = a + b;
            if total > 0.0 {
                let r = (a / total).clamp(0.05, 0.90);
                self.ratios.insert(SplitterId::CommitMainHorizontal, r);
                if let Some(spec) = self.specs.get_mut(&SplitterId::CommitMainHorizontal) {
                    spec.default_ratio = r;
                }
            }
        }

        // stash_ratio: [stash_list, diff]
        if layout.stash_ratio.len() >= 2 {
            let a = layout.stash_ratio[0] as f32;
            let b = layout.stash_ratio[1] as f32;
            let total = a + b;
            if total > 0.0 {
                let r = (a / total).clamp(0.05, 0.90);
                self.ratios.insert(SplitterId::StashMainHorizontal, r);
                if let Some(spec) = self.specs.get_mut(&SplitterId::StashMainHorizontal) {
                    spec.default_ratio = r;
                }
            }
        }

        // console_ratio: [commands, output]
        if layout.console_ratio.len() >= 2 {
            let a = layout.console_ratio[0] as f32;
            let b = layout.console_ratio[1] as f32;
            let total = a + b;
            if total > 0.0 {
                let r = (a / total).clamp(0.05, 0.90);
                self.ratios.insert(SplitterId::ConsoleMainHorizontal, r);
                if let Some(spec) = self.specs.get_mut(&SplitterId::ConsoleMainHorizontal) {
                    spec.default_ratio = r;
                }
            }
        }
    }

    pub fn ratio(&self, id: SplitterId) -> f32 {
        self.ratios.get(&id).copied().unwrap_or(0.5)
    }

    pub fn spec(&self, id: SplitterId) -> SplitterSpec {
        self.specs.get(&id).copied().unwrap_or(SplitterSpec {
            default_ratio: 0.5,
            min_first: 10,
            min_second: 10,
        })
    }

    pub fn reset(&mut self, id: SplitterId) {
        let def = self.spec(id).default_ratio;
        self.ratios.insert(id, def);
    }

    pub fn split(&self, area: Rect, id: SplitterId, axis: SplitAxis) -> SplitterLayout {
        let spec = self.spec(id);
        let ratio = self.ratio(id);
        OneCellSplitter::compute_layout(area, axis, ratio, spec.min_first, spec.min_second)
    }

    pub fn update_from_pointer(
        &mut self,
        id: SplitterId,
        axis: SplitAxis,
        container: Rect,
        col: u16,
        row: u16,
    ) {
        let spec = self.spec(id);
        let total = match axis {
            SplitAxis::Horizontal => container.width,
            SplitAxis::Vertical => container.height,
        };
        if total <= 1 {
            return;
        }
        let min_total = spec
            .min_first
            .saturating_add(1)
            .saturating_add(spec.min_second);
        if total < min_total {
            return;
        }

        let offset = match axis {
            SplitAxis::Horizontal => col.saturating_sub(container.x),
            SplitAxis::Vertical => row.saturating_sub(container.y),
        };
        let clamped = offset.clamp(
            spec.min_first,
            total.saturating_sub(1).saturating_sub(spec.min_second),
        );
        let new_ratio = (clamped as f32) / (total as f32);
        self.ratios.insert(id, new_ratio.clamp(0.05, 0.95));
    }
}

pub struct OneCellSplitter;

impl OneCellSplitter {
    /// Computes saturating 1-cell split rectangles; collapses secondary pane
    /// when `area` is too small to satisfy `min_first + 1 + min_second`.
    pub fn compute_layout(
        area: Rect,
        axis: SplitAxis,
        ratio: f32,
        min_first: u16,
        min_second: u16,
    ) -> SplitterLayout {
        if area.width == 0 || area.height == 0 {
            return SplitterLayout {
                container: area,
                first: Rect::ZERO,
                divider: Rect::ZERO,
                second: Rect::ZERO,
                collapsed: true,
            };
        }

        let total = match axis {
            SplitAxis::Horizontal => area.width,
            SplitAxis::Vertical => area.height,
        };

        let required = min_first.saturating_add(1).saturating_add(min_second);
        if total < required {
            return SplitterLayout {
                container: area,
                first: area,
                divider: Rect::ZERO,
                second: Rect::ZERO,
                collapsed: true,
            };
        }

        let max_first = total.saturating_sub(1).saturating_sub(min_second);
        let desired = ((total as f32) * ratio).round() as u16;
        let first_len = desired.clamp(min_first, max_first);
        let second_len = total.saturating_sub(first_len).saturating_sub(1);

        match axis {
            SplitAxis::Horizontal => {
                let first = Rect::new(area.x, area.y, first_len, area.height);
                let divider = Rect::new(area.x.saturating_add(first_len), area.y, 1, area.height);
                let second = Rect::new(
                    area.x.saturating_add(first_len).saturating_add(1),
                    area.y,
                    second_len,
                    area.height,
                );
                SplitterLayout {
                    container: area,
                    first,
                    divider,
                    second,
                    collapsed: false,
                }
            }
            SplitAxis::Vertical => {
                let first = Rect::new(area.x, area.y, area.width, first_len);
                let divider = Rect::new(area.x, area.y.saturating_add(first_len), area.width, 1);
                let second = Rect::new(
                    area.x,
                    area.y.saturating_add(first_len).saturating_add(1),
                    area.width,
                    second_len,
                );
                SplitterLayout {
                    container: area,
                    first,
                    divider,
                    second,
                    collapsed: false,
                }
            }
        }
    }

    pub fn render_divider(
        frame: &mut Frame<'_>,
        divider: Rect,
        id: SplitterId,
        axis: SplitAxis,
        active: bool,
        theme: &ThemePalette,
        hit_registry: &mut HitTestRegistry,
    ) {
        if divider.width == 0 || divider.height == 0 {
            return;
        }

        let fg = if active {
            theme.border_focused
        } else {
            theme.border_splitter
        };
        let style = Style::default().fg(fg).bg(theme.bg_canvas);
        let ch = match axis {
            SplitAxis::Horizontal => "│",
            SplitAxis::Vertical => "─",
        };

        let buf = frame.buffer_mut();
        for y in divider.y..divider.y.saturating_add(divider.height) {
            for x in divider.x..divider.x.saturating_add(divider.width) {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_symbol(ch).set_style(style);
                }
            }
        }

        hit_registry.register(divider, 2, HitTarget::Splitter { id, axis });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saturating_layout_never_panics_on_tiny_terminal() {
        for w in 0..70u16 {
            for h in 0..20u16 {
                let area = Rect::new(0, 0, w, h);
                let l1 = OneCellSplitter::compute_layout(area, SplitAxis::Horizontal, 0.4, 20, 20);
                assert!(l1.first.width <= w);
                let l2 = OneCellSplitter::compute_layout(area, SplitAxis::Vertical, 0.5, 6, 5);
                assert!(l2.first.height <= h);
            }
        }
    }

    #[test]
    fn custom_layout_ratios_apply_to_splitters() {
        use crate::config::jgit_config::LayoutSection;

        let layout = LayoutSection {
            log_ratio: vec![1, 4, 3], // total 8 -> left: 1/8 = 0.125, right: 4/7 ≈ 0.5714
            commit_ratio: vec![1, 3],  // total 4 -> 1/4 = 0.25
            stash_ratio: vec![2, 3],   // total 5 -> 2/5 = 0.40
            console_ratio: vec![1, 1], // total 2 -> 1/2 = 0.50
        };

        let mut state = SplitterState::new_with_layout(&layout);

        let r_log_left = state.ratio(SplitterId::LogLeftHorizontal);
        assert!((r_log_left - 0.125).abs() < 1e-4);

        let r_log_right = state.ratio(SplitterId::LogRightHorizontal);
        assert!((r_log_right - (4.0 / 7.0)).abs() < 1e-4);

        let r_commit = state.ratio(SplitterId::CommitMainHorizontal);
        assert!((r_commit - 0.25).abs() < 1e-4);

        let r_stash = state.ratio(SplitterId::StashMainHorizontal);
        assert!((r_stash - 0.40).abs() < 1e-4);

        let r_console = state.ratio(SplitterId::ConsoleMainHorizontal);
        assert!((r_console - 0.50).abs() < 1e-4);

        // Test reset restores configured ratio
        state.ratios.insert(SplitterId::CommitMainHorizontal, 0.88);
        assert_eq!(state.ratio(SplitterId::CommitMainHorizontal), 0.88);
        state.reset(SplitterId::CommitMainHorizontal);
        assert!((state.ratio(SplitterId::CommitMainHorizontal) - 0.25).abs() < 1e-4);
    }
}
