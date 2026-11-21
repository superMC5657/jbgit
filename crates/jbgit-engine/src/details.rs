//! git2 + imara-diff loaders for the Log tab's right-hand panes: commit
//! metadata + changed files (`CommitDetailsLoader`), single-file diffs
//! (`FrameDiffPreview`) and path-filter matches (`VcsLogFileFilter`).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};

use git2::{Delta, DiffFindOptions, DiffOptions, Repository};
use imara_diff::{Algorithm, Diff, InternedInput};
use jbgit_core::{
    CommitChangesBundle, CommitDetails, DetailsKey, DetailsMode, DiffHunk, DiffLine, DiffLineKind,
    EAGER_WORD_DIFF_PAIR_BUDGET, EpochToken, FileChange, FileDiff, FileDiffKey, FileHistoryBundle,
    FileHistoryEntry, FileHistoryKey, FileStatusKind, MAX_DIFF_FILE_BYTES, Oid, PersonStamp,
    RepoSnapshot, SignatureStatus, apply_word_diff_with_budget,
};

use crate::error::EngineError;
use crate::pool::{delta_to_kind, with_thread_repo};
use crate::probe::{RepoLocation, to_core_oid};

/// Per-file line statistics are skipped beyond this many files (huge merges).
const MAX_STAT_FILES: usize = 2_000;
/// Diff preview line budget before truncation.
const MAX_DIFF_LINES: usize = 20_000;
/// Default max commits loaded for a single-file history query.
pub const DEFAULT_FILE_HISTORY_LIMIT: usize = 500;

fn to_git2_oid(oid: &Oid) -> Result<git2::Oid, EngineError> {
    git2::Oid::from_bytes(oid.as_bytes()).map_err(EngineError::from)
}

fn person(sig: &git2::Signature<'_>) -> PersonStamp {
    PersonStamp {
        name: sig.name().unwrap_or("Unknown").to_string(),
        email: sig.email().unwrap_or("").to_string(),
        time: sig.when().seconds(),
        offset_minutes: sig.when().offset_minutes(),
    }
}

fn delta_path(file: git2::DiffFile<'_>) -> Option<String> {
    file.path().map(|p| p.to_string_lossy().replace('\\', "/"))
}

pub fn verify_commit_signature_sync(git_dir: &Path, oid: Oid) -> SignatureStatus {
    let mut cmd = Command::new("git");
    cmd.arg("--git-dir")
        .arg(git_dir)
        .args(["log", "-1", "--format=%G?", &oid.to_string()])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    match cmd.output() {
        Ok(o) if o.status.success() => {
            if String::from_utf8_lossy(&o.stdout).trim() == "G" {
                SignatureStatus::Verified
            } else {
                SignatureStatus::SignedUnverified
            }
        }
        _ => SignatureStatus::SignedUnverified,
    }
}

#[derive(Clone)]
struct BlobVersion {
    path: Option<String>,
    oid: git2::Oid,
}

#[derive(Clone)]
struct ResolvedChange {
    path: String,
    old_path: Option<String>,
    kind: FileStatusKind,
    old_oid: git2::Oid,
    new_oid: git2::Oid,
    is_binary: bool,
}

struct NetEntry {
    before: BlobVersion,
    after: BlobVersion,
    is_binary: bool,
}

fn diff_deltas_to_changes(
    diff: &mut git2::Diff<'_>,
    token: &EpochToken,
) -> Result<Vec<ResolvedChange>, EngineError> {
    diff.find_similar(Some(DiffFindOptions::new().renames(true)))?;
    let mut out = Vec::with_capacity(diff.deltas().len());
    for (idx, delta) in diff.deltas().enumerate() {
        if idx % 64 == 0 && token.is_stale() {
            return Err(EngineError::Cancelled);
        }
        let Some(kind) = delta_to_kind(delta.status()) else {
            continue;
        };
        let path = delta_path(delta.new_file())
            .or_else(|| delta_path(delta.old_file()))
            .unwrap_or_default();
        let old_path = delta_path(delta.old_file()).filter(|old| {
            matches!(kind, FileStatusKind::Renamed | FileStatusKind::Copied) && *old != path
        });
        out.push(ResolvedChange {
            path,
            old_path,
            kind,
            old_oid: delta.old_file().id(),
            new_oid: delta.new_file().id(),
            is_binary: delta.flags().is_binary(),
        });
    }
    Ok(out)
}

fn resolve_single_commit_changes(
    repo: &Repository,
    commit: &git2::Commit<'_>,
    parent_index: usize,
    token: &EpochToken,
) -> Result<Vec<ResolvedChange>, EngineError> {
    let parent_idx = if parent_index < commit.parent_count() {
        parent_index
    } else {
        0
    };
    let base = commit.parent(parent_idx).ok().and_then(|p| p.tree().ok());
    let target = commit.tree()?;
    let mut diff = repo.diff_tree_to_tree(base.as_ref(), Some(&target), None)?;
    diff_deltas_to_changes(&mut diff, token)
}

fn resolve_compare_versions(
    repo: &Repository,
    base: &git2::Commit<'_>,
    target: &git2::Commit<'_>,
    token: &EpochToken,
) -> Result<Vec<ResolvedChange>, EngineError> {
    let base_tree = base.tree()?;
    let target_tree = target.tree()?;
    let mut diff = repo.diff_tree_to_tree(Some(&base_tree), Some(&target_tree), None)?;
    diff_deltas_to_changes(&mut diff, token)
}

fn resolve_compare_with_local(
    repo: &Repository,
    base: &git2::Commit<'_>,
    token: &EpochToken,
) -> Result<Vec<ResolvedChange>, EngineError> {
    let base_tree = base.tree()?;
    let mut opts = DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);
    let mut diff = repo.diff_tree_to_workdir_with_index(Some(&base_tree), Some(&mut opts))?;
    diff_deltas_to_changes(&mut diff, token)
}

/// Folds per-commit changes oldest-to-newest (`VcsLogUtil.collectChanges` /
/// `zipChanges` semantics) so interleaved branches in the visible table never
/// leak unrelated files into the net change set.
fn resolve_multi_commit_changes(
    repo: &Repository,
    commits: &[git2::Commit<'_>],
    token: &EpochToken,
) -> Result<Vec<ResolvedChange>, EngineError> {
    let mut entries: HashMap<String, NetEntry> = HashMap::new();

    for commit in commits.iter().rev() {
        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }
        let base = commit.parent(0).ok().and_then(|p| p.tree().ok());
        let target = commit.tree()?;
        let mut diff = repo.diff_tree_to_tree(base.as_ref(), Some(&target), None)?;
        diff.find_similar(Some(DiffFindOptions::new().renames(true)))?;

        for (idx, delta) in diff.deltas().enumerate() {
            if idx % 64 == 0 && token.is_stale() {
                return Err(EngineError::Cancelled);
            }
            if delta_to_kind(delta.status()).is_none() {
                continue;
            }
            let old_p = delta_path(delta.old_file());
            let new_p = delta_path(delta.new_file());
            let old_id = delta.old_file().id();
            let new_id = delta.new_file().id();
            let is_bin = delta.flags().is_binary();

            let lookup = if delta.status() == Delta::Added {
                new_p.as_deref()
            } else {
                old_p.as_deref()
            };
            let existing = lookup.and_then(|k| entries.remove(k));
            let key_path = new_p.clone().or_else(|| old_p.clone()).unwrap_or_default();

            let entry = match existing {
                Some(mut prev) => {
                    prev.after = BlobVersion {
                        path: (!new_id.is_zero()).then(|| key_path.clone()),
                        oid: new_id,
                    };
                    prev.is_binary |= is_bin;
                    prev
                }
                None => NetEntry {
                    before: BlobVersion {
                        path: (!old_id.is_zero()).then_some(old_p).flatten(),
                        oid: old_id,
                    },
                    after: BlobVersion {
                        path: (!new_id.is_zero()).then(|| key_path.clone()),
                        oid: new_id,
                    },
                    is_binary: is_bin,
                },
            };
            entries.insert(key_path, entry);
        }
    }

    let mut out = Vec::new();
    for (key_path, entry) in entries {
        if entry.before.oid == entry.after.oid && entry.before.path == entry.after.path {
            continue;
        }
        let (kind, path, old_path) = if entry.before.oid.is_zero() {
            (FileStatusKind::Added, key_path, None)
        } else if entry.after.oid.is_zero() {
            (
                FileStatusKind::Deleted,
                entry.before.path.unwrap_or(key_path),
                None,
            )
        } else {
            let p = entry.after.path.unwrap_or(key_path);
            let old = entry.before.path.filter(|o| *o != p);
            let kind = if old.is_some() {
                FileStatusKind::Renamed
            } else {
                FileStatusKind::Modified
            };
            (kind, p, old)
        };
        out.push(ResolvedChange {
            path,
            old_path,
            kind,
            old_oid: entry.before.oid,
            new_oid: entry.after.oid,
            is_binary: entry.is_binary,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn resolve_changes<'r>(
    repo: &'r Repository,
    key: &DetailsKey,
    token: &EpochToken,
) -> Result<(Vec<ResolvedChange>, Vec<git2::Commit<'r>>), EngineError> {
    let mut commits = Vec::with_capacity(key.oids.len());
    for oid in &key.oids {
        commits.push(repo.find_commit(to_git2_oid(oid)?)?);
    }
    let Some(first) = commits.first() else {
        return Err(EngineError::Other("empty commit selection".to_string()));
    };
    let changes = match key.mode {
        DetailsMode::CompareVersions => {
            let second = commits.get(1).ok_or_else(|| {
                EngineError::Other("CompareVersions requires 2 commits".to_string())
            })?;
            resolve_compare_versions(repo, first, second, token)?
        }
        DetailsMode::CompareWithLocal => resolve_compare_with_local(repo, first, token)?,
        DetailsMode::Commits => {
            if commits.len() == 1 {
                resolve_single_commit_changes(repo, first, key.parent_index, token)?
            } else {
                resolve_multi_commit_changes(repo, &commits, token)?
            }
        }
    };
    Ok((changes, commits))
}

enum ReadBytes {
    Loaded { bytes: Vec<u8>, is_binary: bool },
    TooLarge,
}

fn read_blob_checked(repo: &Repository, oid: git2::Oid) -> ReadBytes {
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

fn read_new_bytes_checked(
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

fn imara_line_stats(old: &[u8], new: &[u8], token: &EpochToken) -> Result<(usize, usize), EngineError> {
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
    Ok((diff.count_additions() as usize, diff.count_removals() as usize))
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

    // Group adjacent diff hunks whose context windows touch or overlap.
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

pub fn load_commit_details_sync(
    location: &RepoLocation,
    key: &DetailsKey,
    token: &EpochToken,
) -> Result<CommitChangesBundle, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }

    with_thread_repo(&location.git_dir, |repo| {
        let (resolved, commits) = resolve_changes(repo, key, token)?;

        let details = commits
            .iter()
            .map(|c| {
                // Defer `git log --format=%G?` subprocess verification to background task so
                // commit details and changed files appear immediately without stalling on GPG.
                let signature = if repo.extract_signature(&c.id(), None).is_ok() {
                    SignatureStatus::SignedUnverified
                } else {
                    SignatureStatus::Unsigned
                };
                CommitDetails {
                    oid: to_core_oid(c.id()),
                    parents: c.parent_ids().map(to_core_oid).collect(),
                    author: person(&c.author()),
                    committer: person(&c.committer()),
                    message: String::from_utf8_lossy(c.message_bytes())
                        .trim_end()
                        .to_string(),
                    signature,
                }
            })
            .collect();

        let workdir = repo.workdir();
        let opt_core = |id: git2::Oid| (!id.is_zero()).then(|| to_core_oid(id));
        let mut changes = Vec::with_capacity(resolved.len());
        for (idx, item) in resolved.into_iter().enumerate() {
            if idx % 32 == 0 && token.is_stale() {
                return Err(EngineError::Cancelled);
            }
            let mut additions = 0usize;
            let mut deletions = 0usize;
            let mut is_binary = item.is_binary;
            if idx < MAX_STAT_FILES && !is_binary
                && let (
                    ReadBytes::Loaded {
                        bytes: old_bytes,
                        is_binary: old_bin,
                    },
                    ReadBytes::Loaded {
                        bytes: new_bytes,
                        is_binary: new_bin,
                    },
                ) = (
                    read_blob_checked(repo, item.old_oid),
                    read_new_bytes_checked(repo, key, &item),
                )
            {
                is_binary |= old_bin || new_bin;
                if !is_binary {
                    (additions, deletions) = imara_line_stats(&old_bytes, &new_bytes, token)?;
                }
            }
            let exists_in_workdir = workdir.is_some_and(|w| w.join(&item.path).is_file());
            changes.push(FileChange {
                path: item.path,
                old_path: item.old_path,
                kind: item.kind,
                additions,
                deletions,
                is_binary,
                exists_in_workdir,
                old_oid: opt_core(item.old_oid),
                new_oid: opt_core(item.new_oid),
            });
        }

        Ok(CommitChangesBundle {
            key: key.clone(),
            details,
            changes,
        })
    })
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

        // Fallback only when multi-commit range is queried without cached FileChange OIDs.
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

/// Commits (from `snapshot`) touching any of `paths`. A merge matches only
/// when it differs from every parent (`--simplify-merges` TREESAME rule).
pub fn load_path_matches_sync(
    location: &RepoLocation,
    paths: &[String],
    snapshot: &RepoSnapshot,
    token: &EpochToken,
) -> Result<HashSet<Oid>, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }

    with_thread_repo(&location.git_dir, |repo| {
        let mut out = HashSet::new();
        let touches = |parent: Option<&git2::Tree<'_>>, tree: &git2::Tree<'_>| -> bool {
            let mut opts = DiffOptions::new();
            for p in paths {
                opts.pathspec(p);
            }
            repo.diff_tree_to_tree(parent, Some(tree), Some(&mut opts))
                .map(|d| d.deltas().len() > 0)
                .unwrap_or(false)
        };
        for (idx, summary) in snapshot.commits.iter().enumerate() {
            if idx % 64 == 0 && token.is_stale() {
                return Err(EngineError::Cancelled);
            }
            let Ok(commit) = repo.find_commit(to_git2_oid(&summary.oid)?) else {
                continue;
            };
            let Ok(tree) = commit.tree() else {
                continue;
            };
            let matched = if commit.parent_count() == 0 {
                touches(None, &tree)
            } else {
                commit
                    .parents()
                    .all(|p| p.tree().is_ok_and(|pt| touches(Some(&pt), &tree)))
            };
            if matched {
                out.insert(summary.oid);
            }
        }
        Ok(out)
    })
}

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
                        // Git `-z --numstat` emits `add\tdel\t\0old_path\0new_path\0` for renames/copies.
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

    // Backfill empty `path_at_commit` (e.g. merge commits without diff body) from neighbors.
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

    let mut cmd = Command::new("git");
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
    ])
    .current_dir(&location.workdir)
    .env("GIT_OPTIONAL_LOCKS", "0")
    .env("GIT_TERMINAL_PROMPT", "0")
    .env("GIT_PAGER", "cat")
    .env("LC_ALL", "C.UTF-8")
    .stdin(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_oid(fill: u8) -> (String, Oid) {
        let oid = Oid::from_bytes([fill; 20]);
        (oid.to_string(), oid)
    }

    #[test]
    fn parses_added_modified_deleted_typechange_and_spaces_cjk() {
        let (h1, o1) = hex_oid(0x11);
        let (h2, o2) = hex_oid(0x22);
        let (h3, o3) = hex_oid(0x33);
        let (h4, o4) = hex_oid(0x44);
        let mut raw = Vec::new();
        raw.extend_from_slice(
            format!(
                "\x1e{h4}\x1fAlice\x1f400\x1fdelete file\x1f\x00\n:100644 000000 1111111 0000000 D\x00dir with space/中文 文件.rs\x000\t5\tdir with space/中文 文件.rs\x00"
            )
            .as_bytes(),
        );
        raw.extend_from_slice(
            format!(
                "\x1e{h3}\x1fBob\x1f300\x1ftypechange\x1f\x00\n:100644 120000 1111111 2222222 T\x00dir with space/中文 文件.rs\x000\t0\tdir with space/中文 文件.rs\x00"
            )
            .as_bytes(),
        );
        raw.extend_from_slice(
            format!(
                "\x1e{h2}\x1fCarol\x1f200\x1fmodify\x1f\x00\n:100644 100644 1111111 2222222 M\x00dir with space/中文 文件.rs\x003\t1\tdir with space/中文 文件.rs\x00"
            )
            .as_bytes(),
        );
        raw.extend_from_slice(
            format!(
                "\x1e{h1}\x1fDave\x1f100\x1fadd\x1f\x00\n:000000 100644 0000000 1111111 A\x00dir with space/中文 文件.rs\x005\t0\tdir with space/中文 文件.rs\x00"
            )
            .as_bytes(),
        );

        let (entries, truncated) = parse_file_history_z(&raw, 50);
        assert!(!truncated);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].oid, o4);
        assert_eq!(entries[0].kind, FileStatusKind::Deleted);
        assert_eq!(entries[0].deletions, 5);
        assert_eq!(entries[0].path_at_commit, "dir with space/中文 文件.rs");

        assert_eq!(entries[1].oid, o3);
        assert_eq!(entries[1].kind, FileStatusKind::Typechange);
        assert_eq!(entries[1].additions, 0);
        assert_eq!(entries[1].deletions, 0);

        assert_eq!(entries[2].oid, o2);
        assert_eq!(entries[2].kind, FileStatusKind::Modified);
        assert_eq!((entries[2].additions, entries[2].deletions), (3, 1));

        assert_eq!(entries[3].oid, o1);
        assert_eq!(entries[3].kind, FileStatusKind::Added);
        assert_eq!((entries[3].additions, entries[3].deletions), (5, 0));
    }

    #[test]
    fn parses_rename_binary_merge_and_truncation() {
        let (h1, o1) = hex_oid(0xaa);
        let (h2, o2) = hex_oid(0xbb);
        let (h3, o3) = hex_oid(0xcc);
        let (h4, _o4) = hex_oid(0xdd);
        let mut raw = Vec::new();
        // Record 1: binary
        raw.extend_from_slice(
            format!(
                "\x1e{h1}\x1fAlice\x1f40\x1fbin\x1f\x00\n:100644 100644 111 222 M\x00sub/new name.bin\x00-\t-\tsub/new name.bin\x00"
            )
            .as_bytes(),
        );
        // Record 2: merge without diff body
        raw.extend_from_slice(
            format!("\x1e{h2}\x1fAlice\x1f30\x1fmerge\x1f\x00").as_bytes(),
        );
        // Record 3: rename with R092 and `-z --numstat` empty path field
        raw.extend_from_slice(
            format!(
                "\x1e{h3}\x1fAlice\x1f20\x1frename\x1f\x00\n:100644 100644 111 222 R092\x00old name.txt\x00sub/new name.bin\x001\t2\t\x00old name.txt\x00sub/new name.bin\x00"
            )
            .as_bytes(),
        );
        // Record 4: extra record to trigger truncation when max_entries == 3
        raw.extend_from_slice(
            format!(
                "\x1e{h4}\x1fAlice\x1f10\x1finit\x1f\x00\n:000000 100644 000 111 A\x00old name.txt\x0010\t0\told name.txt\x00"
            )
            .as_bytes(),
        );

        let (entries, truncated) = parse_file_history_z(&raw, 3);
        assert!(truncated);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].oid, o1);
        assert!(entries[0].is_binary);
        assert_eq!((entries[0].additions, entries[0].deletions), (0, 0));

        assert_eq!(entries[1].oid, o2);
        assert_eq!(entries[1].path_at_commit, "sub/new name.bin");

        assert_eq!(entries[2].oid, o3);
        assert_eq!(entries[2].kind, FileStatusKind::Renamed);
        assert_eq!(entries[2].old_path.as_deref(), Some("old name.txt"));
        assert_eq!(entries[2].path_at_commit, "sub/new name.bin");
        assert_eq!((entries[2].additions, entries[2].deletions), (1, 2));
    }
}
