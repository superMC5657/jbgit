//! git2 + imara-diff loaders for the Log tab's right-hand panes: commit
//! metadata + changed files (`CommitDetailsLoader`), single-file diffs
//! (`FrameDiffPreview`) and path-filter matches (`VcsLogFileFilter`).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};

use git2::{Delta, DiffFindOptions, DiffOptions, Repository};
use imara_diff::{Algorithm, Diff, InternedInput};
use jgit_core::{
    CommitChangesBundle, CommitDetails, DetailsKey, DetailsMode, DiffHunk, DiffLine, DiffLineKind,
    EpochToken, FileChange, FileDiff, FileDiffKey, FileStatusKind, Oid, PersonStamp, RepoSnapshot,
    SignatureStatus, apply_word_diff,
};

use crate::error::EngineError;
use crate::pool::{delta_to_kind, with_thread_repo};
use crate::probe::{RepoLocation, to_core_oid};

/// Per-file line statistics are skipped beyond this many files (huge merges).
const MAX_STAT_FILES: usize = 2_000;
/// Diff preview line budget before truncation.
const MAX_DIFF_LINES: usize = 20_000;

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

fn verify_commit_signature(git_dir: &Path, oid: git2::Oid) -> SignatureStatus {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(["log", "-1", "--format=%G?", &oid.to_string()])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output();
    match out {
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

fn read_blob_bytes(repo: &Repository, oid: git2::Oid) -> (Vec<u8>, bool) {
    if oid.is_zero() {
        return (Vec::new(), false);
    }
    match repo.find_blob(oid) {
        Ok(blob) => {
            let is_bin = blob.is_binary() || blob.content().contains(&0);
            (blob.content().to_vec(), is_bin)
        }
        Err(_) => (Vec::new(), false),
    }
}

fn read_new_bytes(repo: &Repository, key: &DetailsKey, item: &ResolvedChange) -> (Vec<u8>, bool) {
    if key.mode == DetailsMode::CompareWithLocal
        && item.new_oid.is_zero()
        && item.kind != FileStatusKind::Deleted
        && let Some(workdir) = repo.workdir()
        && let Ok(bytes) = std::fs::read(workdir.join(&item.path))
    {
        let is_bin = bytes.contains(&0);
        return (bytes, is_bin);
    }
    read_blob_bytes(repo, item.new_oid)
}

fn imara_line_stats(old: &[u8], new: &[u8]) -> (usize, usize) {
    let old_s = String::from_utf8_lossy(old);
    let new_s = String::from_utf8_lossy(new);
    let input = InternedInput::new(old_s.as_ref(), new_s.as_ref());
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    (diff.count_additions() as usize, diff.count_removals() as usize)
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
        apply_word_diff(&mut dh);
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
                let signature = if repo.extract_signature(&c.id(), None).is_ok() {
                    verify_commit_signature(repo.path(), c.id())
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

        let mut changes = Vec::with_capacity(resolved.len());
        for (idx, item) in resolved.into_iter().enumerate() {
            if idx % 64 == 0 && token.is_stale() {
                return Err(EngineError::Cancelled);
            }
            let mut additions = 0usize;
            let mut deletions = 0usize;
            let mut is_binary = item.is_binary;
            if idx < MAX_STAT_FILES && !is_binary {
                let (old_bytes, old_bin) = read_blob_bytes(repo, item.old_oid);
                let (new_bytes, new_bin) = read_new_bytes(repo, key, &item);
                is_binary |= old_bin || new_bin;
                if !is_binary {
                    (additions, deletions) = imara_line_stats(&old_bytes, &new_bytes);
                }
            }
            changes.push(FileChange {
                path: item.path,
                old_path: item.old_path,
                kind: item.kind,
                additions,
                deletions,
                is_binary,
            });
        }

        Ok(CommitChangesBundle {
            key: key.clone(),
            details,
            changes,
        })
    })
}

pub fn load_file_diff_sync(
    location: &RepoLocation,
    key: &FileDiffKey,
    token: &EpochToken,
) -> Result<FileDiff, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }

    with_thread_repo(&location.git_dir, |repo| {
        let (resolved, _) = resolve_changes(repo, &key.details, token)?;
        let mut out = FileDiff {
            key: key.clone(),
            hunks: Vec::new(),
            is_binary: false,
            is_truncated: false,
        };
        let Some(item) = resolved.into_iter().find(|c| c.path == key.path) else {
            return Ok(out);
        };
        if item.is_binary {
            out.is_binary = true;
            return Ok(out);
        }
        let (old_bytes, old_bin) = read_blob_bytes(repo, item.old_oid);
        let (new_bytes, new_bin) = read_new_bytes(repo, &key.details, &item);
        if old_bin || new_bin {
            out.is_binary = true;
            return Ok(out);
        }
        let (hunks, is_truncated) =
            build_imara_hunks(&old_bytes, &new_bytes, key.context.lines(), token)?;
        out.hunks = hunks;
        out.is_truncated = is_truncated;
        Ok(out)
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
