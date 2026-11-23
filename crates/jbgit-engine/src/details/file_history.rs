use std::path::Path;
use std::sync::Arc;

use jbgit_core::{
    EpochToken, FileHistoryBundle, FileHistoryEntry, FileHistoryKey, FileStatusKind, Oid,
};

use super::blame::unquote_git_path;
use crate::cli::non_interactive_std_command;
use crate::error::EngineError;
use crate::probe::RepoLocation;

/// Default max commits loaded for a single-file history query.
pub const DEFAULT_FILE_HISTORY_LIMIT: usize = 500;

fn trim_leading_newlines(mut bytes: &[u8]) -> &[u8] {
    while let Some(&b) = bytes.first() {
        if b == b'\n' || b == b'\r' {
            bytes = &bytes[1..];
        } else {
            break;
        }
    }
    bytes
}

fn decode_git_path(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace('\\', "/")
}

fn status_kind_from_code(code: &str) -> FileStatusKind {
    if code.starts_with('R') {
        FileStatusKind::Renamed
    } else if code.starts_with('C') {
        FileStatusKind::Copied
    } else if code.starts_with('A') {
        FileStatusKind::Added
    } else if code.starts_with('D') {
        FileStatusKind::Deleted
    } else if code.starts_with('T') {
        FileStatusKind::Typechange
    } else {
        FileStatusKind::Modified
    }
}

fn set_path_if_empty(target: &mut String, bytes: &[u8]) {
    if target.is_empty() {
        let s = decode_git_path(bytes);
        if !s.is_empty() {
            *target = s;
        }
    }
}

fn take_rename_paths(
    tokens: &[&[u8]],
    idx: usize,
    old_path: &mut Option<String>,
    path_at_commit: &mut String,
) -> usize {
    if let Some(old_b) = tokens.get(idx + 1) {
        let old_s = decode_git_path(old_b);
        if old_path.is_none() && !old_s.is_empty() {
            *old_path = Some(old_s);
        }
    }
    if let Some(new_b) = tokens.get(idx + 2) {
        set_path_if_empty(path_at_commit, new_b);
    }
    idx + 3
}

/// Parses NUL-delimited `git log -z --raw --numstat --format=%x1e%H%x1f%an%x1f%at%x1f%s%x1f`
/// output into [`FileHistoryEntry`] items and a truncation flag.
pub fn parse_file_history_z(raw: &[u8], max_entries: usize) -> (Vec<FileHistoryEntry>, bool) {
    let mut entries: Vec<FileHistoryEntry> = Vec::new();
    let mut truncated = false;

    for rec in raw.split(|&b| b == 0x1E) {
        if rec.is_empty() {
            continue;
        }
        let mut fields = rec.splitn(5, |&b| b == 0x1F);
        let (Some(hash_b), Some(author_b), Some(time_b), Some(summary_b)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Ok(oid) = String::from_utf8_lossy(hash_b).trim().parse::<Oid>() else {
            continue;
        };
        if entries.len() >= max_entries {
            truncated = true;
            break;
        }

        let author_name = String::from_utf8_lossy(author_b).into_owned();
        let author_time = String::from_utf8_lossy(time_b)
            .trim()
            .parse::<i64>()
            .unwrap_or(0);
        let summary = String::from_utf8_lossy(summary_b).into_owned();
        let rest = fields.next().unwrap_or(&[]);

        let mut path_at_commit = String::new();
        let mut old_path: Option<String> = None;
        let mut kind = FileStatusKind::Modified;
        let mut kind_set = false;
        let mut additions = 0usize;
        let mut deletions = 0usize;
        let mut is_binary = false;

        let tokens: Vec<&[u8]> = rest.split(|&b| b == 0).collect();
        let mut idx = 0usize;
        while idx < tokens.len() {
            let tok = trim_leading_newlines(tokens[idx]);
            if tok.is_empty() {
                idx += 1;
                continue;
            }
            if tok.starts_with(b":") {
                let header = String::from_utf8_lossy(tok);
                let code = header.split_ascii_whitespace().last().unwrap_or("M");
                if !kind_set {
                    kind = status_kind_from_code(code);
                    kind_set = true;
                }
                if code.starts_with('R') || code.starts_with('C') {
                    idx = take_rename_paths(&tokens, idx, &mut old_path, &mut path_at_commit);
                } else {
                    if let Some(p_b) = tokens.get(idx + 1) {
                        set_path_if_empty(&mut path_at_commit, p_b);
                    }
                    idx += 2;
                }
                continue;
            }

            let mut parts = tok.splitn(3, |&b| b == b'\t');
            if let (Some(add_b), Some(del_b), Some(path_part)) =
                (parts.next(), parts.next(), parts.next())
                && let (Ok(add_s), Ok(del_s)) =
                    (std::str::from_utf8(add_b), std::str::from_utf8(del_b))
            {
                let add_s = add_s.trim();
                let del_s = del_s.trim();
                let parsed = if add_s == "-" && del_s == "-" {
                    is_binary = true;
                    additions = 0;
                    deletions = 0;
                    true
                } else if let (Ok(a), Ok(d)) = (add_s.parse::<usize>(), del_s.parse::<usize>()) {
                    additions = a;
                    deletions = d;
                    is_binary = false;
                    true
                } else {
                    false
                };
                if parsed {
                    if path_part.is_empty() {
                        idx = take_rename_paths(&tokens, idx, &mut old_path, &mut path_at_commit);
                    } else {
                        set_path_if_empty(&mut path_at_commit, path_part);
                        idx += 1;
                    }
                    continue;
                }
            }

            idx += 1;
        }

        entries.push(FileHistoryEntry {
            oid,
            path_at_commit,
            old_path,
            kind,
            additions,
            deletions,
            is_binary,
            author_name,
            author_time,
            summary,
            range_patch: None,
        });
    }

    backfill_entry_paths(&mut entries);

    (entries, truncated)
}

fn backfill_entry_paths(entries: &mut [FileHistoryEntry]) {
    let mut tracked: Option<String> = None;
    for e in entries.iter_mut() {
        if e.path_at_commit.is_empty() {
            if let Some(p) = &tracked {
                e.path_at_commit = p.clone();
            }
        } else {
            tracked = e
                .old_path
                .clone()
                .or_else(|| Some(e.path_at_commit.clone()));
        }
    }
    let mut next_path: Option<String> = None;
    for e in entries.iter_mut().rev() {
        if e.path_at_commit.is_empty() {
            if let Some(p) = &next_path {
                e.path_at_commit = p.clone();
            }
        } else {
            next_path = Some(e.path_at_commit.clone());
        }
    }
}

fn is_record_boundary_at(raw: &[u8], pos: usize) -> bool {
    if raw.get(pos) != Some(&0x1E) {
        return false;
    }
    if pos > 0 && raw[pos - 1] != b'\n' {
        return false;
    }
    let Some(oid_slice) = raw.get(pos + 1..pos + 41) else {
        return false;
    };
    oid_slice.iter().all(|b| b.is_ascii_hexdigit()) && raw.get(pos + 41) == Some(&0x1F)
}

fn strip_diff_prefix(path: String, prefix: &str) -> String {
    path.strip_prefix(prefix).unwrap_or(&path).to_string()
}

fn hunk_prefix_width(line: &str) -> Option<usize> {
    let at_count = line.bytes().take_while(|&b| b == b'@').count();
    if at_count < 2 {
        return None;
    }
    let after = &line[at_count..];
    if !after.starts_with(' ') {
        return None;
    }
    let closing = "@".repeat(at_count);
    if after.contains(&closing) {
        Some(at_count - 1)
    } else {
        None
    }
}

fn trim_patch_newlines(bytes: &[u8]) -> &[u8] {
    let mut start = 0usize;
    while start < bytes.len() && matches!(bytes[start], b'\n' | b'\r') {
        start += 1;
    }
    let mut end = bytes.len();
    while end > start && matches!(bytes[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    &bytes[start..end]
}

/// Parses `git log -L <start>,<end>:<path> --format=%x1e%H%x1f%an%x1f%at%x1f%s%x1f`
/// output into [`FileHistoryEntry`] items with cached `range_patch` text.
pub fn parse_line_history(
    raw: &[u8],
    default_path: &str,
    max_entries: usize,
) -> (Vec<FileHistoryEntry>, bool) {
    let mut boundaries = Vec::new();
    for pos in 0..raw.len() {
        if is_record_boundary_at(raw, pos) {
            boundaries.push(pos);
        }
    }

    let mut entries = Vec::new();
    let mut truncated = false;

    for (idx, &start) in boundaries.iter().enumerate() {
        let end = boundaries.get(idx + 1).copied().unwrap_or(raw.len());
        let rec = &raw[start + 1..end];
        let mut fields = rec.splitn(5, |&b| b == 0x1F);
        let (Some(hash_b), Some(author_b), Some(time_b), Some(summary_b)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Ok(oid) = String::from_utf8_lossy(hash_b).trim().parse::<Oid>() else {
            continue;
        };
        if entries.len() >= max_entries {
            truncated = true;
            break;
        }

        let author_name = String::from_utf8_lossy(author_b).into_owned();
        let author_time = String::from_utf8_lossy(time_b)
            .trim()
            .parse::<i64>()
            .unwrap_or(0);
        let summary = String::from_utf8_lossy(summary_b).into_owned();
        let patch_bytes = trim_patch_newlines(fields.next().unwrap_or(&[]));
        let patch_str = String::from_utf8_lossy(patch_bytes);
        let range_patch = (!patch_str.is_empty()).then(|| Arc::from(patch_str.as_ref()));

        let mut rename_from: Option<String> = None;
        let mut rename_to: Option<String> = None;
        let mut minus_path: Option<String> = None;
        let mut plus_path: Option<String> = None;
        let mut cc_path: Option<String> = None;
        let mut kind = FileStatusKind::Modified;
        let mut additions = 0usize;
        let mut deletions = 0usize;
        let mut in_hunk = false;
        let mut prefix_width = 1usize;

        for raw_line in patch_str.lines() {
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            if let Some(pw) = hunk_prefix_width(line) {
                in_hunk = true;
                prefix_width = pw;
                continue;
            }
            if in_hunk {
                if line == "\\ No newline at end of file" {
                    continue;
                }
                if line.starts_with("diff --git ")
                    || line.starts_with("diff --cc ")
                    || line.starts_with("diff --combined ")
                {
                    in_hunk = false;
                } else if line.len() >= prefix_width
                    && line.is_char_boundary(prefix_width)
                    && line.as_bytes()[..prefix_width]
                        .iter()
                        .all(|b| matches!(b, b' ' | b'+' | b'-'))
                {
                    let prefix = &line[..prefix_width];
                    if prefix.contains('+') {
                        additions += 1;
                    } else if prefix.contains('-') {
                        deletions += 1;
                    }
                    continue;
                } else {
                    in_hunk = false;
                }
            }

            if let Some(rest) = line.strip_prefix("rename from ") {
                rename_from = Some(unquote_git_path(rest.as_bytes()));
                kind = FileStatusKind::Renamed;
            } else if let Some(rest) = line.strip_prefix("rename to ") {
                rename_to = Some(unquote_git_path(rest.as_bytes()));
                kind = FileStatusKind::Renamed;
            } else if line.starts_with("new file mode ") {
                kind = FileStatusKind::Added;
            } else if line.starts_with("deleted file mode ") {
                kind = FileStatusKind::Deleted;
            } else if let Some(rest) = line.strip_prefix("diff --cc ") {
                cc_path = Some(unquote_git_path(rest.as_bytes()));
            } else if let Some(rest) = line.strip_prefix("diff --combined ") {
                cc_path = Some(unquote_git_path(rest.as_bytes()));
            } else if let Some(rest) = line.strip_prefix("--- ") {
                let decoded = unquote_git_path(rest.as_bytes());
                if decoded == "/dev/null" {
                    kind = FileStatusKind::Added;
                } else {
                    minus_path = Some(strip_diff_prefix(decoded, "a/"));
                }
            } else if let Some(rest) = line.strip_prefix("+++ ") {
                let decoded = unquote_git_path(rest.as_bytes());
                if decoded == "/dev/null" {
                    kind = FileStatusKind::Deleted;
                } else {
                    plus_path = Some(strip_diff_prefix(decoded, "b/"));
                }
            }
        }

        let (path_at_commit, old_path) = if let Some(to) = rename_to {
            (to, rename_from.or(minus_path))
        } else if let Some(plus) = plus_path {
            let renamed_from = minus_path.filter(|m| m != &plus);
            if renamed_from.is_some() {
                kind = FileStatusKind::Renamed;
            }
            (plus, renamed_from)
        } else if let Some(minus) = minus_path {
            (minus, None)
        } else if let Some(cc) = cc_path {
            (cc, None)
        } else {
            (String::new(), None)
        };

        entries.push(FileHistoryEntry {
            oid,
            path_at_commit,
            old_path,
            kind,
            additions,
            deletions,
            is_binary: false,
            author_name,
            author_time,
            summary,
            range_patch,
        });
    }

    backfill_entry_paths(&mut entries);
    for e in &mut entries {
        if e.path_at_commit.is_empty() {
            e.path_at_commit = default_path.to_string();
        }
    }

    (entries, truncated)
}

fn is_empty_or_unborn_log_error(stderr: &str) -> bool {
    let low = stderr.to_ascii_lowercase();
    low.contains("does not have any commits yet")
        || low.contains("your current branch") && low.contains("does not have any commits")
        || low.contains("unknown revision")
        || low.contains("bad revision")
        || low.contains("ambiguous argument")
}

/// Loads commit history for a single file using read-only `git log [--follow] -z --raw --numstat`.
pub fn load_file_history_sync(
    location: &RepoLocation,
    key: &FileHistoryKey,
    token: &EpochToken,
) -> Result<FileHistoryBundle, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    if key.path.trim().is_empty() {
        return Ok(FileHistoryBundle {
            key: key.clone(),
            entries: Vec::new(),
            truncated: false,
        });
    }

    let start_rev = key
        .start_oid
        .map(|o| o.to_string())
        .unwrap_or_else(|| "HEAD".to_string());
    let max_count_arg = format!("--max-count={}", DEFAULT_FILE_HISTORY_LIMIT + 1);

    let git = crate::cli::cached_git_executable().unwrap_or_else(|| Path::new("git"));
    let mut cmd = non_interactive_std_command(git, Some(&location.workdir));
    cmd.arg("--git-dir")
        .arg(&location.git_dir)
        .arg("--work-tree")
        .arg(&location.workdir)
        .args(["-c", "core.quotepath=false", "log", &max_count_arg]);
    if let Some((a, b)) = key.line_range {
        let start = a.min(b).max(1);
        let end = a.max(b).max(1);
        let range_spec = format!("{start},{end}:{}", key.path);
        cmd.args([
            "--format=%x1e%H%x1f%an%x1f%at%x1f%s%x1f",
            "-L",
            &range_spec,
            &start_rev,
        ]);
    } else {
        if key.follow_renames {
            cmd.arg("--follow");
        }
        cmd.args([
            "-z",
            "--raw",
            "--numstat",
            "--format=%x1e%H%x1f%an%x1f%at%x1f%s%x1f",
            &start_rev,
            "--",
            &key.path,
        ]);
    }

    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            EngineError::GitNotFound
        } else {
            EngineError::Io(e)
        }
    })?;

    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if is_empty_or_unborn_log_error(&stderr) {
            return Ok(FileHistoryBundle {
                key: key.clone(),
                entries: Vec::new(),
                truncated: false,
            });
        }
        return Err(EngineError::Other(if stderr.is_empty() {
            format!("git log failed with exit {:?}", output.status.code())
        } else {
            stderr
        }));
    }

    let (mut entries, truncated) = if key.line_range.is_some() {
        parse_line_history(&output.stdout, &key.path, DEFAULT_FILE_HISTORY_LIMIT)
    } else {
        parse_file_history_z(&output.stdout, DEFAULT_FILE_HISTORY_LIMIT)
    };
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    for entry in &mut entries {
        if entry.path_at_commit.is_empty() {
            entry.path_at_commit = key.path.clone();
        }
    }

    Ok(FileHistoryBundle {
        key: key.clone(),
        entries,
        truncated,
    })
}
