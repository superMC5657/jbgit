use std::collections::HashMap;

use git2::{
    Delta, DiffFindOptions, DiffOptions, Repository, Status, StatusOptions, SubmoduleIgnore,
    SubmoduleStatus,
};
use jbgit_core::{EpochToken, FileStatusKind, StageEntry, StageStatusSnapshot, SubmoduleState};

use crate::error::EngineError;
use crate::probe::RepoLocation;

use super::scan_config::StatusScanConfig;
use super::worker_pool::with_thread_repo;

/// Detects copied files in `HEAD -> index` (`Delta::Copied`) because `libgit2`'s
/// `repo.statuses()` only detects renames and has no `INDEX_COPIED` status flag.
fn detect_staged_copies(repo: &Repository, include_unmodified: bool) -> HashMap<String, String> {
    let mut copies = HashMap::new();
    let Ok(head) = repo.head() else {
        return copies;
    };
    let Ok(commit) = head.peel_to_commit() else {
        return copies;
    };
    let Ok(head_tree) = commit.tree() else {
        return copies;
    };
    let mut diff_opts = DiffOptions::new();
    let mut find_opts = DiffFindOptions::new();
    find_opts.renames(true).copies(true);
    if include_unmodified {
        diff_opts.include_unmodified(true);
        find_opts.copies_from_unmodified(true);
    }
    let Ok(mut diff) = repo.diff_tree_to_index(Some(&head_tree), None, Some(&mut diff_opts)) else {
        return copies;
    };
    if diff.find_similar(Some(&mut find_opts)).is_err() {
        return copies;
    }
    for delta in diff.deltas() {
        if delta.status() == Delta::Copied
            && let (Some(new_p), Some(old_p)) = (delta.new_file().path(), delta.old_file().path())
        {
            let new_s = new_p.to_string_lossy().replace('\\', "/");
            let old_s = old_p.to_string_lossy().replace('\\', "/");
            if new_s != old_s {
                copies.insert(new_s, old_s);
            }
        }
    }
    copies
}

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
            let mut is_reversed: Option<bool> = None;
            for item in conflicts.flatten() {
                let p = item
                    .our
                    .as_ref()
                    .or(item.their.as_ref())
                    .or(item.ancestor.as_ref())
                    .map(|e| String::from_utf8_lossy(&e.path).replace('\\', "/"));
                if let Some(path) = p {
                    let reversed = *is_reversed.get_or_insert_with(|| {
                        jbgit_core::is_reverse_conflict_state(
                            &crate::probe::probe_repo_state(repo).state,
                        )
                    });
                    let (ours_present, theirs_present) = if reversed {
                        (item.their.is_some(), item.our.is_some())
                    } else {
                        (item.our.is_some(), item.their.is_some())
                    };
                    let label = match (item.ancestor.is_some(), ours_present, theirs_present) {
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
            let fallback_path = entry.path().unwrap_or_default().replace('\\', "/");

            if st.contains(Status::CONFLICTED) {
                snap.conflicted.push(StageEntry {
                    path: fallback_path,
                    old_path: None,
                    kind: FileStatusKind::Conflicted,
                });
                continue;
            }

            if st.contains(Status::IGNORED) {
                snap.ignored.push(StageEntry {
                    path: fallback_path,
                    old_path: None,
                    kind: FileStatusKind::Ignored,
                });
                continue;
            }

            if let Some(kind) = index_status_kind(st, entry.head_to_index().map(|d| d.status())) {
                // `StatusEntry::path()` returns `old_file.path` in libgit2; for renames/copies
                // resolve `new_file.path` and `old_file.path` independently from `head_to_index`.
                let (staged_path, old_path) = if let Some(d) = entry.head_to_index() {
                    let old = d
                        .old_file()
                        .path()
                        .map(|p| p.to_string_lossy().replace('\\', "/"));
                    let new = d
                        .new_file()
                        .path()
                        .map(|p| p.to_string_lossy().replace('\\', "/"));
                    let path = new
                        .or_else(|| old.clone())
                        .unwrap_or_else(|| fallback_path.clone());
                    let old_path = old.filter(|o| o != &path);
                    (path, old_path)
                } else {
                    (fallback_path.clone(), None)
                };
                snap.staged.push(StageEntry {
                    path: staged_path,
                    old_path,
                    kind,
                });
            }

            if st.contains(Status::WT_NEW) {
                let wt_path = entry
                    .index_to_workdir()
                    .and_then(|d| {
                        d.new_file()
                            .path()
                            .or_else(|| d.old_file().path())
                            .map(|p| p.to_string_lossy().replace('\\', "/"))
                    })
                    .unwrap_or(fallback_path);
                snap.untracked.push(StageEntry {
                    path: wt_path,
                    old_path: None,
                    kind: FileStatusKind::Untracked,
                });
            } else if let Some(kind) = worktree_status_kind(st).or_else(|| {
                entry
                    .index_to_workdir()
                    .and_then(|d| delta_to_kind(d.status()))
            }) {
                // Independently resolve worktree `new_file.path` vs index `old_file.path` so
                // unstaged renames and dual-stage renames (`HEAD -> Index -> Workdir`) keep
                // their true destination paths.
                let (wt_path, old_path) = if let Some(d) = entry.index_to_workdir() {
                    let old = d
                        .old_file()
                        .path()
                        .map(|p| p.to_string_lossy().replace('\\', "/"));
                    let new = d
                        .new_file()
                        .path()
                        .map(|p| p.to_string_lossy().replace('\\', "/"));
                    let path = new.or_else(|| old.clone()).unwrap_or(fallback_path);
                    let old_path = old.filter(|o| o != &path);
                    (path, old_path)
                } else {
                    (fallback_path, None)
                };
                snap.unstaged.push(StageEntry {
                    path: wt_path,
                    old_path,
                    kind,
                });
            }
        }

        // Upgrade newly-added staged entries that match an existing HEAD file to `Copied`.
        // Copies from unmodified sources are only searched on small indexes with few
        // added files (`O(added x index_entries)` similarity comparisons otherwise).
        let added = snap
            .staged
            .iter()
            .filter(|e| e.kind == FileStatusKind::Added)
            .count();
        if added > 0 {
            if token.is_stale() {
                return Err(EngineError::Cancelled);
            }
            let copies = detect_staged_copies(
                repo,
                scan_config.copies_from_unmodified(index_entries, added),
            );
            if !copies.is_empty() {
                for entry in &mut snap.staged {
                    if entry.kind == FileStatusKind::Added
                        && let Some(src) = copies.get(&entry.path)
                    {
                        entry.kind = FileStatusKind::Copied;
                        entry.old_path = Some(src.clone());
                    }
                }
            }
        }

        if let Ok(subs) = repo.submodules() {
            for sub in subs {
                let path = sub.path().to_string_lossy().replace('\\', "/");
                if path.is_empty() {
                    continue;
                }
                let status_key = sub.name().ok().unwrap_or(path.as_str());
                let st = repo
                    .submodule_status(status_key, SubmoduleIgnore::None)
                    .unwrap_or(SubmoduleStatus::empty());
                let state = SubmoduleState {
                    new_commits: st.contains(SubmoduleStatus::WD_MODIFIED),
                    dirty_workdir: st.intersects(
                        SubmoduleStatus::WD_INDEX_MODIFIED
                            | SubmoduleStatus::WD_WD_MODIFIED
                            | SubmoduleStatus::WD_UNTRACKED,
                    ),
                    uninitialized: st.contains(SubmoduleStatus::WD_UNINITIALIZED),
                };
                snap.submodules.insert(path.clone(), state);
                if (state.new_commits || state.dirty_workdir)
                    && !snap.conflicted.iter().any(|e| e.path == path)
                    && !snap.unstaged.iter().any(|e| e.path == path)
                {
                    snap.unstaged.push(StageEntry {
                        path,
                        old_path: None,
                        kind: FileStatusKind::Modified,
                    });
                }
            }
        }

        Ok(snap)
    })
}

fn index_status_kind(st: Status, delta: Option<Delta>) -> Option<FileStatusKind> {
    if delta == Some(Delta::Copied) {
        return Some(FileStatusKind::Copied);
    }
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use git2::{IndexEntry, IndexTime, Repository};
    use jbgit_core::RequestEpoch;

    use super::*;
    use crate::pool::{COPY_SCAN_MAX_ADDED, COPY_SCAN_MAX_INDEX_ENTRIES};

    fn body(i: usize) -> String {
        (0..10).map(|l| format!("file {i} payload {l}\n")).collect()
    }

    /// Commits `count` files; when `materialize` is unset they only exist as
    /// index/`HEAD` entries sharing one blob (fast large fixtures).
    fn repo_with_files(count: usize, materialize: bool) -> (tempfile::TempDir, RepoLocation) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let repo = Repository::init(root).unwrap();
        repo.config()
            .unwrap()
            .set_bool("core.autocrlf", false)
            .unwrap();
        let mut index = repo.index().unwrap();
        for i in 0..count {
            let name = format!("f{i:05}.txt");
            if materialize {
                std::fs::write(root.join(&name), body(i)).unwrap();
                index.add_path(Path::new(&name)).unwrap();
            } else {
                let entry = IndexEntry {
                    ctime: IndexTime::new(0, 0),
                    mtime: IndexTime::new(0, 0),
                    dev: 0,
                    ino: 0,
                    mode: 0o100_644,
                    uid: 0,
                    gid: 0,
                    file_size: 0,
                    id: git2::Oid::ZERO_SHA1,
                    flags: 0,
                    flags_extended: 0,
                    path: name.into_bytes(),
                };
                index.add_frombuffer(&entry, b"shared body\n").unwrap();
            }
        }
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        let loc = RepoLocation::discover(root).unwrap();
        (tmp, loc)
    }

    fn stage_new_file(loc: &RepoLocation, name: &str, content: &[u8]) {
        std::fs::write(loc.workdir.join(name), content).unwrap();
        let repo = Repository::open(&loc.git_dir).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(name)).unwrap();
        index.write().unwrap();
    }

    fn staged_kind(snap: &StageStatusSnapshot, path: &str) -> FileStatusKind {
        snap.staged
            .iter()
            .find(|e| e.path == path)
            .unwrap_or_else(|| panic!("{path} staged"))
            .kind
    }

    fn status(loc: &RepoLocation) -> StageStatusSnapshot {
        let token = RequestEpoch::new().issue();
        load_stage_status_with_config_sync(loc, false, StatusScanConfig::default(), &token).unwrap()
    }

    #[test]
    fn bulk_added_files_skip_copies_from_unmodified_scan() {
        let (_tmp, loc) = repo_with_files(6, true);
        stage_new_file(&loc, "copy.txt", body(3).as_bytes());
        for i in 0..COPY_SCAN_MAX_ADDED {
            stage_new_file(
                &loc,
                &format!("bulk{i:03}.txt"),
                format!("bulk {i}\n").as_bytes(),
            );
        }
        let snap = status(&loc);
        assert_eq!(staged_kind(&snap, "copy.txt"), FileStatusKind::Added);
    }

    #[test]
    fn added_file_on_index_above_copy_scan_threshold_does_not_scan_unmodified_sources() {
        let (_tmp, loc) = repo_with_files(COPY_SCAN_MAX_INDEX_ENTRIES + 500, false);
        // Identical to every (unmodified) `HEAD` blob: only a `copies_from_unmodified`
        // scan could classify it as `Copied`.
        stage_new_file(&loc, "copy.txt", b"shared body\n");
        let started = std::time::Instant::now();
        let snap = status(&loc);
        eprintln!(
            "stage status with {} index entries: {:?}",
            COPY_SCAN_MAX_INDEX_ENTRIES + 501,
            started.elapsed()
        );
        assert_eq!(staged_kind(&snap, "copy.txt"), FileStatusKind::Added);
    }
}
