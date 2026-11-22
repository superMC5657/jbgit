use std::path::Path;

use jbgit_core::{
    EpochToken, FileHistoryBundle, FileHistoryEntry, FileHistoryKey, FileStatusKind, Oid,
};

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
        });
    }

    let mut tracked: Option<String> = None;
    for e in &mut entries {
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
        .args(["-c", "core.quotepath=false", "log", &max_count_arg]);
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

    let (mut entries, truncated) = parse_file_history_z(&output.stdout, DEFAULT_FILE_HISTORY_LIMIT);
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
