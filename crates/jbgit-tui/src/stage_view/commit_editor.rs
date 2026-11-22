use unicode_width::UnicodeWidthChar;

use crate::app::ScrollViewport;

const MAX_EDITOR_UNDO: usize = 128;

#[derive(Debug, Clone)]
struct EditorSnapshot {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedEditorLine {
    pub logical_line: usize,
    pub char_start: usize,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct CommitMessageEditor {
    pub lines: Vec<String>,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub scroll_row: usize,
    scroll_vp: ScrollViewport,
    viewport_width: usize,
    viewport_height: usize,
    undo_stack: Vec<EditorSnapshot>,
    redo_stack: Vec<EditorSnapshot>,
}

impl Default for CommitMessageEditor {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
            scroll_row: 0,
            scroll_vp: ScrollViewport::new(),
            viewport_width: 0,
            viewport_height: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }
}

impl CommitMessageEditor {
    /// Binds the latest rendered text-area dimensions and immediately clamps
    /// `scroll_row` so the cursor remains visible inside the viewport.
    pub fn bind_viewport(&mut self, width: usize, height: usize) {
        self.viewport_width = width;
        self.viewport_height = height;
        self.sync_scroll_to_cursor();
    }

    /// Recomputes and clamps `scroll_row` against the last known viewport dimensions
    /// so the visual cursor row always stays within `[scroll_row, scroll_row + viewport_height)`.
    pub fn sync_scroll_to_cursor(&mut self) {
        if self.viewport_height == 0 {
            return;
        }
        let width = self.viewport_width.max(1);
        let (wrapped, (cur_row, _)) = self.visual_wrapped_lines(width);
        self.scroll_vp
            .ensure_visible(cur_row, wrapped.len(), self.viewport_height);
        self.scroll_row = self.scroll_vp.offset();
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn set_text(&mut self, text: &str) {
        self.push_undo();
        let norm = text.replace("\r\n", "\n");
        self.lines = if norm.is_empty() {
            vec![String::new()]
        } else {
            norm.split('\n').map(ToString::to_string).collect()
        };
        self.cursor_line = self.lines.len().saturating_sub(1);
        self.cursor_col = self.lines[self.cursor_line].chars().count();
        self.scroll_vp.reset();
        self.scroll_row = 0;
        self.sync_scroll_to_cursor();
    }

    pub fn clear(&mut self) {
        if !self.is_empty() {
            self.push_undo();
        }
        self.lines = vec![String::new()];
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.scroll_vp.reset();
        self.scroll_row = 0;
    }

    fn push_undo(&mut self) {
        if self.undo_stack.len() >= MAX_EDITOR_UNDO {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(EditorSnapshot {
            lines: self.lines.clone(),
            cursor_line: self.cursor_line,
            cursor_col: self.cursor_col,
        });
        self.redo_stack.clear();
    }

    pub fn undo(&mut self) -> bool {
        let Some(snap) = self.undo_stack.pop() else {
            return false;
        };
        self.redo_stack.push(EditorSnapshot {
            lines: self.lines.clone(),
            cursor_line: self.cursor_line,
            cursor_col: self.cursor_col,
        });
        self.lines = snap.lines;
        self.cursor_line = snap.cursor_line.min(self.lines.len().saturating_sub(1));
        self.cursor_col = snap
            .cursor_col
            .min(self.lines[self.cursor_line].chars().count());
        self.sync_scroll_to_cursor();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(snap) = self.redo_stack.pop() else {
            return false;
        };
        self.undo_stack.push(EditorSnapshot {
            lines: self.lines.clone(),
            cursor_line: self.cursor_line,
            cursor_col: self.cursor_col,
        });
        self.lines = snap.lines;
        self.cursor_line = snap.cursor_line.min(self.lines.len().saturating_sub(1));
        self.cursor_col = snap
            .cursor_col
            .min(self.lines[self.cursor_line].chars().count());
        self.sync_scroll_to_cursor();
        true
    }

    fn byte_idx_at(s: &str, char_col: usize) -> usize {
        s.char_indices()
            .nth(char_col)
            .map_or(s.len(), |(idx, _)| idx)
    }

    pub fn insert_char(&mut self, ch: char) {
        self.push_undo();
        let line = &mut self.lines[self.cursor_line];
        let b = Self::byte_idx_at(line, self.cursor_col);
        line.insert(b, ch);
        self.cursor_col += 1;
        self.sync_scroll_to_cursor();
    }

    pub fn insert_newline(&mut self) {
        self.push_undo();
        let line = &mut self.lines[self.cursor_line];
        let b = Self::byte_idx_at(line, self.cursor_col);
        let tail = line.split_off(b);
        self.cursor_line += 1;
        self.lines.insert(self.cursor_line, tail);
        self.cursor_col = 0;
        self.sync_scroll_to_cursor();
    }

    pub fn insert_paste(&mut self, pasted: &str) {
        if pasted.is_empty() {
            return;
        }
        self.push_undo();
        let norm = pasted.replace("\r\n", "\n").replace('\r', "\n");
        let mut parts = norm.split('\n');
        let first = parts.next().unwrap_or("");
        let tail_after = {
            let line = &mut self.lines[self.cursor_line];
            let b = Self::byte_idx_at(line, self.cursor_col);
            let tail = line.split_off(b);
            line.push_str(first);
            tail
        };
        let rest: Vec<&str> = parts.collect();
        if rest.is_empty() {
            self.cursor_col += first.chars().count();
            self.lines[self.cursor_line].push_str(&tail_after);
        } else {
            for (idx, part) in rest.iter().enumerate() {
                self.cursor_line += 1;
                let mut s = (*part).to_string();
                if idx + 1 == rest.len() {
                    self.cursor_col = part.chars().count();
                    s.push_str(&tail_after);
                }
                self.lines.insert(self.cursor_line, s);
            }
        }
        self.sync_scroll_to_cursor();
    }

    pub fn backspace(&mut self) {
        if self.cursor_line == 0 && self.cursor_col == 0 {
            return;
        }
        self.push_undo();
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_line];
            let start = Self::byte_idx_at(line, self.cursor_col - 1);
            let end = Self::byte_idx_at(line, self.cursor_col);
            line.replace_range(start..end, "");
            self.cursor_col -= 1;
        } else {
            let removed = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].chars().count();
            self.lines[self.cursor_line].push_str(&removed);
        }
        self.sync_scroll_to_cursor();
    }

    pub fn delete_forward(&mut self) {
        let len = self.lines[self.cursor_line].chars().count();
        if self.cursor_col < len {
            self.push_undo();
            let line = &mut self.lines[self.cursor_line];
            let start = Self::byte_idx_at(line, self.cursor_col);
            let end = Self::byte_idx_at(line, self.cursor_col + 1);
            line.replace_range(start..end, "");
        } else if self.cursor_line + 1 < self.lines.len() {
            self.push_undo();
            let next = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&next);
        }
        self.sync_scroll_to_cursor();
    }

    pub fn delete_word_backward(&mut self) {
        if self.cursor_col == 0 {
            self.backspace();
            return;
        }
        self.push_undo();
        let line = &mut self.lines[self.cursor_line];
        let chars: Vec<char> = line.chars().collect();
        let mut new_col = self.cursor_col;
        while new_col > 0 && chars[new_col - 1].is_whitespace() {
            new_col -= 1;
        }
        while new_col > 0 && !chars[new_col - 1].is_whitespace() {
            new_col -= 1;
        }
        let start = Self::byte_idx_at(line, new_col);
        let end = Self::byte_idx_at(line, self.cursor_col);
        line.replace_range(start..end, "");
        self.cursor_col = new_col;
        self.sync_scroll_to_cursor();
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].chars().count();
        }
        self.sync_scroll_to_cursor();
    }

    pub fn move_right(&mut self) {
        let len = self.lines[self.cursor_line].chars().count();
        if self.cursor_col < len {
            self.cursor_col += 1;
        } else if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = 0;
        }
        self.sync_scroll_to_cursor();
    }

    pub fn move_up(&mut self) {
        if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_line].chars().count());
        }
        self.sync_scroll_to_cursor();
    }

    pub fn move_down(&mut self) {
        if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_line].chars().count());
        }
        self.sync_scroll_to_cursor();
    }

    pub fn move_home(&mut self) {
        self.cursor_col = 0;
        self.sync_scroll_to_cursor();
    }

    pub fn move_end(&mut self) {
        self.cursor_col = self.lines[self.cursor_line].chars().count();
        self.sync_scroll_to_cursor();
    }

    pub fn subject_char_count(&self) -> usize {
        self.lines.first().map_or(0, |l| l.chars().count())
    }

    pub fn needs_blank_second_line_hint(&self) -> bool {
        self.lines.len() >= 2 && !self.lines[1].trim().is_empty()
    }

    /// Soft-wraps logical lines to `width` terminal columns and computes visual `(row, col)` cursor.
    pub fn visual_wrapped_lines(&self, width: usize) -> (Vec<WrappedEditorLine>, (usize, usize)) {
        let width = width.max(1);
        let mut out = Vec::new();
        let mut cursor_vis = (0usize, 0usize);

        for (l_idx, line) in self.lines.iter().enumerate() {
            if line.is_empty() {
                if l_idx == self.cursor_line {
                    cursor_vis = (out.len(), 0);
                }
                out.push(WrappedEditorLine {
                    logical_line: l_idx,
                    char_start: 0,
                    text: String::new(),
                });
                continue;
            }

            let mut seg = String::new();
            let mut seg_w = 0usize;
            let mut seg_start = 0usize;
            let mut placed_cursor = false;

            for (c_idx, ch) in line.chars().enumerate() {
                let cw = ch.width().unwrap_or(0).max(1);
                if seg_w + cw > width && !seg.is_empty() {
                    if l_idx == self.cursor_line
                        && !placed_cursor
                        && self.cursor_col >= seg_start
                        && self.cursor_col < c_idx
                    {
                        let prefix_w: usize = seg
                            .chars()
                            .take(self.cursor_col - seg_start)
                            .map(|c| c.width().unwrap_or(0))
                            .sum();
                        cursor_vis = (out.len(), prefix_w);
                        placed_cursor = true;
                    }
                    out.push(WrappedEditorLine {
                        logical_line: l_idx,
                        char_start: seg_start,
                        text: std::mem::take(&mut seg),
                    });
                    seg_start = c_idx;
                    seg_w = 0;
                }
                seg.push(ch);
                seg_w += cw;
            }

            if l_idx == self.cursor_line && !placed_cursor {
                let offset = self.cursor_col.saturating_sub(seg_start);
                let prefix_w: usize = seg
                    .chars()
                    .take(offset)
                    .map(|c| c.width().unwrap_or(0))
                    .sum();
                cursor_vis = (out.len(), prefix_w.min(width));
            }
            out.push(WrappedEditorLine {
                logical_line: l_idx,
                char_start: seg_start,
                text: seg,
            });
        }

        (out, cursor_vis)
    }
}
