use std::path::Path;

use git2::Repository;
use imara_diff::{Algorithm, Diff, InternedInput};
use jbgit_core::{
    DetailsKey, DetailsMode, DiffHunk, DiffLine, DiffLineKind, EAGER_WORD_DIFF_PAIR_BUDGET,
    EpochToken, FileDiff, FileDiffKey, FileStatusKind, MAX_DIFF_FILE_BYTES, Oid,
    apply_word_diff_with_budget,
};

use crate::error::EngineError;
use crate::pool::with_thread_repo;
use crate::probe::RepoLocation;

use super::commit_changes::{ResolvedChange, resolve_changes};
use super::to_git2_oid;

/// Diff preview line budget before truncation.
const MAX_DIFF_LINES: usize = 20_000;

pub(super) enum ReadBytes {
    Loaded { bytes: Vec<u8>, is_binary: bool },
    TooLarge,
}

pub(super) fn read_blob_checked(repo: &Repository, oid: git2::Oid) -> ReadBytes {
    if oid.is_zero() {
        return ReadBytes::Loaded {
            bytes: Vec::new(),
            is_binary: false,
        };
    }
    if let Ok(odb) = repo.odb()
        && let Ok((size, _)) = odb.read_header(oid)
        && size > MAX_DIFF_FILE_BYTES
    {
        return ReadBytes::TooLarge;
    }
    match repo.find_blob(oid) {
        Ok(blob) => {
            if blob.size() > MAX_DIFF_FILE_BYTES {
                return ReadBytes::TooLarge;
            }
            let content = blob.content();
            let is_bin = blob.is_binary() || content.contains(&0);
            ReadBytes::Loaded {
                bytes: content.to_vec(),
                is_binary: is_bin,
            }
        }
        Err(_) => ReadBytes::Loaded {
            bytes: Vec::new(),
            is_binary: false,
        },
    }
}

pub(super) fn read_new_bytes_checked(
    repo: &Repository,
    key: &DetailsKey,
    item: &ResolvedChange,
) -> ReadBytes {
    if key.mode == DetailsMode::CompareWithLocal
        && item.new_oid.is_zero()
        && item.kind != FileStatusKind::Deleted
    {
        return read_workdir_path_checked(repo, &item.path);
    }
    read_blob_checked(repo, item.new_oid)
}

fn read_tree_path_checked(repo: &Repository, tree: &git2::Tree<'_>, path: &str) -> ReadBytes {
    let Ok(entry) = tree.get_path(Path::new(path)) else {
        return ReadBytes::Loaded {
            bytes: Vec::new(),
            is_binary: false,
        };
    };
    read_blob_checked(repo, entry.id())
}

fn read_workdir_path_checked(repo: &Repository, path: &str) -> ReadBytes {
    let Some(workdir) = repo.workdir() else {
        return ReadBytes::Loaded {
            bytes: Vec::new(),
            is_binary: false,
        };
    };
    let full = workdir.join(path);
    if let Ok(meta) = std::fs::metadata(&full)
        && meta.len() > MAX_DIFF_FILE_BYTES as u64
    {
        return ReadBytes::TooLarge;
    }
    match std::fs::read(&full) {
        Ok(bytes) => {
            if bytes.len() > MAX_DIFF_FILE_BYTES {
                return ReadBytes::TooLarge;
            }
            let is_bin = bytes.contains(&0);
            ReadBytes::Loaded {
                bytes,
                is_binary: is_bin,
            }
        }
        Err(_) => ReadBytes::Loaded {
            bytes: Vec::new(),
            is_binary: false,
        },
    }
}

pub(super) fn imara_line_stats(
    old: &[u8],
    new: &[u8],
    token: &EpochToken,
) -> Result<(usize, usize), EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    let old_s = String::from_utf8_lossy(old);
    let new_s = String::from_utf8_lossy(new);
    let input = InternedInput::new(old_s.as_ref(), new_s.as_ref());
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    Ok((
        diff.count_additions() as usize,
        diff.count_removals() as usize,
    ))
}

fn build_imara_hunks(
    old: &[u8],
    new: &[u8],
    context_len: u32,
    token: &EpochToken,
) -> Result<(Vec<DiffHunk>, bool), EngineError> {
    let old_s = String::from_utf8_lossy(old);
    let new_s = String::from_utf8_lossy(new);
    let input = InternedInput::new(old_s.as_ref(), new_s.as_ref());
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);

    let before_len = input.before.len() as u32;
    let raw_hunks: Vec<_> = diff.hunks().collect();
    if raw_hunks.is_empty() {
        return Ok((Vec::new(), false));
    }

    let clean_line = |tok| {
        input.interner[tok]
            .trim_end_matches(['\n', '\r'])
            .replace('\t', "    ")
    };

    let mut out = Vec::new();
    let mut budget = MAX_DIFF_LINES;
    let mut word_diff_budget = EAGER_WORD_DIFF_PAIR_BUDGET;
    let mut truncated = false;

    let mut i = 0usize;
    while i < raw_hunks.len() {
        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }
        let first = &raw_hunks[i];
        let mut group_end = i + 1;
        while group_end < raw_hunks.len() {
            let prev_end = raw_hunks[group_end - 1].before.end;
            let next_start = raw_hunks[group_end].before.start;
            if next_start.saturating_sub(prev_end) <= context_len.saturating_mul(2) {
                group_end += 1;
            } else {
                break;
            }
        }

        let start_before = first.before.start.saturating_sub(context_len);
        let leading = first.before.start - start_before;
        let start_after = first.after.start.saturating_sub(leading);

        let mut lines = Vec::new();
        let mut cur_before = start_before;
        let mut cur_after = start_after;

        for h in &raw_hunks[i..group_end] {
            while cur_before < h.before.start {
                if budget == 0 {
                    truncated = true;
                    break;
                }
                budget -= 1;
                lines.push(DiffLine {
                    kind: DiffLineKind::Context,
                    old_lineno: Some(cur_before + 1),
                    new_lineno: Some(cur_after + 1),
                    text: clean_line(input.before[cur_before as usize]),
                    word_ranges: Vec::new(),
                });
                cur_before += 1;
                cur_after += 1;
            }
            if truncated {
                break;
            }
            for b_idx in h.before.start..h.before.end {
                if budget == 0 {
                    truncated = true;
                    break;
                }
                budget -= 1;
                lines.push(DiffLine {
                    kind: DiffLineKind::Deleted,
                    old_lineno: Some(b_idx + 1),
                    new_lineno: None,
                    text: clean_line(input.before[b_idx as usize]),
                    word_ranges: Vec::new(),
                });
            }
            for a_idx in h.after.start..h.after.end {
                if budget == 0 {
                    truncated = true;
                    break;
                }
                budget -= 1;
                lines.push(DiffLine {
                    kind: DiffLineKind::Added,
                    old_lineno: None,
                    new_lineno: Some(a_idx + 1),
                    text: clean_line(input.after[a_idx as usize]),
                    word_ranges: Vec::new(),
                });
            }
            if truncated {
                break;
            }
            cur_before = h.before.end;
            cur_after = h.after.end;
        }

        if !truncated {
            let tail_end = cur_before.saturating_add(context_len).min(before_len);
            while cur_before < tail_end {
                if budget == 0 {
                    truncated = true;
                    break;
                }
                budget -= 1;
                lines.push(DiffLine {
                    kind: DiffLineKind::Context,
                    old_lineno: Some(cur_before + 1),
                    new_lineno: Some(cur_after + 1),
                    text: clean_line(input.before[cur_before as usize]),
                    word_ranges: Vec::new(),
                });
                cur_before += 1;
                cur_after += 1;
            }
        }

        let len_before = cur_before - start_before;
        let len_after = cur_after - start_after;
        let mut dh = DiffHunk {
            header: format!(
                "@@ -{},{} +{},{} @@",
                start_before + 1,
                len_before,
                start_after + 1,
                len_after
            ),
            lines,
        };
        apply_word_diff_with_budget(&mut dh, &mut word_diff_budget);
        out.push(dh);
        if truncated {
            break;
        }
        i = group_end;
    }

    Ok((out, truncated))
}

fn populate_file_diff(
    mut out: FileDiff,
    old: ReadBytes,
    new: ReadBytes,
    context_lines: u32,
    token: &EpochToken,
) -> Result<FileDiff, EngineError> {
    let (
        ReadBytes::Loaded {
            bytes: old_bytes,
            is_binary: old_bin,
        },
        ReadBytes::Loaded {
            bytes: new_bytes,
            is_binary: new_bin,
        },
    ) = (old, new)
    else {
        out.is_too_large = true;
        return Ok(out);
    };
    if old_bin || new_bin {
        out.is_binary = true;
        return Ok(out);
    }
    let (hunks, is_truncated) = build_imara_hunks(&old_bytes, &new_bytes, context_lines, token)?;
    out.hunks = hunks;
    out.is_truncated = is_truncated;
    Ok(out)
}

pub fn load_file_diff_sync(
    location: &RepoLocation,
    key: &FileDiffKey,
    token: &EpochToken,
) -> Result<FileDiff, EngineError> {
    load_file_diff_with_oids_sync(location, key, None, token)
}

pub fn load_file_diff_with_oids_sync(
    location: &RepoLocation,
    key: &FileDiffKey,
    known_oids: Option<(Option<Oid>, Option<Oid>)>,
    token: &EpochToken,
) -> Result<FileDiff, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }

    with_thread_repo(&location.git_dir, |repo| {
        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }
        let mut out = FileDiff {
            key: key.clone(),
            hunks: Vec::new(),
            is_binary: false,
            is_truncated: false,
            is_too_large: false,
        };
        let context_lines = key.context.lines();
        let lookup_old = key.old_path.as_deref().unwrap_or(&key.path);

        let read_opt_oid = |opt: Option<Oid>| -> Result<ReadBytes, EngineError> {
            match opt {
                Some(id) => Ok(read_blob_checked(repo, to_git2_oid(&id)?)),
                None => Ok(ReadBytes::Loaded {
                    bytes: Vec::new(),
                    is_binary: false,
                }),
            }
        };

        match key.details.mode {
            DetailsMode::CompareVersions if key.details.oids.len() >= 2 => {
                if let Some((old_id, new_id)) = known_oids {
                    return populate_file_diff(
                        out,
                        read_opt_oid(old_id)?,
                        read_opt_oid(new_id)?,
                        context_lines,
                        token,
                    );
                }
                let base = repo.find_commit(to_git2_oid(&key.details.oids[0])?)?;
                let target = repo.find_commit(to_git2_oid(&key.details.oids[1])?)?;
                let base_tree = base.tree()?;
                let target_tree = target.tree()?;
                let old = read_tree_path_checked(repo, &base_tree, lookup_old);
                let new = read_tree_path_checked(repo, &target_tree, &key.path);
                return populate_file_diff(out, old, new, context_lines, token);
            }
            DetailsMode::CompareWithLocal if !key.details.oids.is_empty() => {
                let old = match known_oids {
                    Some((old_id, _)) => read_opt_oid(old_id)?,
                    None => {
                        let base = repo.find_commit(to_git2_oid(&key.details.oids[0])?)?;
                        let base_tree = base.tree()?;
                        read_tree_path_checked(repo, &base_tree, lookup_old)
                    }
                };
                let new = read_workdir_path_checked(repo, &key.path);
                return populate_file_diff(out, old, new, context_lines, token);
            }
            DetailsMode::Commits if key.details.oids.len() == 1 => {
                if let Some((old_id, new_id)) = known_oids {
                    return populate_file_diff(
                        out,
                        read_opt_oid(old_id)?,
                        read_opt_oid(new_id)?,
                        context_lines,
                        token,
                    );
                }
                let commit = repo.find_commit(to_git2_oid(&key.details.oids[0])?)?;
                let new_tree = commit.tree()?;
                let parent_idx = if key.details.parent_index < commit.parent_count() {
                    key.details.parent_index
                } else {
                    0
                };
                let old_tree = commit.parent(parent_idx).ok().and_then(|p| p.tree().ok());
                let old = old_tree
                    .as_ref()
                    .map(|t| read_tree_path_checked(repo, t, lookup_old))
                    .unwrap_or_else(|| ReadBytes::Loaded {
                        bytes: Vec::new(),
                        is_binary: false,
                    });
                let new = read_tree_path_checked(repo, &new_tree, &key.path);
                return populate_file_diff(out, old, new, context_lines, token);
            }
            DetailsMode::Commits if !key.details.oids.is_empty() => {
                if let Some((old_id, new_id)) = known_oids {
                    return populate_file_diff(
                        out,
                        read_opt_oid(old_id)?,
                        read_opt_oid(new_id)?,
                        context_lines,
                        token,
                    );
                }
            }
            _ => {}
        }

        let (resolved, commits) = resolve_changes(repo, &key.details, token)?;
        let Some(item) = resolved.into_iter().find(|c| c.path == key.path) else {
            return Ok(out);
        };
        if item.is_binary {
            out.is_binary = true;
            return Ok(out);
        }
        let old = if item.old_oid.is_zero()
            && key.details.mode == DetailsMode::Commits
            && commits.len() == 1
            && let Some(old_path) = key.old_path.as_deref().filter(|old| *old != key.path)
            && let Some(parent_tree) = commits[0]
                .parent(key.details.parent_index)
                .ok()
                .and_then(|p| p.tree().ok())
        {
            read_tree_path_checked(repo, &parent_tree, old_path)
        } else {
            read_blob_checked(repo, item.old_oid)
        };
        let new = read_new_bytes_checked(repo, &key.details, &item);
        populate_file_diff(out, old, new, context_lines, token)
    })
}
