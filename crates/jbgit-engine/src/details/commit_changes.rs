use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use git2::{Delta, DiffFindOptions, DiffOptions, Repository};
use jbgit_core::{
    CommitChangesBundle, CommitDetails, CommitSignature, DetailsKey, DetailsMode, EpochToken,
    FileChange, FileStatusKind, Oid,
};

use crate::blob_reader::read_blob;
use crate::error::EngineError;
use crate::pool::{delta_to_kind, with_thread_repo};
use crate::probe::{RepoLocation, to_core_oid};

use super::file_diff::{imara_line_stats, read_new_bytes_checked};
use super::{delta_path, person, to_git2_oid};

/// Per-file line statistics are skipped beyond this many files (huge merges).
const MAX_STAT_FILES: usize = 2_000;

pub fn verify_commit_signature_sync(git_dir: &Path, oid: Oid) -> CommitSignature {
    let git = crate::cli::cached_git_executable().unwrap_or_else(|| Path::new("git"));
    let mut cmd = crate::cli::non_interactive_std_command(git, None);
    cmd.arg("--git-dir")
        .arg(git_dir)
        .args(["log", "-1", "--format=%G?%x1f%GS%x1f%GK", &oid.to_string()]);
    match cmd.output() {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            let line = text.lines().next().unwrap_or("");
            CommitSignature::parse_single_format_line(line)
        }
        _ => CommitSignature::cannot_check(),
    }
}

pub fn verify_commit_signatures_batch_sync(
    git_dir: &Path,
    oids: &[Oid],
    token: &EpochToken,
) -> Vec<(Oid, CommitSignature)> {
    if oids.is_empty() || token.is_stale() {
        return Vec::new();
    }
    let batch: &[Oid] = &oids[..oids.len().min(100)];
    let fallback = || -> Vec<(Oid, CommitSignature)> {
        if token.is_stale() {
            Vec::new()
        } else {
            batch
                .iter()
                .map(|&oid| (oid, CommitSignature::cannot_check()))
                .collect()
        }
    };
    let git = crate::cli::cached_git_executable().unwrap_or_else(|| Path::new("git"));
    let mut cmd = crate::cli::non_interactive_std_command(git, None);
    cmd.arg("--git-dir")
        .arg(git_dir)
        .args([
            "log",
            "--no-walk",
            "--stdin",
            "--format=%H%x1f%G?%x1f%GS%x1f%GK",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return fallback();
    };
    if let Some(mut stdin) = child.stdin.take() {
        for oid in batch {
            let _ = writeln!(stdin, "{oid}");
        }
        drop(stdin);
    }
    if token.is_stale() {
        let _ = child.kill();
        let _ = child.wait();
        return Vec::new();
    }
    let Ok(output) = child.wait_with_output() else {
        return fallback();
    };
    if !output.status.success() {
        return fallback();
    }
    if token.is_stale() {
        return Vec::new();
    }
    let mut parsed =
        CommitSignature::parse_batch_format_output(&String::from_utf8_lossy(&output.stdout));
    for &oid in batch {
        if !parsed.iter().any(|(seen, _)| *seen == oid) {
            parsed.push((oid, CommitSignature::cannot_check()));
        }
    }
    parsed
}

#[derive(Clone)]
struct BlobVersion {
    path: Option<String>,
    oid: git2::Oid,
    mode: Option<u32>,
}

#[derive(Clone)]
pub(super) struct ResolvedChange {
    pub(super) path: String,
    pub(super) old_path: Option<String>,
    pub(super) kind: FileStatusKind,
    pub(super) old_oid: git2::Oid,
    pub(super) new_oid: git2::Oid,
    pub(super) old_mode: Option<u32>,
    pub(super) new_mode: Option<u32>,
    pub(super) is_binary: bool,
}

fn file_mode_opt(file: &git2::DiffFile<'_>) -> Option<u32> {
    let raw: u32 = file.mode().into();
    (raw != 0).then_some(raw)
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
            old_mode: file_mode_opt(&delta.old_file()),
            new_mode: file_mode_opt(&delta.new_file()),
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
            let old_mode = file_mode_opt(&delta.old_file());
            let new_mode = file_mode_opt(&delta.new_file());
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
                        mode: new_mode,
                    };
                    prev.is_binary |= is_bin;
                    prev
                }
                None => NetEntry {
                    before: BlobVersion {
                        path: (!old_id.is_zero()).then_some(old_p).flatten(),
                        oid: old_id,
                        mode: old_mode,
                    },
                    after: BlobVersion {
                        path: (!new_id.is_zero()).then(|| key_path.clone()),
                        oid: new_id,
                        mode: new_mode,
                    },
                    is_binary: is_bin,
                },
            };
            entries.insert(key_path, entry);
        }
    }

    let mut out = Vec::new();
    for (key_path, entry) in entries {
        if entry.before.oid == entry.after.oid
            && entry.before.path == entry.after.path
            && entry.before.mode == entry.after.mode
        {
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
            old_mode: entry.before.mode,
            new_mode: entry.after.mode,
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
                    CommitSignature::pending()
                } else {
                    CommitSignature::unsigned()
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
                && let (Some((old_bytes, old_bin)), Some((new_bytes, new_bin))) = (
                    read_blob(repo, item.old_oid, None).into_bytes_or_too_large(),
                    read_new_bytes_checked(repo, key, &item).into_bytes_or_too_large(),
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
