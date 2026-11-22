use git2::Repository;
use imara_diff::{Algorithm, Diff, InternedInput};
use jbgit_core::{
    DetailsKey, DetailsMode, EpochToken, FileDiff, FileDiffKey, FileStatusKind, MAX_DIFF_LINES,
    Oid, build_diff_hunks,
};

use crate::blob_reader::{CheckedBlob, read_blob, read_tree_path, read_worktree_path};
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
    read_blob(repo, item.new_oid, None)
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

fn populate_file_diff(
    mut out: FileDiff,
    old: CheckedBlob,
    new: CheckedBlob,
    context_lines: u32,
    token: &EpochToken,
) -> Result<FileDiff, EngineError> {
    let (Some((old_bytes, old_bin)), Some((new_bytes, new_bin))) = (
        old.into_bytes_or_too_large(),
        new.into_bytes_or_too_large(),
    ) else {
        out.is_too_large = true;
        return Ok(out);
    };
    if old_bin || new_bin {
        out.is_binary = true;
        return Ok(out);
    }
    let (hunks, is_truncated) = build_diff_hunks(
        &old_bytes,
        &new_bytes,
        context_lines as usize,
        MAX_DIFF_LINES,
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
        let mut out = FileDiff {
            key: key.clone(),
            hunks: Vec::new(),
            is_binary: false,
            is_truncated: false,
            is_too_large: false,
        };
        let context_lines = key.context.lines();
        let lookup_old = key.old_path.as_deref().unwrap_or(&key.path);

        let read_opt_oid = |opt: Option<Oid>| -> Result<CheckedBlob, EngineError> {
            match opt {
                Some(id) => Ok(read_blob(repo, to_git2_oid(&id)?, None)),
                None => Ok(CheckedBlob::empty()),
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
                let old = read_tree_path(repo, &base_tree, lookup_old);
                let new = read_tree_path(repo, &target_tree, &key.path);
                return populate_file_diff(out, old, new, context_lines, token);
            }
            DetailsMode::CompareWithLocal if !key.details.oids.is_empty() => {
                let old = match known_oids {
                    Some((old_id, _)) => read_opt_oid(old_id)?,
                    None => {
                        let base = repo.find_commit(to_git2_oid(&key.details.oids[0])?)?;
                        let base_tree = base.tree()?;
                        read_tree_path(repo, &base_tree, lookup_old)
                    }
                };
                let new = repo
                    .workdir()
                    .map_or(CheckedBlob::Missing, |w| read_worktree_path(w, &key.path));
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
                    .map(|t| read_tree_path(repo, t, lookup_old))
                    .unwrap_or_else(CheckedBlob::empty);
                let new = read_tree_path(repo, &new_tree, &key.path);
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
            read_tree_path(repo, &parent_tree, old_path)
        } else {
            read_blob(repo, item.old_oid, None)
        };
        let new = read_new_bytes_checked(repo, &key.details, &item);
        populate_file_diff(out, old, new, context_lines, token)
    })
}
