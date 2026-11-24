use std::path::Path;

use git2::Repository;
use imara_diff::{Algorithm, Diff, InternedInput};
use jbgit_core::{
    BinaryDiffInfo, DetailsKey, DetailsMode, EpochToken, FileDiff, FileDiffKey, FileModeChange,
    FileStatusKind, LfsDiff, MAX_DIFF_LINES, Oid, build_diff_hunks_with_whitespace,
    parse_lfs_pointer,
};

use crate::blob_reader::{
    CheckedBlob, effective_worktree_mode, path_has_lfs_filter, read_blob, read_tree_path,
    read_worktree_path,
};
use crate::error::EngineError;
use crate::pool::with_thread_repo;
use crate::probe::RepoLocation;

use super::commit_changes::{ResolvedChange, resolve_changes};
use super::to_git2_oid;

pub(super) fn read_new_bytes_checked(
    repo: &Repository,
    key: &DetailsKey,
    item: &ResolvedChange,
) -> CheckedBlob {
    if key.mode == DetailsMode::CompareWithLocal
        && item.new_oid.is_zero()
        && item.kind != FileStatusKind::Deleted
    {
        return repo
            .workdir()
            .map_or(CheckedBlob::Missing, |w| read_worktree_path(w, &item.path));
    }
    read_blob(repo, item.new_oid, item.new_mode)
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

fn tree_entry_mode(tree: &git2::Tree<'_>, path: &str) -> Option<u32> {
    tree.get_path(Path::new(path))
        .ok()
        .map(|e| e.filemode() as u32)
}

fn tree_entry_info(tree: &git2::Tree<'_>, path: &str) -> Option<(git2::Oid, u32)> {
    tree.get_path(Path::new(path))
        .ok()
        .map(|e| (e.id(), e.filemode() as u32))
}

fn lookup_commit_modes(
    repo: &Repository,
    key: &DetailsKey,
    lookup_old: &str,
    path: &str,
) -> (Option<u32>, Option<u32>) {
    match key.mode {
        DetailsMode::CompareVersions if key.oids.len() >= 2 => {
            let old_m = to_git2_oid(&key.oids[0])
                .ok()
                .and_then(|id| repo.find_commit(id).ok())
                .and_then(|c| c.tree().ok())
                .and_then(|t| tree_entry_mode(&t, lookup_old));
            let new_m = to_git2_oid(&key.oids[1])
                .ok()
                .and_then(|id| repo.find_commit(id).ok())
                .and_then(|c| c.tree().ok())
                .and_then(|t| tree_entry_mode(&t, path));
            (old_m, new_m)
        }
        DetailsMode::CompareWithLocal if !key.oids.is_empty() => {
            let old_m = to_git2_oid(&key.oids[0])
                .ok()
                .and_then(|id| repo.find_commit(id).ok())
                .and_then(|c| c.tree().ok())
                .and_then(|t| tree_entry_mode(&t, lookup_old));
            (old_m, None)
        }
        DetailsMode::Commits if key.oids.len() == 1 => {
            let Some(commit) = to_git2_oid(&key.oids[0])
                .ok()
                .and_then(|id| repo.find_commit(id).ok())
            else {
                return (None, None);
            };
            let parent_idx = if key.parent_index < commit.parent_count() {
                key.parent_index
            } else {
                0
            };
            let old_m = commit
                .parent(parent_idx)
                .ok()
                .and_then(|p| p.tree().ok())
                .and_then(|t| tree_entry_mode(&t, lookup_old));
            let new_m = commit
                .tree()
                .ok()
                .and_then(|t| tree_entry_mode(&t, path));
            (old_m, new_m)
        }
        _ => (None, None),
    }
}

/// Resolves accurate `(old_mode, new_mode)` for a multi-commit selection by
/// locating the selected commit(s) whose parent/tree entries match `known_oids`
/// instead of assuming `oids.first()` / `oids.last()`.
///
/// Returns `None` when a present OID cannot be matched directly (e.g. a multi-hop
/// rename chain), so the caller falls back to [`resolve_changes`].
fn lookup_multi_commit_modes(
    repo: &Repository,
    oids: &[Oid],
    lookup_old: &str,
    path: &str,
    old_id: Option<Oid>,
    new_id: Option<Oid>,
) -> Option<(Option<u32>, Option<u32>)> {
    let target_old = match old_id {
        Some(id) => Some(to_git2_oid(&id).ok()?),
        None => None,
    };
    let target_new = match new_id {
        Some(id) => Some(to_git2_oid(&id).ok()?),
        None => None,
    };

    let commits: Vec<git2::Commit<'_>> = oids
        .iter()
        .filter_map(|id| to_git2_oid(id).ok().and_then(|g| repo.find_commit(g).ok()))
        .collect();

    let old_m = match target_old {
        None => None,
        Some(want) => {
            let mut fallback_mode = None;
            let mut matched_changed = None;
            for c in commits.iter().rev() {
                let parent_info = c
                    .parent(0)
                    .ok()
                    .and_then(|p| p.tree().ok())
                    .and_then(|t| tree_entry_info(&t, lookup_old));
                if let Some((pid, pmode)) = parent_info
                    && pid == want
                {
                    if fallback_mode.is_none() {
                        fallback_mode = Some(pmode);
                    }
                    let commit_info = c
                        .tree()
                        .ok()
                        .and_then(|t| tree_entry_info(&t, lookup_old));
                    if commit_info != Some((pid, pmode)) {
                        matched_changed = Some(pmode);
                        break;
                    }
                }
            }
            Some(matched_changed.or(fallback_mode)?)
        }
    };

    let new_m = match target_new {
        None => None,
        Some(want) => {
            let mut fallback_mode = None;
            let mut matched_changed = None;
            for c in &commits {
                let commit_info = c.tree().ok().and_then(|t| tree_entry_info(&t, path));
                if let Some((cid, cmode)) = commit_info
                    && cid == want
                {
                    if fallback_mode.is_none() {
                        fallback_mode = Some(cmode);
                    }
                    let parent_info = c
                        .parent(0)
                        .ok()
                        .and_then(|p| p.tree().ok())
                        .and_then(|t| tree_entry_info(&t, path));
                    if parent_info != Some((cid, cmode)) {
                        matched_changed = Some(cmode);
                        break;
                    }
                }
            }
            Some(matched_changed.or(fallback_mode)?)
        }
    };

    Some((old_m, new_m))
}

fn populate_file_diff(
    mut out: FileDiff,
    old: CheckedBlob,
    new: CheckedBlob,
    mode_change: Option<FileModeChange>,
    lfs_filter: bool,
    context_lines: u32,
    token: &EpochToken,
) -> Result<FileDiff, EngineError> {
    out.mode_change = mode_change;
    let (old_size, old_oid) = old.metadata();
    let (new_size, new_oid) = new.metadata();
    let (Some((old_bytes, old_bin)), Some((new_bytes, new_bin))) = (
        old.into_bytes_or_too_large(),
        new.into_bytes_or_too_large(),
    ) else {
        out.is_too_large = true;
        return Ok(out);
    };
    let old_ptr = parse_lfs_pointer(&old_bytes);
    let new_ptr = parse_lfs_pointer(&new_bytes);
    if old_ptr.is_some() || new_ptr.is_some() {
        out.lfs = Some(LfsDiff {
            old: old_ptr,
            new: new_ptr,
            filter_detected: lfs_filter,
        });
        return Ok(out);
    }
    if old_bin || new_bin {
        out.is_binary = true;
        out.binary = Some(BinaryDiffInfo {
            old_size,
            new_size,
            old_oid,
            new_oid,
        });
        return Ok(out);
    }
    let whitespace = out.key.whitespace;
    let (hunks, is_truncated) = build_diff_hunks_with_whitespace(
        &old_bytes,
        &new_bytes,
        context_lines as usize,
        MAX_DIFF_LINES,
        whitespace,
        || token.is_stale(),
    )
    .ok_or(EngineError::Cancelled)?;
    out.hunks = hunks;
    out.is_truncated = is_truncated;
    Ok(out)
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
        let lfs_filter = path_has_lfs_filter(repo, &key.path);
        let out = FileDiff {
            key: key.clone(),
            hunks: Vec::new(),
            is_binary: false,
            binary: None,
            is_truncated: false,
            is_too_large: false,
            lfs: None,
            mode_change: None,
        };
        let context_lines = key.context.lines();
        let lookup_old = key.old_path.as_deref().unwrap_or(&key.path);

        let read_opt_oid = |opt: Option<Oid>, mode: Option<u32>| -> Result<CheckedBlob, EngineError> {
            match opt {
                Some(id) => Ok(read_blob(repo, to_git2_oid(&id)?, mode)),
                None => Ok(CheckedBlob::Missing),
            }
        };

        match key.details.mode {
            DetailsMode::CompareVersions if key.details.oids.len() >= 2 => {
                if let Some((old_id, new_id)) = known_oids {
                    let (old_m, new_m) = lookup_commit_modes(repo, &key.details, lookup_old, &key.path);
                    let mc = FileModeChange::from_modes(old_m, new_m);
                    return populate_file_diff(
                        out,
                        read_opt_oid(old_id, old_m)?,
                        read_opt_oid(new_id, new_m)?,
                        mc,
                        lfs_filter,
                        context_lines,
                        token,
                    );
                }
                let base = repo.find_commit(to_git2_oid(&key.details.oids[0])?)?;
                let target = repo.find_commit(to_git2_oid(&key.details.oids[1])?)?;
                let base_tree = base.tree()?;
                let target_tree = target.tree()?;
                let old = read_tree_path(repo, &base_tree, lookup_old);
                let new = read_tree_path(repo, &target_tree, &key.path);
                let mc = FileModeChange::from_modes(old.mode(), new.mode());
                return populate_file_diff(out, old, new, mc, lfs_filter, context_lines, token);
            }
            DetailsMode::CompareWithLocal if !key.details.oids.is_empty() => {
                let (old_m, _) = lookup_commit_modes(repo, &key.details, lookup_old, &key.path);
                let old = match known_oids {
                    Some((old_id, _)) => read_opt_oid(old_id, old_m)?,
                    None => {
                        let base = repo.find_commit(to_git2_oid(&key.details.oids[0])?)?;
                        let base_tree = base.tree()?;
                        read_tree_path(repo, &base_tree, lookup_old)
                    }
                };
                let new = repo
                    .workdir()
                    .map_or(CheckedBlob::Missing, |w| read_worktree_path(w, &key.path));
                let eff_new_mode = effective_worktree_mode(repo, new.mode(), old.mode());
                let mc = FileModeChange::from_modes(old.mode(), eff_new_mode);
                return populate_file_diff(out, old, new, mc, lfs_filter, context_lines, token);
            }
            DetailsMode::Commits if key.details.oids.len() == 1 => {
                if let Some((old_id, new_id)) = known_oids {
                    let (old_m, new_m) = lookup_commit_modes(repo, &key.details, lookup_old, &key.path);
                    let mc = FileModeChange::from_modes(old_m, new_m);
                    return populate_file_diff(
                        out,
                        read_opt_oid(old_id, old_m)?,
                        read_opt_oid(new_id, new_m)?,
                        mc,
                        lfs_filter,
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
                    .map(|t| read_tree_path(repo, t, lookup_old))
                    .unwrap_or(CheckedBlob::Missing);
                let new = read_tree_path(repo, &new_tree, &key.path);
                let mc = FileModeChange::from_modes(old.mode(), new.mode());
                return populate_file_diff(out, old, new, mc, lfs_filter, context_lines, token);
            }
            DetailsMode::Commits if !key.details.oids.is_empty() => {
                if let Some((old_id, new_id)) = known_oids
                    && let Some((old_m, new_m)) = lookup_multi_commit_modes(
                        repo,
                        &key.details.oids,
                        lookup_old,
                        &key.path,
                        old_id,
                        new_id,
                    )
                {
                    let mc = FileModeChange::from_modes(old_m, new_m);
                    return populate_file_diff(
                        out,
                        read_opt_oid(old_id, old_m)?,
                        read_opt_oid(new_id, new_m)?,
                        mc,
                        lfs_filter,
                        context_lines,
                        token,
                    );
                }
            }
            _ => {}
        }

        let (mut resolved, commits) = resolve_changes(repo, &key.details, token)?;
        let Some(idx) = resolved
            .iter()
            .position(|c| c.path == key.path && c.old_path == key.old_path)
            .or_else(|| resolved.iter().position(|c| c.path == key.path))
        else {
            return Ok(out);
        };
        let item = resolved.swap_remove(idx);
        let old = if item.old_oid.is_zero()
            && key.details.mode == DetailsMode::Commits
            && commits.len() == 1
            && let Some(old_path) = key.old_path.as_deref().filter(|old| *old != key.path)
            && let Some(parent_tree) = commits[0]
                .parent(key.details.parent_index)
                .ok()
                .and_then(|p| p.tree().ok())
        {
            read_tree_path(repo, &parent_tree, old_path)
        } else {
            read_blob(repo, item.old_oid, item.old_mode)
        };
        let new = read_new_bytes_checked(repo, &key.details, &item);
        let eff_new_mode = if key.details.mode == DetailsMode::CompareWithLocal {
            effective_worktree_mode(repo, new.mode(), old.mode())
        } else {
            new.mode()
        };
        let mc = FileModeChange::from_modes(old.mode(), eff_new_mode);
        if item.is_binary {
            let (old_size, old_oid) = old.metadata();
            let (new_size, new_oid) = new.metadata();
            let mut out = out;
            out.is_binary = true;
            out.mode_change = mc;
            out.binary = Some(BinaryDiffInfo {
                old_size,
                new_size,
                old_oid,
                new_oid,
            });
            return Ok(out);
        }
        populate_file_diff(out, old, new, mc, lfs_filter, context_lines, token)
    })
}
