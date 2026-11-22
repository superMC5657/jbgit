//! Self-contained `[4 Console]` workbench module encapsulating command history
//! entries, selection state, and a self-clamping [`ScrollViewport`] for output.

use jbgit_core::{ConsoleStream, GitConsoleEntry};

use crate::app::{Action, ScrollViewport};

#[derive(Debug, Default)]
pub struct ConsoleWorkbench {
    entries: Vec<GitConsoleEntry>,
    selected_idx: usize,
    output_scroll: ScrollViewport,
}

impl ConsoleWorkbench {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entries(&self) -> &[GitConsoleEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn selected_idx(&self) -> usize {
        self.selected_idx
    }

    pub fn selected_entry(&self) -> Option<&GitConsoleEntry> {
        self.entries.get(self.selected_idx)
    }

    pub fn output_viewport(&self) -> &ScrollViewport {
        &self.output_scroll
    }

    pub fn output_scroll(&self) -> usize {
        self.output_scroll.offset()
    }

    pub fn has_running(&self) -> bool {
        self.entries.iter().any(GitConsoleEntry::is_running)
    }

    pub fn push_started(&mut self, entry: GitConsoleEntry) {
        self.entries.insert(0, entry);
        self.selected_idx = 0;
        self.output_scroll.reset();
    }

    pub fn push_output(&mut self, id: u64, stream: ConsoleStream, line: String) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.push_line(stream, line);
        }
    }

    pub fn finish_command(&mut self, id: u64, exit_code: i32, duration_ms: u64) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.exit_code = Some(exit_code);
            entry.duration_ms = Some(duration_ms);
        }
    }

    pub fn select_row(&mut self, idx: usize) {
        if !self.entries.is_empty() {
            self.selected_idx = idx.min(self.entries.len() - 1);
            self.output_scroll.reset();
        }
    }

    pub fn scroll_list(&mut self, delta: isize) {
        let len = self.entries.len();
        if len > 0 {
            let next = (self.selected_idx as isize + delta).clamp(0, len as isize - 1) as usize;
            if next != self.selected_idx {
                self.selected_idx = next;
                self.output_scroll.reset();
            }
        }
    }

    pub fn bind_output_bound(&mut self, viewport_h: usize, max_scroll: usize) {
        self.output_scroll.bind_bound(viewport_h, max_scroll);
    }

    pub fn output_max_scroll(&self) -> usize {
        let Some(entry) = self.selected_entry() else {
            return 0;
        };
        let total_lines = crate::views::console::console_entry_total_lines(entry);
        match self.output_scroll.viewport_height() {
            Some(vh) if vh > 0 => total_lines.saturating_sub(vh),
            _ => total_lines.saturating_sub(1),
        }
    }

    pub fn scroll_output(&mut self, delta: isize) {
        let max_scroll = self.output_max_scroll();
        if let Some(vh) = self.output_scroll.viewport_height() {
            self.output_scroll.bind_bound(vh, max_scroll);
        }
        self.output_scroll.scroll_by(delta, max_scroll);
    }

    pub fn page_step(&self) -> isize {
        self.output_scroll.page_step(10)
    }

    pub fn half_page_step(&self) -> isize {
        self.output_scroll.half_page_step(10)
    }

    pub fn dispatch(&mut self, action: &Action) -> bool {
        match action {
            Action::SelectConsoleRow(idx) => self.select_row(*idx),
            Action::ScrollConsoleList(delta) => self.scroll_list(*delta),
            Action::ScrollConsoleOutput(delta) => self.scroll_output(*delta),
            _ => return false,
        }
        true
    }

    #[cfg(test)]
    pub fn set_output_scroll_for_test(&mut self, offset: usize) {
        self.output_scroll.set_offset(offset);
    }

    #[cfg(test)]
    pub fn entries_mut_for_test(&mut self) -> &mut Vec<GitConsoleEntry> {
        &mut self.entries
    }
}
