use std::fs;
use std::path::Path;

use git2::build::CheckoutBuilder;
use git2::{
    Commit, DiffFindOptions, IndexEntry, IndexTime, ObjectType, Repository, ResetType, Signature,
    SubmoduleIgnore, SubmoduleStatus,
};
use jbgit_core::{
    AmendCommitPreview, DetailsKey, DiffContext, DiffHunk, DiffLine, DiffLineKind, EpochToken,
    FileChange, FileDiff, FileDiffKey, FileStatusKind, LfsDiff, MAX_DIFF_LINES, Oid,
    StageDiffHunk, StageLineTracker, StageNodeKind, StageTarget, SubmoduleState, parse_lfs_pointer,
};

use crate::blob_reader::{
    CheckedBlob, path_has_lfs_filter, read_head_tree_path, read_index_entry, read_tree_path,
    read_worktree_path,
};
use crate::error::EngineError;
use crate::pool::{delta_to_kind, with_thread_repo};
use crate::probe::{RepoLocation, to_core_oid};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StageDiffSide {
    Staged,
    Unstaged,
    AmendCommit,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StageDiffKey {
    pub path: String,
    pub old_path: Option<String>,
    pub side: StageDiffSide,
    pub context: DiffContext,
}

/// Read-only submodule pointer metadata shown instead of a text diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmoduleDiffInfo {
    pub head_oid: Option<Oid>,
    pub index_oid: Option<Oid>,
    pub workdir_oid: Option<Oid>,
    pub state: SubmoduleState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreeDocStageDiff {
    pub key: StageDiffKey,
    pub vcs_exists: bool,
    pub staged_exists: bool,
    pub local_exists: bool,
    pub vcs_bytes: Vec<u8>,
    pub staged_bytes: Vec<u8>,
    pub local_bytes: Vec<u8>,
    pub file_mode: u32,
    pub is_binary: bool,
    pub is_conflicted: bool,
    pub submodule: Option<SubmoduleDiffInfo>,
    pub lfs: Option<LfsDiff>,
    pub tracker: StageLineTracker,
    pub hunks: Vec<StageDiffHunk>,
    pub is_truncated: bool,
    pub is_too_large: bool,
}

impl ThreeDocStageDiff {
    pub fn as_file_diff(&self) -> FileDiff {
        FileDiff {
            key: FileDiffKey {
                details: DetailsKey::single(Oid::ZERO),
                path: self.key.path.clone(),
                old_path: self.key.old_path.clone(),
                context: self.key.context,
            },
            hunks: self.hunks.iter().map(|h| h.hunk.clone()).collect(),
            is_binary: self.is_binary,
            is_truncated: self.is_truncated,
            is_too_large: self.is_too_large,
            lfs: self.lfs.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitPrecheckInfo {
    pub has_commit_hooks: bool,
    pub gpg_sign: bool,
    pub user_name: Option<String>,
    pub user_email: Option<String>,
    pub has_remote: bool,
}

#[derive(Debug, Clone)]
pub struct CommitRequest {
    pub message: String,
    pub amend: bool,
    pub signoff: bool,
    pub run_hooks: bool,
    pub author_override: Option<String>,
    pub stage_all_tracked: bool,
    pub and_push: bool,
}

#[derive(Debug, Clone)]
pub struct CommitOutcome {
    pub success: bool,
    pub commit_oid: Option<Oid>,
    pub short_sha: Option<String>,
    pub summary: String,
    pub error_output: Option<String>,
    pub and_push: bool,
    pub preserved_message: Option<String>,
}

fn detect_submodule_diff(repo: &Repository, path: &str) -> Option<SubmoduleDiffInfo> {
    let norm = path.replace('\\', "/");
    let subs = repo.submodules().ok()?;
    for sub in subs {
        let sub_path = sub.path().to_string_lossy().replace('\\', "/");
        if sub_path == norm {
            let status_key = sub.name().ok().unwrap_or(sub_path.as_str());
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
            return Some(SubmoduleDiffInfo {
                head_oid: sub.head_id().map(to_core_oid),
                index_oid: sub.index_id().map(to_core_oid),
                workdir_oid: sub.workdir_id().map(to_core_oid),
                state,
            });
        }
    }
    None
}

fn empty_submodule_diff(key: StageDiffKey, info: SubmoduleDiffInfo) -> ThreeDocStageDiff {
    ThreeDocStageDiff {
        key,
        vcs_exists: info.head_oid.is_some(),
        staged_exists: info.index_oid.is_some(),
        local_exists: info.workdir_oid.is_some(),
        vcs_bytes: Vec::new(),
        staged_bytes: Vec::new(),
        local_bytes: Vec::new(),
        file_mode: 0o160_000,
        is_binary: false,
        is_conflicted: false,
        submodule: Some(info),
        lfs: None,
        tracker: StageLineTracker::new(b"", b"", b""),
        hunks: Vec::new(),
        is_truncated: false,
        is_too_large: false,
    }
}

pub(crate) fn load_stage_file_diff_sync(
    location: &RepoLocation,
    key: &StageDiffKey,
    token: &EpochToken,
) -> Result<ThreeDocStageDiff, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }

    with_thread_repo(&location.git_dir, |repo| {
        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }

        if let Some(info) = detect_submodule_diff(repo, &key.path) {
            return Ok(empty_submodule_diff(key.clone(), info));
        }

        let lfs_filter = path_has_lfs_filter(repo, &key.path);

        if key.side == StageDiffSide::AmendCommit {
            let vcs_lookup = key.old_path.as_deref().unwrap_or(&key.path);
            let (vcs_side, head_side) = match repo.head().ok().and_then(|h| h.peel_to_commit().ok())
            {
                Some(commit) => {
                    let parent_res = commit
                        .parent(0)
                        .ok()
                        .and_then(|p| p.tree().ok())
                        .map(|t| read_tree_path(repo, &t, vcs_lookup))
                        .unwrap_or(CheckedBlob::Missing);
                    let head_res = commit
                        .tree()
                        .ok()
                        .map(|t| read_tree_path(repo, &t, &key.path))
                        .unwrap_or(CheckedBlob::Missing);
                    (parent_res, head_res)
                }
                None => (CheckedBlob::Missing, CheckedBlob::Missing),
            };

            if token.is_stale() {
                return Err(EngineError::Cancelled);
            }

            let file_mode = head_side.mode().or(vcs_side.mode()).unwrap_or(0o100644);
            if file_mode == 0o160_000 {
                return Ok(empty_submodule_diff(
                    key.clone(),
                    SubmoduleDiffInfo {
                        head_oid: None,
                        index_oid: None,
                        workdir_oid: None,
                        state: SubmoduleState::default(),
                    },
                ));
            }
            let vcs_exists = vcs_side.exists();
            let head_exists = head_side.exists();
            if vcs_side.is_too_large() || head_side.is_too_large() {
                return Ok(ThreeDocStageDiff {
                    key: key.clone(),
                    vcs_exists,
                    staged_exists: head_exists,
                    local_exists: head_exists,
                    vcs_bytes: Vec::new(),
                    staged_bytes: Vec::new(),
                    local_bytes: Vec::new(),
                    file_mode,
                    is_binary: false,
                    is_conflicted: false,
                    submodule: None,
                    lfs: None,
                    tracker: StageLineTracker::new(b"", b"", b""),
                    hunks: Vec::new(),
                    is_truncated: false,
                    is_too_large: true,
                });
            }

            let (vcs_opt, _, vcs_bin) = vcs_side.into_parts();
            let (head_opt, _, head_bin) = head_side.into_parts();
            let is_binary = vcs_bin || head_bin;
            let vcs_bytes = vcs_opt.unwrap_or_default();
            let head_bytes = head_opt.unwrap_or_default();

            let old_ptr = parse_lfs_pointer(&vcs_bytes);
            let new_ptr = parse_lfs_pointer(&head_bytes);
            if old_ptr.is_some() || new_ptr.is_some() {
                return Ok(ThreeDocStageDiff {
                    key: key.clone(),
                    vcs_exists,
                    staged_exists: head_exists,
                    local_exists: head_exists,
                    vcs_bytes,
                    staged_bytes: head_bytes.clone(),
                    local_bytes: head_bytes,
                    file_mode,
                    is_binary: false,
                    is_conflicted: false,
                    submodule: None,
                    lfs: Some(LfsDiff {
                        old: old_ptr,
                        new: new_ptr,
                        filter_detected: lfs_filter,
                    }),
                    tracker: StageLineTracker::new(b"", b"", b""),
                    hunks: Vec::new(),
                    is_truncated: false,
                    is_too_large: false,
                });
            }

            if is_binary {
                return Ok(ThreeDocStageDiff {
                    key: key.clone(),
                    vcs_exists,
                    staged_exists: head_exists,
                    local_exists: head_exists,
                    vcs_bytes,
                    staged_bytes: head_bytes.clone(),
                    local_bytes: head_bytes,
                    file_mode,
                    is_binary: true,
                    is_conflicted: false,
                    submodule: None,
                    lfs: None,
                    tracker: StageLineTracker::new(b"", b"", b""),
                    hunks: Vec::new(),
                    is_truncated: false,
                    is_too_large: false,
                });
            }

            let tracker = StageLineTracker::new(&vcs_bytes, &head_bytes, &head_bytes);
            if token.is_stale() {
                return Err(EngineError::Cancelled);
            }
            let (hunks, is_truncated) =
                tracker.build_interactive_hunks(true, key.context, MAX_DIFF_LINES);
            if token.is_stale() {
                return Err(EngineError::Cancelled);
            }

            return Ok(ThreeDocStageDiff {
                key: key.clone(),
                vcs_exists,
                staged_exists: head_exists,
                local_exists: head_exists,
                vcs_bytes,
                staged_bytes: head_bytes.clone(),
                local_bytes: head_bytes,
                file_mode,
                is_binary: false,
                is_conflicted: false,
                submodule: None,
                lfs: None,
                tracker,
                hunks,
                is_truncated,
                is_too_large: false,
            });
        }

        let vcs_lookup = if key.side == StageDiffSide::Staged {
            key.old_path.as_deref().unwrap_or(&key.path)
        } else {
            &key.path
        };

        let vcs_side = read_head_tree_path(repo, vcs_lookup);
        let (staged_side, is_conflicted) = read_index_entry(repo, &key.path);
        let local_side = read_worktree_path(&location.workdir, &key.path);

        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }

        let file_mode = staged_side
            .mode()
            .or(vcs_side.mode())
            .or(local_side.mode())
            .unwrap_or(0o100644);
        if file_mode == 0o160_000 {
            return Ok(empty_submodule_diff(
                key.clone(),
                SubmoduleDiffInfo {
                    head_oid: None,
                    index_oid: None,
                    workdir_oid: None,
                    state: SubmoduleState::default(),
                },
            ));
        }
        let vcs_exists = vcs_side.exists();
        let staged_exists = staged_side.exists();
        let local_exists = local_side.exists();

        if vcs_side.is_too_large() || staged_side.is_too_large() || local_side.is_too_large() {
            return Ok(ThreeDocStageDiff {
                key: key.clone(),
                vcs_exists,
                staged_exists,
                local_exists,
                vcs_bytes: Vec::new(),
                staged_bytes: Vec::new(),
                local_bytes: Vec::new(),
                file_mode,
                is_binary: false,
                is_conflicted,
                submodule: None,
                lfs: None,
                tracker: StageLineTracker::new(b"", b"", b""),
                hunks: Vec::new(),
                is_truncated: false,
                is_too_large: true,
            });
        }

        let (vcs_opt, _, vcs_bin) = vcs_side.into_parts();
        let (staged_opt, _, staged_bin) = staged_side.into_parts();
        let (local_opt, _, local_bin) = local_side.into_parts();
        let is_binary = vcs_bin || staged_bin || local_bin;

        let vcs_bytes = vcs_opt.unwrap_or_default();
        let staged_bytes = staged_opt.unwrap_or_default();
        let local_bytes = local_opt.unwrap_or_default();

        let (old_ptr, new_ptr) = if key.side == StageDiffSide::Staged {
            (parse_lfs_pointer(&vcs_bytes), parse_lfs_pointer(&staged_bytes))
        } else {
            (parse_lfs_pointer(&staged_bytes), parse_lfs_pointer(&local_bytes))
        };
        if old_ptr.is_some() || new_ptr.is_some() {
            return Ok(ThreeDocStageDiff {
                key: key.clone(),
                vcs_exists,
                staged_exists,
                local_exists,
                vcs_bytes,
                staged_bytes,
                local_bytes,
                file_mode,
                is_binary: false,
                is_conflicted,
                submodule: None,
                lfs: Some(LfsDiff {
                    old: old_ptr,
                    new: new_ptr,
                    filter_detected: lfs_filter,
                }),
                tracker: StageLineTracker::new(b"", b"", b""),
                hunks: Vec::new(),
                is_truncated: false,
                is_too_large: false,
            });
        }

        if is_binary {
            return Ok(ThreeDocStageDiff {
                key: key.clone(),
                vcs_exists,
                staged_exists,
                local_exists,
                vcs_bytes,
                staged_bytes,
                local_bytes,
                file_mode,
                is_binary: true,
                is_conflicted,
                submodule: None,
                lfs: None,
                tracker: StageLineTracker::new(b"", b"", b""),
                hunks: Vec::new(),
                is_truncated: false,
                is_too_large: false,
            });
        }

        let tracker = StageLineTracker::new(&vcs_bytes, &staged_bytes, &local_bytes);
        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }
        let (mut hunks, is_truncated) = tracker.build_interactive_hunks(
            key.side == StageDiffSide::Staged,
            key.context,
            MAX_DIFF_LINES,
        );
        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }

        // If conflicted and index stage 0 is absent, show the worktree conflict markers directly
        if is_conflicted && hunks.is_empty() && !local_bytes.is_empty() {
            let mut lines = Vec::new();
            for (idx, t) in String::from_utf8_lossy(&local_bytes)
                .lines()
                .enumerate()
                .take(MAX_DIFF_LINES)
            {
                if idx % 64 == 0 && token.is_stale() {
                    return Err(EngineError::Cancelled);
                }
                lines.push(DiffLine {
                    kind: if t.starts_with("<<<<<<<")
                        || t.starts_with("=======")
                        || t.starts_with(">>>>>>>")
                    {
                        DiffLineKind::Deleted
                    } else {
                        DiffLineKind::Context
                    },
                    old_lineno: Some(idx as u32 + 1),
                    new_lineno: Some(idx as u32 + 1),
                    text: t.trim_end_matches('\r').to_string(),
                    word_ranges: Vec::new(),
                });
            }
            let line_count = lines.len();
            hunks.push(StageDiffHunk {
                hunk: DiffHunk {
                    header: format!("@@ -1,{line_count} +1,{line_count} @@ (Merge Conflict)"),
                    lines,
                },
                block_indices: Vec::new(),
                line_meta: Vec::new(),
                has_mixed_range: false,
            });
        }

        Ok(ThreeDocStageDiff {
            key: key.clone(),
            vcs_exists,
            staged_exists,
            local_exists,
            vcs_bytes,
            staged_bytes,
            local_bytes,
            file_mode,
            is_binary: false,
            is_conflicted,
            submodule: None,
            lfs: None,
            tracker,
            hunks,
            is_truncated,
            is_too_large: false,
        })
    })
}

/// Writes `new_staged_bytes` directly into `.git/objects` and `.git/index` via
/// `git2::Index::add_frombuffer`, never touching the working tree.
pub(crate) fn apply_stage_buffer_sync(
    location: &RepoLocation,
    path: &str,
    new_staged_bytes: &[u8],
    file_mode: u32,
    remove_from_index: bool,
) -> Result<(), EngineError> {
    with_thread_repo(&location.git_dir, |repo| {
        let mut index = repo.index()?;
        if remove_from_index {
            let _ = index.remove_path(Path::new(path));
            index.write()?;
            return Ok(());
        }

        let mut entry = match index.get_path(Path::new(path), 0) {
            Some(existing) => existing,
            None => IndexEntry {
                ctime: IndexTime::new(0, 0),
                mtime: IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: if file_mode == 0o100755 {
                    0o100755
                } else {
                    0o100644
                },
                uid: 0,
                gid: 0,
                file_size: new_staged_bytes.len() as u32,
                id: git2::Oid::ZERO_SHA1,
                flags: 0,
                flags_extended: 0,
                path: path.as_bytes().to_vec(),
            },
        };
        if file_mode == 0o100755 || entry.mode == 0o100755 {
            entry.mode = 0o100755;
        } else if entry.mode == 0 {
            entry.mode = 0o100644;
        }
        entry.file_size = new_staged_bytes.len() as u32;
        index.add_frombuffer(&entry, new_staged_bytes)?;
        index.write()?;
        Ok(())
    })
}

/// Atomically writes `new_local_bytes` back to the worktree file (used for partial rollback & undo).
pub(crate) fn write_worktree_buffer_sync(
    location: &RepoLocation,
    path: &str,
    new_local_bytes: &[u8],
) -> Result<(), EngineError> {
    let full = location.workdir.join(path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&full, new_local_bytes)?;
    Ok(())
}

/// Removes `path` from the working tree (ignoring missing files) and, when
/// `stage` is true, clears both conflict stages (`:1`/`:2`/`:3`) and stage 0
/// from the index so a Delete/Modify or Both-Deleted conflict is resolved.
pub(crate) fn remove_conflict_path_sync(
    location: &RepoLocation,
    path: &str,
    stage: bool,
) -> Result<(), EngineError> {
    let full = location.workdir.join(path);
    match fs::remove_file(&full) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    if stage {
        with_thread_repo(&location.git_dir, |repo| {
            let mut index = repo.index()?;
            let rel = Path::new(path);
            let _ = index.conflict_remove(rel);
            let _ = index.remove_path(rel);
            index.write()?;
            Ok(())
        })?;
    }
    Ok(())
}

pub(crate) fn stage_targets_sync(
    location: &RepoLocation,
    targets: &[StageTarget],
) -> Result<(), EngineError> {
    with_thread_repo(&location.git_dir, |repo| {
        let mut index = repo.index()?;
        for t in targets {
            let rel = Path::new(&t.path);
            let full = location.workdir.join(rel);
            if full.exists() {
                index.add_path(rel)?;
            } else {
                let _ = index.conflict_remove(rel);
                let _ = index.remove_path(rel);
            }
            if let Some(old) = &t.old_path {
                let old_full = location.workdir.join(old);
                if !old_full.exists() {
                    let _ = index.conflict_remove(Path::new(old));
                    let _ = index.remove_path(Path::new(old));
                }
            }
        }
        index.write()?;
        Ok(())
    })
}

pub(crate) fn stage_all_tracked_sync(location: &RepoLocation) -> Result<(), EngineError> {
    with_thread_repo(&location.git_dir, |repo| {
        let mut index = repo.index()?;
        index.update_all(["*"], None)?;
        index.write()?;
        Ok(())
    })
}

pub(crate) fn unstage_targets_sync(
    location: &RepoLocation,
    targets: &[StageTarget],
) -> Result<(), EngineError> {
    with_thread_repo(&location.git_dir, |repo| {
        let mut pathspecs: Vec<String> = Vec::new();
        for t in targets {
            pathspecs.push(t.path.clone());
            if let Some(old) = &t.old_path {
                pathspecs.push(old.clone());
            }
        }
        if pathspecs.is_empty() {
            return Ok(());
        }

        if let Ok(head) = repo.head()
            && let Ok(obj) = head.peel(ObjectType::Commit)
        {
            repo.reset_default(Some(&obj), pathspecs.iter().map(String::as_str))?;
        } else {
            // Unborn HEAD: remove entries directly from Index
            let mut index = repo.index()?;
            for p in &pathspecs {
                let _ = index.remove_path(Path::new(p));
            }
            index.write()?;
        }
        Ok(())
    })
}

pub(crate) fn discard_targets_sync(
    location: &RepoLocation,
    targets: &[StageTarget],
) -> Result<(), EngineError> {
    with_thread_repo(&location.git_dir, |repo| {
        let mut tracked_paths = Vec::new();
        for t in targets {
            if t.group_kind == StageNodeKind::Untracked || t.status_kind == FileStatusKind::Untracked
            {
                let full = location.workdir.join(&t.path);
                if full.is_file() {
                    let _ = fs::remove_file(full);
                }
            } else {
                tracked_paths.push(t.path.clone());
            }
        }
        if !tracked_paths.is_empty() {
            let mut index = repo.index()?;
            let mut opts = CheckoutBuilder::new();
            opts.force();
            for p in &tracked_paths {
                opts.path(p);
            }
            repo.checkout_index(Some(&mut index), Some(&mut opts))?;
        }
        Ok(())
    })
}

pub(crate) fn load_amend_preview_sync(
    location: &RepoLocation,
) -> Result<Option<AmendCommitPreview>, EngineError> {
    with_thread_repo(&location.git_dir, |repo| {
        let Ok(head) = repo.head() else {
            return Ok(None);
        };
        let Ok(commit) = head.peel_to_commit() else {
            return Ok(None);
        };
        let oid = to_core_oid(commit.id());
        let short_sha = oid.short();
        let message = String::from_utf8_lossy(commit.message_bytes())
            .trim_end_matches('\n')
            .to_string();

        let base = commit.parent(0).ok().and_then(|p| p.tree().ok());
        let target = commit.tree()?;
        let mut diff = repo.diff_tree_to_tree(base.as_ref(), Some(&target), None)?;
        diff.find_similar(Some(DiffFindOptions::new().renames(true)))?;

        let workdir = repo.workdir();
        let mut files = Vec::new();
        for delta in diff.deltas() {
            let Some(kind) = delta_to_kind(delta.status()) else {
                continue;
            };
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let old_path = delta
                .old_file()
                .path()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .filter(|o| matches!(kind, FileStatusKind::Renamed | FileStatusKind::Copied) && *o != path);
            let exists_in_workdir = workdir.is_some_and(|w| w.join(&path).is_file());
            let opt_core = |id: git2::Oid| (!id.is_zero()).then(|| to_core_oid(id));
            files.push(FileChange {
                path,
                old_path,
                kind,
                additions: 0,
                deletions: 0,
                is_binary: delta.flags().is_binary(),
                exists_in_workdir,
                old_oid: opt_core(delta.old_file().id()),
                new_oid: opt_core(delta.new_file().id()),
            });
        }

        Ok(Some(AmendCommitPreview {
            short_sha,
            message,
            files,
        }))
    })
}

pub(crate) fn probe_commit_precheck_sync(location: &RepoLocation) -> CommitPrecheckInfo {
    with_thread_repo(&location.git_dir, |repo| {
        let cfg = repo.config().ok();
        let user_name = cfg.as_ref().and_then(|c| c.get_string("user.name").ok());
        let user_email = cfg.as_ref().and_then(|c| c.get_string("user.email").ok());
        let gpg_sign = cfg
            .as_ref()
            .and_then(|c| c.get_bool("commit.gpgsign").ok())
            .unwrap_or(false);

        let hooks_dir = cfg
            .as_ref()
            .and_then(|c| c.get_string("core.hooksPath").ok())
            .map(|p| {
                let pb = std::path::PathBuf::from(p);
                if pb.is_absolute() {
                    pb
                } else {
                    location.workdir.join(pb)
                }
            })
            .unwrap_or_else(|| location.git_dir.join("hooks"));

        let has_hook = |name: &str| -> bool {
            let p = hooks_dir.join(name);
            if !p.is_file() {
                return false;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::metadata(&p)
                    .map(|m| m.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false)
            }
            #[cfg(not(unix))]
            {
                true
            }
        };

        let has_commit_hooks = has_hook("pre-commit")
            || has_hook("prepare-commit-msg")
            || has_hook("commit-msg")
            || has_hook("post-commit");

        let has_remote = repo
            .remotes()
            .map(|r| !r.is_empty())
            .unwrap_or(false);

        Ok(CommitPrecheckInfo {
            has_commit_hooks,
            gpg_sign,
            user_name,
            user_email,
            has_remote,
        })
    })
    .unwrap_or_default()
}

fn parse_author_override<'a>(
    spec: &str,
    fallback_time: git2::Time,
) -> Result<Signature<'a>, EngineError> {
    let trimmed = spec.trim();
    if let (Some(lt), Some(gt)) = (trimmed.rfind('<'), trimmed.rfind('>'))
        && lt < gt
    {
        let name = trimmed[..lt].trim();
        let email = trimmed[lt + 1..gt].trim();
        if !name.is_empty() && !email.is_empty() {
            return Signature::new(name, email, &fallback_time).map_err(EngineError::from);
        }
    }
    Err(EngineError::Other(format!(
        "Invalid author format '{trimmed}', expected 'Name <email>'"
    )))
}

fn append_signoff_if_needed(message: &str, name: &str, email: &str) -> String {
    let trailer = format!("Signed-off-by: {name} <{email}>");
    if message.lines().any(|l| l.trim() == trailer) {
        return message.to_string();
    }
    let mut out = message.trim_end_matches('\n').to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(&trailer);
    out.push('\n');
    out
}

/// Fast-path commit using `git2` (used when no commit hooks or GPG signing are active).
pub(crate) fn execute_commit_git2_sync(
    location: &RepoLocation,
    req: &CommitRequest,
) -> Result<CommitOutcome, EngineError> {
    with_thread_repo(&location.git_dir, |repo| {
        if req.stage_all_tracked {
            let mut index = repo.index()?;
            index.update_all(["*"], None)?;
            index.write()?;
        }

        let mut index = repo.index()?;
        let tree_oid = index.write_tree()?;
        let tree = repo.find_tree(tree_oid)?;

        let committer = repo.signature().map_err(|_| {
            EngineError::Other("Git user.name / user.email not configured".to_string())
        })?;
        let author = match &req.author_override {
            Some(a) if !a.trim().is_empty() => parse_author_override(a, committer.when())?,
            _ => committer.clone(),
        };

        let mut msg = req.message.clone();
        if req.signoff {
            let name = committer.name().unwrap_or("Unknown");
            let email = committer.email().unwrap_or("");
            msg = append_signoff_if_needed(&msg, name, email);
        }

        let oid = if req.amend {
            let head_commit = repo.head()?.peel_to_commit()?;
            let amend_author = if req.author_override.as_ref().is_some_and(|s| !s.trim().is_empty())
            {
                author
            } else {
                head_commit.author().to_owned()
            };
            head_commit.amend(
                Some("HEAD"),
                Some(&amend_author),
                Some(&committer),
                None,
                Some(&msg),
                Some(&tree),
            )?
        } else {
            let parent_commits: Vec<Commit<'_>> = match repo.head() {
                Ok(h) => vec![h.peel_to_commit()?],
                Err(_) => Vec::new(), // Unborn HEAD root commit
            };
            let parent_refs: Vec<&Commit<'_>> = parent_commits.iter().collect();
            repo.commit(
                Some("HEAD"),
                &author,
                &committer,
                &msg,
                &tree,
                &parent_refs,
            )?
        };

        let core_oid = to_core_oid(oid);
        let short = core_oid.short();
        Ok(CommitOutcome {
            success: true,
            commit_oid: Some(core_oid),
            short_sha: Some(short.clone()),
            summary: if req.amend {
                format!("Amended commit {short}")
            } else {
                format!("Committed {short}")
            },
            error_output: None,
            and_push: req.and_push,
            preserved_message: None,
        })
    })
}

/// Builds CLI arguments for `git commit --file=-` when hooks or GPG signing are active.
pub(crate) fn build_commit_cli_args(req: &CommitRequest) -> (Vec<String>, Vec<u8>) {
    let mut args = vec!["commit".to_string(), "--file=-".to_string()];
    if req.amend {
        args.push("--amend".to_string());
    }
    if req.signoff {
        args.push("--signoff".to_string());
    }
    if !req.run_hooks {
        args.push("--no-verify".to_string());
    }
    if let Some(author) = &req.author_override
        && !author.trim().is_empty()
    {
        args.push(format!("--author={}", author.trim()));
    }
    (args, req.message.as_bytes().to_vec())
}

/// Executes a CLI commit (`hooks` or `gpg_sign`) through [`crate::cli::GitCliRunner`],
/// returning both the [`CommitOutcome`] and any detected `index.lock` conflict.
pub(crate) async fn execute_commit_cli_async<R, F>(
    cli: &R,
    loc: &RepoLocation,
    id: u64,
    request: CommitRequest,
    on_cli: F,
) -> (CommitOutcome, Option<crate::cli::IndexLockConflict>)
where
    R: crate::cli::GitCliRunner,
    F: Fn(crate::cli::CliStreamEvent) + Send + Sync,
{
    let (args, stdin_payload) = build_commit_cli_args(&request);
    let res = cli
        .execute_boxed(
            crate::cli::CliCommand {
                id,
                workdir: &loc.workdir,
                git_dir: &loc.git_dir,
                args: &args,
                stdin_payload: Some(&stdin_payload),
                is_read_only: false,
                env: &[],
            },
            &on_cli,
        )
        .await;

    match res {
        Ok(out) if out.exit_code == 0 => {
            let oid = with_thread_repo(&loc.git_dir, |r| {
                Ok(crate::probe::probe_repo_state(r).head_oid)
            })
            .ok()
            .flatten();
            let short = oid.map(|o| o.short());
            let label = short.clone().unwrap_or_default();
            (
                CommitOutcome {
                    success: true,
                    commit_oid: oid,
                    short_sha: short,
                    summary: if request.amend {
                        format!("Amended commit {label}")
                    } else {
                        format!("Committed {label}")
                    },
                    error_output: None,
                    and_push: request.and_push,
                    preserved_message: None,
                },
                None,
            )
        }
        Ok(out) => {
            let index_lock =
                out.index_lock_conflict
                    .map(|(lock_path, message)| crate::cli::IndexLockConflict {
                        lock_path,
                        message,
                        retry_args: args,
                        retry_stdin: Some(stdin_payload),
                        retry_is_read_only: false,
                    });
            let mut combined = out.stderr_lines.clone();
            if combined.is_empty() {
                combined = out.stdout_lines;
            } else if !out.stdout_lines.is_empty() {
                combined.extend(out.stdout_lines);
            }
            (
                CommitOutcome {
                    success: false,
                    commit_oid: None,
                    short_sha: None,
                    summary: format!("Commit failed (exit {})", out.exit_code),
                    error_output: Some(combined.join("\n")),
                    and_push: false,
                    preserved_message: Some(request.message),
                },
                index_lock,
            )
        }
        Err(e) => (
            CommitOutcome {
                success: false,
                commit_oid: None,
                short_sha: None,
                summary: "Commit failed".to_string(),
                error_output: Some(e.to_string()),
                and_push: false,
                preserved_message: Some(request.message),
            },
            None,
        ),
    }
}

/// Soft-resets `HEAD` back to `HEAD~1` (`Git.Uncommit`), preserving all changes in Index.
pub(crate) fn undo_last_commit_sync(location: &RepoLocation) -> Result<String, EngineError> {
    with_thread_repo(&location.git_dir, |repo| {
        let head = repo.head()?;
        let commit = head.peel_to_commit()?;
        let msg = String::from_utf8_lossy(commit.message_bytes())
            .trim_end_matches('\n')
            .to_string();
        if commit.parent_count() > 0 {
            let parent = commit.parent(0)?;
            repo.reset(parent.as_object(), ResetType::Soft, None)?;
        } else {
            // Root commit: detach/delete HEAD target ref so HEAD becomes unborn again
            if let Ok(target_name) = head.name()
                && let Ok(mut reference) = repo.find_reference(target_name)
            {
                reference.delete()?;
            }
        }
        Ok(msg)
    })
}
