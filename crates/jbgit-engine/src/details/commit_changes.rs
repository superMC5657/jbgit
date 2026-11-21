use std::collections::HashMap;
use std::path::Path;

use git2::{Delta, DiffFindOptions, DiffOptions, Repository};
use jbgit_core::{
    CommitChangesBundle, CommitDetails, DetailsKey, DetailsMode, EpochToken, FileChange,
    FileStatusKind, Oid, SignatureStatus,
};

use crate::error::EngineError;
use crate::pool::{delta_to_kind, with_thread_repo};
use crate::probe::{RepoLocation, to_core_oid};

use super::file_diff::{
    ReadBytes, imara_line_stats, read_blob_checked, read_new_bytes_checked,
};
use super::{delta_path, non_interactive_git_command, person, to_git2_oid};

/// Per-file line statistics are skipped beyond this many files (huge merges).
const MAX_STAT_FILES: usize = 2_000;

pub fn verify_commit_signature_sync(git_dir: &Path, oid: Oid) -> SignatureStatus {
    let git = crate::cli::cached_git_executable().unwrap_or_else(|| Path::new("git"));
    let mut cmd = non_interactive_git_command(git, None);
    cmd.arg("--git-dir")
        .arg(git_dir)
        .args(["log", "-1", "--format=%G?", &oid.to_string()]);
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
pub(super) struct ResolvedChange {
    pub(super) path: String,
    pub(super) old_path: Option<String>,
    pub(super) kind: FileStatusKind,
    pub(super) old_oid: git2::Oid,
    pub(super) new_oid: git2::Oid,
    pub(super) is_binary: bool,
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

pub(super) fn resolve_changes<'r>(
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
            if idx < MAX_STAT_FILES
                && !is_binary
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
