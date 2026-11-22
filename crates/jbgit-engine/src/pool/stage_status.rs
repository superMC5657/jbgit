use git2::{Delta, Status, StatusOptions};
use jbgit_core::{EpochToken, FileStatusKind, StageEntry, StageStatusSnapshot};

use crate::error::EngineError;
use crate::probe::RepoLocation;

use super::scan_config::StatusScanConfig;
use super::worker_pool::with_thread_repo;

pub fn load_stage_status_with_config_sync(
    location: &RepoLocation,
    include_ignored: bool,
    scan_config: StatusScanConfig,
    token: &EpochToken,
) -> Result<StageStatusSnapshot, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }

    if location.is_bare {
        return Ok(StageStatusSnapshot::default());
    }

    with_thread_repo(&location.git_dir, |repo| {
        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }

        let index_entries = repo.index().map(|idx| idx.len()).unwrap_or(0);
        let policy = scan_config.policy(index_entries);
        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(policy.recurse_untracked_dirs)
            .renames_head_to_index(true)
            .renames_index_to_workdir(policy.worktree_renames)
            .include_ignored(include_ignored);

        let statuses = repo.statuses(Some(&mut opts))?;

        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }

        let mut snap = StageStatusSnapshot::default();
        if let Ok(index) = repo.index()
            && let Ok(conflicts) = index.conflicts()
        {
            for item in conflicts.flatten() {
                let p = item
                    .our
                    .as_ref()
                    .or(item.their.as_ref())
                    .or(item.ancestor.as_ref())
                    .map(|e| String::from_utf8_lossy(&e.path).replace('\\', "/"));
                if let Some(path) = p {
                    let label = match (
                        item.ancestor.is_some(),
                        item.our.is_some(),
                        item.their.is_some(),
                    ) {
                        (true, true, true) => "Both Modified",
                        (false, true, true) => "Added by Both",
                        (true, true, false) => "Deleted by Them",
                        (true, false, true) => "Deleted by Us",
                        (true, false, false) => "Both Deleted",
                        (false, true, false) => "Added by Us",
                        (false, false, true) => "Added by Them",
                        (false, false, false) => "Conflicted",
                    };
                    snap.conflict_labels.insert(path, label.to_string());
                }
            }
        }
        for entry in statuses.iter() {
            let st = entry.status();
            let path = entry.path().unwrap_or_default().to_string();

            if st.contains(Status::CONFLICTED) {
                snap.conflicted.push(StageEntry {
                    path,
                    old_path: None,
                    kind: FileStatusKind::Conflicted,
                });
                continue;
            }

            if st.contains(Status::IGNORED) {
                snap.ignored.push(StageEntry {
                    path,
                    old_path: None,
                    kind: FileStatusKind::Ignored,
                });
                continue;
            }

            if let Some(kind) = index_status_kind(st) {
                let old_path = entry
                    .head_to_index()
                    .and_then(|d| d.old_file().path().map(|p| p.to_string_lossy().to_string()))
                    .filter(|old| old != &path);
                snap.staged.push(StageEntry {
                    path: path.clone(),
                    old_path,
                    kind,
                });
            }

            if st.contains(Status::WT_NEW) {
                snap.untracked.push(StageEntry {
                    path,
                    old_path: None,
                    kind: FileStatusKind::Untracked,
                });
            } else if let Some(kind) = worktree_status_kind(st) {
                let old_path = entry
                    .index_to_workdir()
                    .and_then(|d| d.old_file().path().map(|p| p.to_string_lossy().to_string()))
                    .filter(|old| old != &path);
                snap.unstaged.push(StageEntry {
                    path,
                    old_path,
                    kind,
                });
            } else if let Some(diff) = entry.index_to_workdir()
                && let Some(kind) = delta_to_kind(diff.status())
            {
                snap.unstaged.push(StageEntry {
                    path,
                    old_path: None,
                    kind,
                });
            }
        }

        Ok(snap)
    })
}

fn index_status_kind(st: Status) -> Option<FileStatusKind> {
    if st.contains(Status::INDEX_NEW) {
        Some(FileStatusKind::Added)
    } else if st.contains(Status::INDEX_MODIFIED) {
        Some(FileStatusKind::Modified)
    } else if st.contains(Status::INDEX_DELETED) {
        Some(FileStatusKind::Deleted)
    } else if st.contains(Status::INDEX_RENAMED) {
        Some(FileStatusKind::Renamed)
    } else if st.contains(Status::INDEX_TYPECHANGE) {
        Some(FileStatusKind::Typechange)
    } else {
        None
    }
}

fn worktree_status_kind(st: Status) -> Option<FileStatusKind> {
    if st.contains(Status::WT_MODIFIED) {
        Some(FileStatusKind::Modified)
    } else if st.contains(Status::WT_DELETED) {
        Some(FileStatusKind::Deleted)
    } else if st.contains(Status::WT_RENAMED) {
        Some(FileStatusKind::Renamed)
    } else if st.contains(Status::WT_TYPECHANGE) {
        Some(FileStatusKind::Typechange)
    } else {
        None
    }
}

pub(crate) fn delta_to_kind(delta: Delta) -> Option<FileStatusKind> {
    match delta {
        Delta::Added => Some(FileStatusKind::Added),
        Delta::Deleted => Some(FileStatusKind::Deleted),
        Delta::Modified => Some(FileStatusKind::Modified),
        Delta::Renamed => Some(FileStatusKind::Renamed),
        Delta::Copied => Some(FileStatusKind::Copied),
        Delta::Typechange => Some(FileStatusKind::Typechange),
        Delta::Untracked => Some(FileStatusKind::Untracked),
        Delta::Ignored => Some(FileStatusKind::Ignored),
        Delta::Conflicted => Some(FileStatusKind::Conflicted),
        _ => None,
    }
}
