//! Pure domain model for built-in three-pane conflict resolution:
//! parses Git conflict markers (`<<<<<<<` / `|||||||` / `=======` / `>>>>>>>`)
//! or falls back to stage 2 (Ours) / stage 3 (Theirs) index blobs, tracks
//! per-hunk resolution choices (`Ours` / `Theirs` / `Both`), and renders both
//! the merged output and aligned three-pane display rows.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    Unresolved,
    Ours,
    Theirs,
    BothOursFirst,
    BothTheirsFirst,
}

impl ConflictChoice {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unresolved => "Unresolved",
            Self::Ours => "Ours",
            Self::Theirs => "Theirs",
            Self::BothOursFirst => "Both (Ours + Theirs)",
            Self::BothTheirsFirst => "Both (Theirs + Ours)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictSegment {
    Common(Vec<String>),
    Conflict(ConflictBlock),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictBlock {
    pub id: usize,
    pub ours_label: String,
    pub theirs_label: String,
    pub ours_lines: Vec<String>,
    pub base_lines: Vec<String>,
    pub theirs_lines: Vec<String>,
    pub choice: ConflictChoice,
}

impl ConflictBlock {
    pub fn resolved_lines(&self) -> Option<Vec<String>> {
        match self.choice {
            ConflictChoice::Unresolved => None,
            ConflictChoice::Ours => Some(self.ours_lines.clone()),
            ConflictChoice::Theirs => Some(self.theirs_lines.clone()),
            ConflictChoice::BothOursFirst => {
                let mut out = self.ours_lines.clone();
                out.extend(self.theirs_lines.iter().cloned());
                Some(out)
            }
            ConflictChoice::BothTheirsFirst => {
                let mut out = self.theirs_lines.clone();
                out.extend(self.ours_lines.iter().cloned());
                Some(out)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictPaneRow {
    pub block_idx: Option<usize>,
    pub ours: String,
    pub merged: String,
    pub theirs: String,
    pub choice: Option<ConflictChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictDocument {
    pub path: String,
    pub segments: Vec<ConflictSegment>,
    pub ends_with_newline: bool,
    pub conflict_count: usize,
}

impl ConflictDocument {
    /// Parses conflict markers from `worktree_text`, or falls back to
    /// `index_ours` / `index_theirs` when no markers are present (e.g.
    /// add/add or delete/modify conflicts).
    pub fn parse(
        path: impl Into<String>,
        worktree_text: &str,
        index_ours: Option<&str>,
        index_theirs: Option<&str>,
    ) -> Self {
        let path = path.into();
        let ends_with_newline = worktree_text.ends_with('\n');
        let raw_lines: Vec<String> = worktree_text
            .lines()
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect();

        let mut segments = Vec::new();
        let mut common = Vec::new();
        let mut i = 0usize;
        let mut conflict_id = 0usize;

        while i < raw_lines.len() {
            if let Some(rest) = raw_lines[i].strip_prefix("<<<<<<<") {
                let ours_label = rest.trim().to_string();
                let start = i;
                i += 1;
                let mut ours_lines = Vec::new();
                let mut base_lines = Vec::new();
                let mut theirs_lines = Vec::new();
                let mut section = 0u8; // 0 = ours, 1 = base, 2 = theirs
                let mut closed = None;
                while i < raw_lines.len() {
                    if raw_lines[i].starts_with("|||||||") && section == 0 {
                        section = 1;
                    } else if raw_lines[i].starts_with("=======") && section < 2 {
                        section = 2;
                    } else if let Some(trest) = raw_lines[i].strip_prefix(">>>>>>>")
                        && section == 2
                    {
                        closed = Some(trest.trim().to_string());
                        i += 1;
                        break;
                    } else {
                        match section {
                            0 => ours_lines.push(raw_lines[i].clone()),
                            1 => base_lines.push(raw_lines[i].clone()),
                            _ => theirs_lines.push(raw_lines[i].clone()),
                        }
                    }
                    i += 1;
                }
                if let Some(theirs_label) = closed {
                    if !common.is_empty() {
                        segments.push(ConflictSegment::Common(std::mem::take(&mut common)));
                    }
                    segments.push(ConflictSegment::Conflict(ConflictBlock {
                        id: conflict_id,
                        ours_label: if ours_label.is_empty() {
                            "HEAD".into()
                        } else {
                            ours_label
                        },
                        theirs_label: if theirs_label.is_empty() {
                            "Incoming".into()
                        } else {
                            theirs_label
                        },
                        ours_lines,
                        base_lines,
                        theirs_lines,
                        choice: ConflictChoice::Unresolved,
                    }));
                    conflict_id += 1;
                    continue;
                } else {
                    // Malformed / unclosed marker: treat as normal text.
                    common.extend(raw_lines[start..i].iter().cloned());
                    continue;
                }
            }
            common.push(raw_lines[i].clone());
            i += 1;
        }
        if !common.is_empty() {
            segments.push(ConflictSegment::Common(common));
        }

        // If no inline markers were found but index stages differ, synthesize one
        // conflict block from index stage 2 (Ours) and stage 3 (Theirs).
        if conflict_id == 0 && (index_ours.is_some() || index_theirs.is_some()) {
            let split_lines = |s: Option<&str>| -> Vec<String> {
                s.map(|t| {
                    t.lines()
                        .map(|l| l.trim_end_matches('\r').to_string())
                        .collect()
                })
                .unwrap_or_default()
            };
            segments.clear();
            segments.push(ConflictSegment::Conflict(ConflictBlock {
                id: 0,
                ours_label: "Ours".into(),
                theirs_label: "Theirs".into(),
                ours_lines: split_lines(index_ours),
                base_lines: Vec::new(),
                theirs_lines: split_lines(index_theirs),
                choice: ConflictChoice::Unresolved,
            }));
            conflict_id = 1;
        }

        Self {
            path,
            segments,
            ends_with_newline: ends_with_newline || worktree_text.is_empty(),
            conflict_count: conflict_id,
        }
    }

    pub fn unresolved_count(&self) -> usize {
        self.segments
            .iter()
            .filter(|s| {
                matches!(
                    s,
                    ConflictSegment::Conflict(b) if b.choice == ConflictChoice::Unresolved
                )
            })
            .count()
    }

    pub fn block(&self, idx: usize) -> Option<&ConflictBlock> {
        self.segments.iter().find_map(|s| match s {
            ConflictSegment::Conflict(b) if b.id == idx => Some(b),
            _ => None,
        })
    }

    pub fn set_choice(&mut self, block_idx: usize, choice: ConflictChoice) {
        for seg in &mut self.segments {
            if let ConflictSegment::Conflict(b) = seg
                && b.id == block_idx
            {
                b.choice = choice;
                break;
            }
        }
    }

    pub fn accept_all(&mut self, choice: ConflictChoice) {
        for seg in &mut self.segments {
            if let ConflictSegment::Conflict(b) = seg {
                b.choice = choice;
            }
        }
    }

    /// Renders the merged file contents; any still-unresolved block retains its
    /// standard Git conflict markers so nothing is silently lost.
    pub fn render_merged(&self) -> String {
        let mut lines = Vec::new();
        for seg in &self.segments {
            match seg {
                ConflictSegment::Common(ls) => lines.extend(ls.iter().cloned()),
                ConflictSegment::Conflict(b) => match b.resolved_lines() {
                    Some(resolved) => lines.extend(resolved),
                    None => {
                        lines.push(format!("<<<<<<< {}", b.ours_label));
                        lines.extend(b.ours_lines.iter().cloned());
                        if !b.base_lines.is_empty() {
                            lines.push("||||||| base".to_string());
                            lines.extend(b.base_lines.iter().cloned());
                        }
                        lines.push("=======".to_string());
                        lines.extend(b.theirs_lines.iter().cloned());
                        lines.push(format!(">>>>>>> {}", b.theirs_label));
                    }
                },
            }
        }
        let mut out = lines.join("\n");
        if self.ends_with_newline && !out.is_empty() {
            out.push('\n');
        }
        out
    }

    /// Builds aligned rows for the three-pane `(Ours | Result | Theirs)` view.
    pub fn three_pane_rows(&self) -> Vec<ConflictPaneRow> {
        let mut rows = Vec::new();
        for seg in &self.segments {
            match seg {
                ConflictSegment::Common(ls) => {
                    for l in ls {
                        rows.push(ConflictPaneRow {
                            block_idx: None,
                            ours: l.clone(),
                            merged: l.clone(),
                            theirs: l.clone(),
                            choice: None,
                        });
                    }
                }
                ConflictSegment::Conflict(b) => {
                    let merged_lines = b.resolved_lines().unwrap_or_else(|| {
                        vec![format!(
                            "⚠ Conflict #{}: choose [y] Ours / [t] Theirs / [b] Both",
                            b.id + 1
                        )]
                    });
                    let max_len = b
                        .ours_lines
                        .len()
                        .max(merged_lines.len())
                        .max(b.theirs_lines.len())
                        .max(1);
                    for i in 0..max_len {
                        rows.push(ConflictPaneRow {
                            block_idx: Some(b.id),
                            ours: b.ours_lines.get(i).cloned().unwrap_or_default(),
                            merged: merged_lines.get(i).cloned().unwrap_or_default(),
                            theirs: b.theirs_lines.get(i).cloned().unwrap_or_default(),
                            choice: Some(b.choice),
                        });
                    }
                }
            }
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_markers_resolves_per_block_and_renders_merged() {
        let src = "\
fn a() {}
<<<<<<< HEAD
let x = 1;
=======
let x = 2;
>>>>>>> feature
fn mid() {}
<<<<<<< HEAD
let y = 10;
||||||| base
let y = 0;
=======
let y = 20;
>>>>>>> feature
";
        let mut doc = ConflictDocument::parse("src/lib.rs", src, None, None);
        assert_eq!(doc.conflict_count, 2);
        assert_eq!(doc.unresolved_count(), 2);

        doc.set_choice(0, ConflictChoice::Ours);
        doc.set_choice(1, ConflictChoice::BothOursFirst);
        assert_eq!(doc.unresolved_count(), 0);
        assert_eq!(
            doc.render_merged(),
            "fn a() {}\nlet x = 1;\nfn mid() {}\nlet y = 10;\nlet y = 20;\n"
        );
        let rows = doc.three_pane_rows();
        assert!(rows.iter().any(|r| r.block_idx == Some(0) && r.ours == "let x = 1;"));
        assert!(rows.iter().any(|r| r.block_idx == Some(1) && r.theirs == "let y = 20;"));
    }

    #[test]
    fn falls_back_to_index_stages_when_no_markers_exist() {
        let doc = ConflictDocument::parse("a.txt", "", Some("ours\n"), Some("theirs\n"));
        assert_eq!(doc.conflict_count, 1);
        let b = doc.block(0).unwrap();
        assert_eq!(b.ours_lines, vec!["ours"]);
        assert_eq!(b.theirs_lines, vec!["theirs"]);
    }
}
