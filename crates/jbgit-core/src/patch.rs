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

/// Options controlling `git apply` (mode, reverse, strip level, target directory, file filter).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ApplyOptions {
    pub mode: ApplyMode,
    /// Apply patch in reverse (`-R`).
    pub reverse: bool,
    /// Number of leading path components to strip (`-p<n>`; `None` = git default `-p1`).
    pub strip: Option<u8>,
    /// Prepend `<dir>/` to all filenames (`--directory=<dir>`).
    pub directory: Option<String>,
    /// Apply only changes to matching target paths (`--include=<path>`). Empty = all files.
    pub include_paths: Vec<String>,
}

impl ApplyOptions {
    pub fn with_mode(mode: ApplyMode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    /// Normalized `--directory` value (trimmed, slashes unified, leading/trailing `/` stripped).
    pub fn normalized_directory(&self) -> Option<String> {
        let raw = self.directory.as_deref()?.trim();
        let norm = raw.replace('\\', "/");
        let trimmed = norm.trim_matches('/');
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }

    /// Effective strip count used for preview path display (`-p1` when unset).
    pub const fn effective_strip(&self) -> u8 {
        match self.strip {
            Some(n) => n,
            None => 1,
        }
    }
}

/// Read-only preflight request (`git apply --check --numstat [options]`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PatchCheckKey {
    pub source: PatchInput,
    pub options: ApplyOptions,
}

impl PatchCheckKey {
    pub fn new(source: PatchInput, options: ApplyOptions) -> Self {
        Self { source, options }
    }
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

/// One file's unified diff section extracted from raw patch text for previewing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchFileSection {
    /// Target path after applying `strip` and `directory`.
    pub path: String,
    /// Raw unified diff text of this file section.
    pub diff_text: String,
}

/// Result of the preflight: affected files, parsed diff sections, or the first error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchCheck {
    pub key: PatchCheckKey,
    pub files: Vec<PatchFileStat>,
    /// Per-file diff sections parsed from the patch text (available even if `--check` fails).
    pub sections: Vec<PatchFileSection>,
    /// Whether the patch contains Git `index <old>..<new>` headers (required for `--3way`).
    pub has_index_headers: bool,
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

    /// True when `--3way` failed on a plain unified patch that lacks Git `index` blob headers.
    pub fn needs_three_way_index_fallback(&self) -> bool {
        needs_three_way_index_fallback(self)
    }
}

/// Whether a failed `--3way` preflight should advise switching to Working Tree or adjusting `-p`.
pub fn needs_three_way_index_fallback(check: &PatchCheck) -> bool {
    if check.key.options.mode != ApplyMode::ThreeWay || check.applies() {
        return false;
    }
    if !check.has_index_headers {
        return true;
    }
    check.error.as_deref().is_some_and(|err| {
        let lower = err.to_ascii_lowercase();
        lower.contains("lacks the necessary blob")
            || lower.contains("cannot perform three-way")
            || lower.contains("without index")
    })
}

/// Strips `strip` leading `/`-separated path components and prepends `directory` if non-empty.
pub fn apply_strip_and_directory(raw_path: &str, strip: u8, directory: Option<&str>) -> String {
    let norm = raw_path.replace('\\', "/");
    let parts: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    let stripped = if parts.is_empty() {
        String::new()
    } else {
        let skip = (strip as usize).min(parts.len().saturating_sub(1));
        parts[skip..].join("/")
    };
    match directory
        .map(|d| d.replace('\\', "/"))
        .map(|d| d.trim_matches('/').to_string())
        .filter(|d| !d.is_empty())
    {
        Some(dir) if !stripped.is_empty() => format!("{dir}/{stripped}"),
        Some(dir) => dir,
        None => stripped,
    }
}

fn parse_header_path(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "/dev/null" {
        return None;
    }
    // Plain unified diffs may append `\t<timestamp>`.
    let without_ts = trimmed.split('\t').next().unwrap_or(trimmed).trim();
    if without_ts.is_empty() || without_ts == "/dev/null" {
        return None;
    }
    Some(unquote_path(without_ts))
}

fn parse_diff_git_b_path(line: &str) -> Option<String> {
    let rest = line.strip_prefix("diff --git ")?;
    // Handle quoted or unquoted `a/... b/...`: scan tokens cleanly.
    let mut tokens: Vec<String> = Vec::new();
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b'"' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            tokens.push(unquote_path(&rest[start..i]));
        } else {
            let start = i;
            while i < bytes.len() && bytes[i] != b' ' {
                i += 1;
            }
            tokens.push(rest[start..i].to_string());
        }
    }
    if tokens.len() == 2 {
        return Some(tokens.remove(1));
    }
    // Unquoted path containing spaces: `a/<p> b/<p>` — find ` b/` separator.
    if let Some(pos) = rest.rfind(" b/") {
        return Some(rest[pos + 1..].to_string());
    }
    tokens.pop()
}

/// Splits raw unified/git patch text into per-file sections and detects Git `index` headers.
pub fn parse_patch_sections(
    patch_text: &str,
    strip: u8,
    directory: Option<&str>,
) -> (Vec<PatchFileSection>, bool) {
    let mut sections: Vec<PatchFileSection> = Vec::new();
    let mut has_index_headers = false;

    let lines: Vec<&str> = patch_text.lines().collect();
    let has_diff_git = lines.iter().any(|l| l.starts_with("diff --git "));

    let mut cur_lines: Vec<&str> = Vec::new();
    let mut cur_raw_path: Option<String> = None;
    let mut cur_from_path: Option<String> = None;
    let mut cur_to_path: Option<String> = None;
    let mut cur_git_path: Option<String> = None;

    let flush = |sections: &mut Vec<PatchFileSection>,
                 cur_lines: &mut Vec<&str>,
                 cur_raw_path: &mut Option<String>,
                 cur_from_path: &mut Option<String>,
                 cur_to_path: &mut Option<String>,
                 cur_git_path: &mut Option<String>| {
        if cur_lines.is_empty() {
            return;
        }
        let chosen = cur_to_path
            .take()
            .or_else(|| cur_from_path.take())
            .or_else(|| cur_git_path.take())
            .or_else(|| cur_raw_path.take());
        if let Some(raw) = chosen {
            let path = apply_strip_and_directory(&raw, strip, directory);
            if !path.is_empty() {
                sections.push(PatchFileSection {
                    path,
                    diff_text: cur_lines.join("\n"),
                });
            }
        }
        cur_lines.clear();
        *cur_raw_path = None;
        *cur_from_path = None;
        *cur_to_path = None;
        *cur_git_path = None;
    };

    let mut idx = 0;
    while idx < lines.len() {
        let line = lines[idx];
        let starts_section = if has_diff_git {
            line.starts_with("diff --git ")
        } else {
            line.starts_with("--- ")
                && lines
                    .get(idx + 1)
                    .is_some_and(|next| next.starts_with("+++ "))
        };

        if starts_section {
            flush(
                &mut sections,
                &mut cur_lines,
                &mut cur_raw_path,
                &mut cur_from_path,
                &mut cur_to_path,
                &mut cur_git_path,
            );
            if line.starts_with("diff --git ") {
                cur_git_path = parse_diff_git_b_path(line);
            }
        }

        if cur_lines.is_empty() && !starts_section {
            // Skip mail/format-patch preamble lines before the first diff header.
            idx += 1;
            continue;
        }

        if line.starts_with("index ") && line.contains("..") {
            has_index_headers = true;
        }
        if let Some(rest) = line.strip_prefix("--- ")
            && cur_from_path.is_none()
        {
            cur_from_path = parse_header_path(rest);
        } else if let Some(rest) = line.strip_prefix("+++ ")
            && cur_to_path.is_none()
        {
            cur_to_path = parse_header_path(rest);
        }
        cur_lines.push(line);
        idx += 1;
    }

    flush(
        &mut sections,
        &mut cur_lines,
        &mut cur_raw_path,
        &mut cur_from_path,
        &mut cur_to_path,
        &mut cur_git_path,
    );

    (sections, has_index_headers)
}

/// Builds a standard unified new-file (or deleted-file when `reverse`) patch for
/// an untracked text file. Returns `None` if `bytes` appears binary (contains NUL).
pub fn build_untracked_file_patch(rel_path: &str, bytes: &[u8], reverse: bool) -> Option<String> {
    if crate::is_binary_bytes(bytes) {
        return None;
    }
    let path = rel_path.replace('\\', "/");
    let path = path.trim_start_matches("./");
    let mut out = String::new();
    out.push_str(&format!("diff --git a/{path} b/{path}\n"));
    if reverse {
        out.push_str("deleted file mode 100644\n");
        out.push_str(&format!("--- a/{path}\n"));
        out.push_str("+++ /dev/null\n");
    } else {
        out.push_str("new file mode 100644\n");
        out.push_str("--- /dev/null\n");
        out.push_str(&format!("+++ b/{path}\n"));
    }
    if bytes.is_empty() {
        return Some(out);
    }
    let text = String::from_utf8_lossy(bytes);
    let ends_with_newline = text.ends_with('\n');
    let raw_lines: Vec<&str> = text.split('\n').collect();
    let lines: &[&str] = if ends_with_newline {
        &raw_lines[..raw_lines.len().saturating_sub(1)]
    } else {
        &raw_lines[..]
    };
    let count = lines.len();
    if count == 0 {
        return Some(out);
    }
    if reverse {
        if count == 1 {
            out.push_str("@@ -1 +0,0 @@\n");
        } else {
            out.push_str(&format!("@@ -1,{count} +0,0 @@\n"));
        }
        for l in lines {
            out.push('-');
            out.push_str(l);
            out.push('\n');
        }
    } else {
        if count == 1 {
            out.push_str("@@ -0,0 +1 @@\n");
        } else {
            out.push_str(&format!("@@ -0,0 +1,{count} @@\n"));
        }
        for l in lines {
            out.push('+');
            out.push_str(l);
            out.push('\n');
        }
    }
    if !ends_with_newline {
        out.push_str("\\ No newline at end of file\n");
    }
    Some(out)
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

    #[test]
    fn strip_and_directory_transform_paths() {
        assert_eq!(
            apply_strip_and_directory("a/prefix/sub/file.rs", 2, Some("dst/")),
            "dst/sub/file.rs"
        );
        assert_eq!(
            apply_strip_and_directory("b/src/main.rs", 1, None),
            "src/main.rs"
        );
        assert_eq!(
            apply_strip_and_directory("only.txt", 5, Some("sub")),
            "sub/only.txt"
        );
    }

    #[test]
    fn parse_patch_sections_extracts_git_and_plain_unified_diffs() {
        let git_patch = "\
From 1111111111111111111111111111111111111111 Mon Sep 17 00:00:00 2001
Subject: [PATCH] demo

diff --git a/old_dir/one.txt b/old_dir/one.txt
index 1111111..2222222 100644
--- a/old_dir/one.txt
+++ b/old_dir/one.txt
@@ -1 +1 @@
-a
+b
diff --git a/old_dir/two.txt b/old_dir/two.txt
new file mode 100644
--- /dev/null
+++ b/old_dir/two.txt
@@ -0,0 +1 @@
+hello
";
        let (sections, has_idx) = parse_patch_sections(git_patch, 2, Some("sub"));
        assert!(has_idx);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].path, "sub/one.txt");
        assert!(sections[0].diff_text.contains("+b"));
        assert_eq!(sections[1].path, "sub/two.txt");
        assert!(sections[1].diff_text.contains("+hello"));

        let plain_patch = "\
--- a/foo.txt\t2026-01-01 00:00:00
+++ b/foo.txt\t2026-01-01 00:01:00
@@ -1 +1 @@
-old
+new
";
        let (plain_sections, plain_idx) = parse_patch_sections(plain_patch, 1, None);
        assert!(!plain_idx);
        assert_eq!(plain_sections.len(), 1);
        assert_eq!(plain_sections[0].path, "foo.txt");
        assert!(plain_sections[0].diff_text.contains("+new"));
    }

    #[test]
    fn untracked_file_patch_handles_forward_reverse_no_newline_empty_and_binary() {
        let fwd = build_untracked_file_patch("dir/new.txt", b"alpha\nbeta\n", false).unwrap();
        assert!(fwd.contains("new file mode 100644\n--- /dev/null\n+++ b/dir/new.txt\n"));
        assert!(fwd.contains("@@ -0,0 +1,2 @@\n+alpha\n+beta\n"));

        let rev = build_untracked_file_patch("dir/new.txt", b"alpha\nbeta", true).unwrap();
        assert!(rev.contains("deleted file mode 100644\n--- a/dir/new.txt\n+++ /dev/null\n"));
        assert!(rev.contains("@@ -1,2 +0,0 @@\n-alpha\n-beta\n\\ No newline at end of file\n"));

        let empty = build_untracked_file_patch("empty.txt", b"", false).unwrap();
        assert!(empty.contains("new file mode 100644"));
        assert!(!empty.contains("@@"));

        assert_eq!(build_untracked_file_patch("bin.dat", b"\x00\x01\x02", false), None);
    }

    #[test]
    fn three_way_fallback_detects_missing_index_headers() {
        let check = PatchCheck {
            key: PatchCheckKey::new(
                PatchInput::Clipboard,
                ApplyOptions::with_mode(ApplyMode::ThreeWay),
            ),
            files: Vec::new(),
            sections: Vec::new(),
            has_index_headers: false,
            error: Some("error: foo.txt: patch does not apply".into()),
        };
        assert!(check.needs_three_way_index_fallback());

        let wt_check = PatchCheck {
            key: PatchCheckKey::new(
                PatchInput::Clipboard,
                ApplyOptions::with_mode(ApplyMode::WorkingTree),
            ),
            ..check.clone()
        };
        assert!(!wt_check.needs_three_way_index_fallback());
    }
}
