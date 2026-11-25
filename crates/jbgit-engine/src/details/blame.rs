use std::collections::HashMap;
use std::sync::Arc;

use jbgit_core::{
    BlameBundle, BlameCommit, BlameKey, BlameLine, EpochToken, MAX_DIFF_FILE_BYTES, Oid,
    PersonStamp,
};

use crate::blob_reader::{CheckedBlob, read_tree_path, read_worktree_path};
use crate::cli::non_interactive_std_command;
use crate::error::EngineError;
use crate::pool::with_thread_repo;
use crate::probe::RepoLocation;

use super::to_git2_oid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedBlame {
    pub lines: Vec<BlameLine>,
    pub commits: HashMap<Oid, BlameCommit>,
    pub content: Arc<str>,
    pub uncommitted_oid_present: bool,
}

pub(super) fn unescape_c_quoted_bytes(inner: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0usize;
    while i < inner.len() {
        if inner[i] == b'\\' && i + 1 < inner.len() {
            let next = inner[i + 1];
            match next {
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                }
                b'"' => {
                    out.push(b'"');
                    i += 2;
                }
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                }
                b'r' => {
                    out.push(b'\r');
                    i += 2;
                }
                b't' => {
                    out.push(b'\t');
                    i += 2;
                }
                b'a' => {
                    out.push(0x07);
                    i += 2;
                }
                b'b' => {
                    out.push(0x08);
                    i += 2;
                }
                b'f' => {
                    out.push(0x0C);
                    i += 2;
                }
                b'v' => {
                    out.push(0x0B);
                    i += 2;
                }
                b'0'..=b'7' => {
                    let mut val = (next - b'0') as u16;
                    let mut consumed = 2;
                    for &d in inner.get(i + 2..i + 4).unwrap_or(&[]) {
                        if (b'0'..=b'7').contains(&d) {
                            val = (val << 3) | (d - b'0') as u16;
                            consumed += 1;
                        } else {
                            break;
                        }
                    }
                    out.push((val & 0xFF) as u8);
                    i += consumed;
                }
                other => {
                    out.push(other);
                    i += 2;
                }
            }
        } else {
            out.push(inner[i]);
            i += 1;
        }
    }
    out
}

pub(super) fn unquote_git_path(raw: &[u8]) -> String {
    let trimmed = raw.strip_suffix(b"\r").unwrap_or(raw);
    let decoded = if trimmed.len() >= 2 && trimmed.starts_with(b"\"") && trimmed.ends_with(b"\"") {
        unescape_c_quoted_bytes(&trimmed[1..trimmed.len() - 1])
    } else {
        trimmed.to_vec()
    };
    String::from_utf8_lossy(&decoded).replace('\\', "/")
}

fn parse_tz_offset(raw: &[u8]) -> i32 {
    let s = String::from_utf8_lossy(raw);
    let trimmed = s.trim();
    let sign = if trimmed.starts_with('-') { -1 } else { 1 };
    let digits = trimmed.trim_start_matches(['+', '-']);
    if digits.len() == 4
        && digits.is_ascii()
        && let (Ok(hours), Ok(mins)) = (digits[0..2].parse::<i32>(), digits[2..4].parse::<i32>())
    {
        return sign * (hours * 60 + mins);
    }
    0
}

fn parse_blame_header(line: &[u8]) -> Option<(Oid, u32, u32)> {
    let s = std::str::from_utf8(line.strip_suffix(b"\r").unwrap_or(line)).ok()?;
    let mut parts = s.split_ascii_whitespace();
    let oid = parts.next()?.parse::<Oid>().ok()?;
    let orig_line = parts.next()?.parse::<u32>().ok()?;
    let final_line = parts.next()?.parse::<u32>().ok()?;
    Some((oid, orig_line, final_line))
}

#[cfg(test)]
pub fn parse_blame_porcelain(raw: &[u8]) -> Result<ParsedBlame, EngineError> {
    parse_blame_porcelain_cancellable(raw, || false)
}

pub fn parse_blame_porcelain_cancellable(
    raw: &[u8],
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<ParsedBlame, EngineError> {
    if raw.is_empty() {
        return Ok(ParsedBlame {
            lines: Vec::new(),
            commits: HashMap::new(),
            content: Arc::from(""),
            uncommitted_oid_present: false,
        });
    }

    let mut raw_lines: Vec<&[u8]> = raw.split(|&b| b == b'\n').collect();
    if raw_lines.last().is_some_and(|last| last.is_empty()) {
        raw_lines.pop();
    }

    let mut lines = Vec::new();
    let mut commits: HashMap<Oid, BlameCommit> = HashMap::new();
    let mut last_filename_by_oid: HashMap<Oid, String> = HashMap::new();
    let mut content_lines: Vec<String> = Vec::new();
    let mut uncommitted_oid_present = false;

    let mut i = 0usize;
    while i < raw_lines.len() {
        if lines.len() % 256 == 0 && is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let header_bytes = raw_lines[i];
        if header_bytes.is_empty() {
            i += 1;
            continue;
        }
        let Some((oid, orig_line, final_line)) = parse_blame_header(header_bytes) else {
            return Err(EngineError::Other(format!(
                "invalid git blame porcelain header: {}",
                String::from_utf8_lossy(header_bytes)
            )));
        };
        i += 1;

        let mut author_name: Option<String> = None;
        let mut author_email: Option<String> = None;
        let mut author_time: Option<i64> = None;
        let mut author_tz: Option<i32> = None;
        let mut committer_time: Option<i64> = None;
        let mut committer_tz: Option<i32> = None;
        let mut summary: Option<String> = None;
        let mut previous: Option<(Oid, String)> = None;
        let mut is_boundary = false;
        let mut filename: Option<String> = None;
        let mut found_content = false;

        while i < raw_lines.len() {
            let meta_line = raw_lines[i];
            if let Some(content_bytes) = meta_line.strip_prefix(b"\t") {
                let stripped = content_bytes.strip_suffix(b"\r").unwrap_or(content_bytes);
                content_lines.push(String::from_utf8_lossy(stripped).into_owned());
                i += 1;
                found_content = true;
                break;
            }
            let meta_line = meta_line.strip_suffix(b"\r").unwrap_or(meta_line);
            if let Some(rest) = meta_line.strip_prefix(b"author ") {
                author_name = Some(String::from_utf8_lossy(rest).into_owned());
            } else if let Some(rest) = meta_line.strip_prefix(b"author-mail ") {
                let s = String::from_utf8_lossy(rest);
                let trimmed = s.trim();
                let email = trimmed
                    .strip_prefix('<')
                    .and_then(|v| v.strip_suffix('>'))
                    .unwrap_or(trimmed);
                author_email = Some(email.to_string());
            } else if let Some(rest) = meta_line.strip_prefix(b"author-time ") {
                if let Ok(ts) = String::from_utf8_lossy(rest).trim().parse::<i64>() {
                    author_time = Some(ts);
                }
            } else if let Some(rest) = meta_line.strip_prefix(b"author-tz ") {
                author_tz = Some(parse_tz_offset(rest));
            } else if let Some(rest) = meta_line.strip_prefix(b"committer-time ") {
                if let Ok(ts) = String::from_utf8_lossy(rest).trim().parse::<i64>() {
                    committer_time = Some(ts);
                }
            } else if let Some(rest) = meta_line.strip_prefix(b"committer-tz ") {
                committer_tz = Some(parse_tz_offset(rest));
            } else if let Some(rest) = meta_line.strip_prefix(b"summary ") {
                summary = Some(String::from_utf8_lossy(rest).into_owned());
            } else if let Some(rest) = meta_line.strip_prefix(b"previous ") {
                if let Some(space_pos) = rest.iter().position(|&b| b == b' ') {
                    let prev_oid_b = &rest[..space_pos];
                    let prev_path_b = &rest[space_pos + 1..];
                    if let Ok(prev_oid) = String::from_utf8_lossy(prev_oid_b).trim().parse::<Oid>()
                    {
                        previous = Some((prev_oid, unquote_git_path(prev_path_b)));
                    }
                }
            } else if meta_line == b"boundary" {
                is_boundary = true;
            } else if let Some(rest) = meta_line.strip_prefix(b"filename ") {
                filename = Some(unquote_git_path(rest));
            }
            i += 1;
        }

        if !found_content {
            return Err(EngineError::Other(
                "unexpected EOF inside git blame porcelain entry".to_string(),
            ));
        }

        if oid.is_zero() {
            uncommitted_oid_present = true;
        }

        let orig_path = if let Some(fname) = filename {
            last_filename_by_oid.insert(oid, fname.clone());
            fname
        } else {
            last_filename_by_oid.get(&oid).cloned().unwrap_or_default()
        };

        let line_previous = previous.clone();
        let commit = commits.entry(oid).or_insert_with(|| BlameCommit {
            oid,
            author: PersonStamp {
                name: if oid.is_zero() {
                    author_name
                        .take()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "Not Committed Yet".to_string())
                } else {
                    author_name.take().unwrap_or_default()
                },
                email: author_email.take().unwrap_or_default(),
                time: author_time.unwrap_or(0),
                offset_minutes: author_tz.unwrap_or(0),
            },
            committer_time: committer_time.unwrap_or(0),
            committer_offset_minutes: committer_tz.unwrap_or(0),
            summary: summary.take().unwrap_or_default(),
            previous: previous.take(),
            is_boundary,
        });
        if let Some(name) = author_name
            && commit.author.name.is_empty()
        {
            commit.author.name = name;
        }
        if let Some(email) = author_email
            && commit.author.email.is_empty()
        {
            commit.author.email = email;
        }
        if let Some(ts) = author_time
            && commit.author.time == 0
        {
            commit.author.time = ts;
        }
        if let Some(tz) = author_tz
            && commit.author.offset_minutes == 0
        {
            commit.author.offset_minutes = tz;
        }
        if let Some(ts) = committer_time
            && commit.committer_time == 0
        {
            commit.committer_time = ts;
        }
        if let Some(tz) = committer_tz
            && commit.committer_offset_minutes == 0
        {
            commit.committer_offset_minutes = tz;
        }
        if let Some(sum) = summary
            && commit.summary.is_empty()
        {
            commit.summary = sum;
        }
        if previous.is_some() && commit.previous.is_none() {
            commit.previous = previous;
        }
        commit.is_boundary |= is_boundary;

        lines.push(BlameLine {
            final_line,
            orig_line,
            oid,
            orig_path,
            previous: line_previous,
        });
    }

    let content: Arc<str> = Arc::from(content_lines.join("\n"));
    Ok(ParsedBlame {
        lines,
        commits,
        content,
        uncommitted_oid_present,
    })
}

fn precheck_blame_target(location: &RepoLocation, key: &BlameKey) -> Result<(), EngineError> {
    let checked = match key.rev {
        Some(rev) => with_thread_repo(&location.git_dir, |repo| {
            let oid = to_git2_oid(&rev)?;
            let commit = repo.find_commit(oid)?;
            let tree = commit.tree()?;
            Ok(read_tree_path(repo, &tree, &key.path))
        })?,
        None => read_worktree_path(&location.workdir, &key.path),
    };

    match checked {
        CheckedBlob::TooLarge { .. } => Err(EngineError::TooLarge {
            limit: MAX_DIFF_FILE_BYTES,
        }),
        CheckedBlob::Loaded {
            is_binary: true, ..
        } => Err(EngineError::Binary),
        _ => Ok(()),
    }
}

/// Runs read-only `git blame --line-porcelain` asynchronously outside
/// [`crate::pool::Git2WorkerPool`] so rapid file switching cancels and kills
/// stale subprocesses without starving interactive git2 workers.
pub(crate) async fn load_blame_async(
    pool: &crate::pool::Git2WorkerPool,
    location: &RepoLocation,
    key: &BlameKey,
    token: &EpochToken,
) -> Result<BlameBundle, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    if key.path.trim().is_empty() {
        return Err(EngineError::Other("empty path for blame".to_string()));
    }

    let loc_pre = location.clone();
    let key_pre = key.clone();
    pool.run_epoch(
        crate::pool::TaskPriority::Interactive,
        token.clone(),
        move |_| precheck_blame_target(&loc_pre, &key_pre),
    )
    .await?;
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }

    let git = crate::cli::require_git_executable()?;
    let mut cmd = non_interactive_std_command(&git, Some(&location.workdir));
    cmd.arg("--git-dir")
        .arg(&location.git_dir)
        .arg("--work-tree")
        .arg(&location.workdir)
        .args(["-c", "core.quotepath=false", "blame", "--line-porcelain"]);
    if key.options.ignore_whitespace {
        cmd.arg("-w");
    }
    cmd.args(key.options.move_detection.git_flags());
    if let Some(rev) = key.rev {
        cmd.arg(rev.to_string());
    }
    cmd.arg("--").arg(&key.path);

    let output = super::run_git_output_cancellable(cmd, None, token).await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(EngineError::Other(if stderr.is_empty() {
            format!("git blame failed with exit {:?}", output.status.code())
        } else {
            stderr
        }));
    }

    let parse_token = token.clone();
    let parsed = tokio::task::spawn_blocking(move || {
        parse_blame_porcelain_cancellable(&output.stdout, || parse_token.is_stale())
    })
    .await
    .map_err(|_| EngineError::Cancelled)??;
    Ok(BlameBundle {
        key: key.clone(),
        lines: parsed.lines,
        commits: parsed.commits,
        content: parsed.content,
        uncommitted_oid_present: parsed.uncommitted_oid_present,
    })
}
