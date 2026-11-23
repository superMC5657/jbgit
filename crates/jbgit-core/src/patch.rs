//! Apply Patch (`ApplyPatchAction` / `ApplyPatchDefaultExecutor`): patch input,
//! apply mode and the read-only `git apply --check --numstat` preflight model.

use std::path::PathBuf;

/// Where the patch text comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PatchInput {
    /// A patch / diff file (absolute, or relative to the working tree root).
    File(PathBuf),
    /// The system clipboard, read by the engine and fed to `git apply` on stdin.
    Clipboard,
}

/// How `git apply` touches the repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ApplyMode {
    /// Working tree only (`git apply`).
    #[default]
    WorkingTree,
    /// Working tree and index (`git apply --index`).
    Index,
    /// Three-way merge fallback (`git apply --3way`): leaves conflicts to resolve.
    ThreeWay,
}

impl ApplyMode {
    pub const ALL: [Self; 3] = [Self::WorkingTree, Self::Index, Self::ThreeWay];

    /// `git apply` flag selecting this mode.
    pub const fn flag(self) -> Option<&'static str> {
        match self {
            Self::WorkingTree => None,
            Self::Index => Some("--index"),
            Self::ThreeWay => Some("--3way"),
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::WorkingTree => Self::Index,
            Self::Index => Self::ThreeWay,
            Self::ThreeWay => Self::WorkingTree,
        }
    }
}

/// Read-only preflight request (`git apply --check --numstat [mode]`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PatchCheckKey {
    pub source: PatchInput,
    pub mode: ApplyMode,
}

/// One file touched by a patch (`--numstat` row; `None` counts = binary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchFileStat {
    pub path: String,
    pub added: Option<u32>,
    pub deleted: Option<u32>,
    /// `--3way` reported this file would apply with conflicts.
    pub conflicts: bool,
}

/// Result of the preflight: affected files, or the first error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchCheck {
    pub key: PatchCheckKey,
    pub files: Vec<PatchFileStat>,
    /// First meaningful `error:` line when the patch does not apply.
    pub error: Option<String>,
}

impl PatchCheck {
    pub fn applies(&self) -> bool {
        self.error.is_none()
    }

    pub fn conflict_count(&self) -> usize {
        self.files.iter().filter(|f| f.conflicts).count()
    }
}

/// Parses `git apply --numstat` output (`added\tdeleted\tpath`, `-` for
/// binary) plus the `--3way` `Applied patch to 'p' with conflicts.` notes.
pub fn parse_apply_check(stdout: &[String], stderr: &[String]) -> Vec<PatchFileStat> {
    let conflicted: Vec<String> = stdout
        .iter()
        .chain(stderr)
        .filter_map(|l| {
            l.trim()
                .strip_prefix("Applied patch to '")
                .and_then(|rest| rest.strip_suffix("' with conflicts."))
                .map(str::to_string)
        })
        .collect();
    stdout
        .iter()
        .filter_map(|line| {
            let mut cols = line.splitn(3, '\t');
            let added = cols.next()?;
            let deleted = cols.next()?;
            let path = unquote_path(cols.next()?);
            let count = |s: &str| s.trim().parse::<u32>().ok();
            if count(added).is_none() && added.trim() != "-" {
                return None;
            }
            Some(PatchFileStat {
                conflicts: conflicted.contains(&path),
                added: count(added),
                deleted: count(deleted),
                path,
            })
        })
        .collect()
}

/// First error line of a failed `git apply` (`error: f.txt: patch does not apply`).
pub fn first_apply_error(lines: &[String]) -> String {
    lines
        .iter()
        .map(|l| l.trim())
        .find(|l| {
            l.starts_with("error:") && !l.starts_with("error: patch failed:")
                || l.starts_with("fatal:")
        })
        .or_else(|| lines.iter().map(|l| l.trim()).find(|l| !l.is_empty()))
        .unwrap_or("git apply failed")
        .to_string()
}

/// `U <path>` lines printed by `git apply --3way` for conflicted files.
pub fn parse_apply_conflicts(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|l| l.strip_prefix("U ").map(|p| unquote_path(p.trim())))
        .collect()
}

/// Strips git's C-style quoting from a path (`"a b.txt"` → `a b.txt`).
fn unquote_path(raw: &str) -> String {
    let raw = raw.trim_end_matches(['\r', '\n']);
    match raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        Some(inner) => inner.replace("\\\"", "\"").replace("\\\\", "\\"),
        None => raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(str::to_string).collect()
    }

    #[test]
    fn numstat_rows_binary_and_conflict_markers() {
        let out = lines("1\t1\tf.txt\n-\t-\timg.png\n3\t0\t\"a b.txt\"\n");
        let err =
            lines("Applied patch to 'f.txt' with conflicts.\nApplied patch to 'img.png' cleanly.");
        let files = parse_apply_check(&out, &err);
        assert_eq!(
            files,
            vec![
                PatchFileStat {
                    path: "f.txt".into(),
                    added: Some(1),
                    deleted: Some(1),
                    conflicts: true,
                },
                PatchFileStat {
                    path: "img.png".into(),
                    added: None,
                    deleted: None,
                    conflicts: false,
                },
                PatchFileStat {
                    path: "a b.txt".into(),
                    added: Some(3),
                    deleted: Some(0),
                    conflicts: false,
                },
            ]
        );
    }

    #[test]
    fn notes_mixed_into_stdout_are_not_rows() {
        let out = lines("Applied patch to 'f.txt' with conflicts.\n1\t1\tf.txt");
        let files = parse_apply_check(&out, &[]);
        assert_eq!(files.len(), 1);
        assert!(files[0].conflicts);
    }

    #[test]
    fn first_error_skips_the_hunk_location_line() {
        let err = lines("error: patch failed: f.txt:1\nerror: f.txt: patch does not apply");
        assert_eq!(
            first_apply_error(&err),
            "error: f.txt: patch does not apply"
        );
        assert_eq!(
            first_apply_error(&lines(
                "error: No valid patches in input (allow with \"--allow-empty\")"
            )),
            "error: No valid patches in input (allow with \"--allow-empty\")"
        );
    }

    #[test]
    fn three_way_conflict_paths() {
        let out = lines("Applied patch to 'f.txt' with conflicts.\nU f.txt\nU \"sp ace.txt\"");
        assert_eq!(parse_apply_conflicts(&out), vec!["f.txt", "sp ace.txt"]);
    }

    #[test]
    fn mode_flags_cycle() {
        assert_eq!(ApplyMode::WorkingTree.flag(), None);
        assert_eq!(ApplyMode::Index.flag(), Some("--index"));
        assert_eq!(ApplyMode::ThreeWay.next(), ApplyMode::WorkingTree);
    }
}
