use std::collections::HashSet;
use std::path::Path;

use git2::{BranchType, Repository, Sort};
use jbgit_core::{
    CommitSummary, ConflictDocument, ConflictParseOptions, ConflictSideInfo, ConflictSidePresence,
    EpochToken, LoadedConflict, Oid, RefInfo, RefKind, RemoteInfo, RepoSnapshot,
    SpecialConflictInfo, SpecialConflictKind, WorktreeInfo, is_binary_bytes,
    is_reverse_conflict_state,
};
use rustc_hash::FxHashMap;

use crate::error::EngineError;
use crate::probe::{
    RepoLocation, RepoProbeResult, canonical_or_self, probe_repo_state, to_core_oid,
};

use super::scan_config::{
    AHEAD_BEHIND_BRANCH_LIMIT, AHEAD_BEHIND_TIME_BUDGET, AheadBehindUpdate, PendingAheadBehind,
    StatusScanConfig,
};
use super::worker_pool::with_thread_repo;

const GITLINK_MODE: u32 = 0o160_000;
/// Maximum number of new commits scanned by an incremental head revwalk before
/// falling back to a full revwalk (guards against massive rewrites/fetches).
pub const MAX_INCREMENTAL_NEW_COMMITS: usize = 2_000;

/// Cached commit table + `oid -> index` lookup retained across snapshot refreshes
/// for `VcsLogJoiner`-style incremental head joins and OID reuse during pagination.
#[derive(Debug, Clone)]
pub struct SnapshotCommitCache {
    pub commits: Vec<CommitSummary>,
    pub oid_to_index: FxHashMap<Oid, usize>,
    pub tip_oids: HashSet<Oid>,
    /// Requested pagination limit (`max_commits`) for this cached snapshot.
    pub target_limit: usize,
    pub has_more_commits: bool,
    pub is_shallow: bool,
    pub shallow_fingerprint: Option<Vec<u8>>,
    pub load_commit_bodies: bool,
}

impl SnapshotCommitCache {
    #[allow(clippy::too_many_arguments)]
    fn build(
        commits: Vec<CommitSummary>,
        reuse_index: Option<FxHashMap<Oid, usize>>,
        tip_oids: HashSet<Oid>,
        target_limit: usize,
        has_more_commits: bool,
        is_shallow: bool,
        shallow_fingerprint: Option<Vec<u8>>,
        load_commit_bodies: bool,
    ) -> Self {
        let oid_to_index = reuse_index.unwrap_or_else(|| {
            let mut map =
                FxHashMap::with_capacity_and_hasher(commits.len(), Default::default());
            for (idx, c) in commits.iter().enumerate() {
                map.entry(c.oid).or_insert(idx);
            }
            map
        });
        Self {
            commits,
            oid_to_index,
            tip_oids,
            target_limit,
            has_more_commits,
            is_shallow,
            shallow_fingerprint,
            load_commit_bodies,
        }
    }
}

/// Strategy actually taken by [`load_repo_snapshot_incremental_sync`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotLoadKind {
    Full,
    IncrementalNoOp,
    IncrementalJoined { added: usize, removed: usize },
}

/// Requested loading behavior for [`load_repo_snapshot_incremental_sync`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SnapshotLoadMode {
    /// Attempt `VcsLogJoiner` incremental head refresh; fall back to full revwalk
    /// (with cached OID reuse) when needed.
    #[default]
    Incremental,
    /// Run a full revwalk up to `max_commits` while reusing cached `CommitSummary`
    /// records for already-loaded OIDs (used by progressive pagination `LoadMore`).
    Paginate,
    /// Full revwalk from disk without reusing any cached commit data.
    FullNoReuse,
}

fn resolve_rebase_onto_label(repo: &Repository, git_dir: &Path) -> Option<String> {
    let hex = ["rebase-merge", "rebase-apply"]
        .iter()
        .find_map(|d| std::fs::read_to_string(git_dir.join(d).join("onto")).ok())?;
    let oid: Oid = hex.trim().parse().ok()?;
    let target = git2::Oid::from_bytes(oid.as_bytes()).ok();
    if let Some(target_oid) = target
        && let Ok(branches) = repo.branches(None)
    {
        let mut best: Option<(u8, String)> = None;
        for (branch, branch_type) in branches.flatten() {
            if branch.get().target() == Some(target_oid)
                && let Ok(Some(name)) = branch.name()
            {
                let rank = match branch_type {
                    BranchType::Local => 0,
                    BranchType::Remote => 1,
                };
                if best.as_ref().is_none_or(|(r, _)| rank < *r) {
                    best = Some((rank, name.to_string()));
                }
            }
        }
        if let Some((_, name)) = best {
            return Some(name);
        }
    }
    Some(oid.short())
}

fn resolve_rebase_replayed_label(
    state: &jbgit_core::RepositoryState,
    git_dir: &Path,
) -> Option<String> {
    match state {
        jbgit_core::RepositoryState::Rebase { branch: Some(b), .. }
        | jbgit_core::RepositoryState::RebaseInteractive { branch: Some(b), .. }
        | jbgit_core::RepositoryState::RebaseMerge { branch: Some(b), .. } => {
            return Some(b.clone());
        }
        _ => {}
    }
    for (dir, file) in [
        ("rebase-merge", "stopped-sha"),
        ("rebase-apply", "original-commit"),
    ] {
        if let Ok(raw) = std::fs::read_to_string(git_dir.join(dir).join(file))
            && let Ok(oid) = raw.trim().parse::<Oid>()
        {
            return Some(oid.short());
        }
    }
    None
}

fn side_info_from_entry(repo: &Repository, entry: &git2::IndexEntry) -> ConflictSideInfo {
    let size = if entry.mode == GITLINK_MODE {
        None
    } else {
        repo.odb()
            .ok()
            .and_then(|odb| odb.read_header(entry.id).ok())
            .map(|(sz, _)| sz as u64)
    };
    ConflictSideInfo {
        oid: to_core_oid(entry.id),
        size,
        mode: entry.mode,
    }
}

pub(crate) fn load_conflict_document_sync(
    location: &RepoLocation,
    path: &str,
) -> Result<LoadedConflict, EngineError> {
    let norm_path = path.replace('\\', "/");
    let full_path = location.workdir.join(path);

    with_thread_repo(&location.git_dir, |repo| {
        let probe = probe_repo_state(repo);
        let is_reversed = is_reverse_conflict_state(&probe.state);
        let replayed_label = if is_reversed {
            resolve_rebase_replayed_label(&probe.state, &location.git_dir)
        } else {
            None
        };
        let onto_label = if is_reversed {
            resolve_rebase_onto_label(repo, &location.git_dir)
        } else {
            None
        };

        let mut found_conflict = false;
        let mut ancestor_entry = None;
        let mut raw_our = None;
        let mut raw_their = None;

        if let Ok(index) = repo.index()
            && let Ok(conflicts) = index.conflicts()
        {
            for item in conflicts.flatten() {
                let entry_path = item
                    .our
                    .as_ref()
                    .or(item.their.as_ref())
                    .or(item.ancestor.as_ref())
                    .map(|e| String::from_utf8_lossy(&e.path).replace('\\', "/"));
                if entry_path.as_deref() == Some(norm_path.as_str()) {
                    found_conflict = true;
                    ancestor_entry = item.ancestor;
                    raw_our = item.our;
                    raw_their = item.their;
                    break;
                }
            }
        }

        let (yours_entry, theirs_entry) = if is_reversed {
            (raw_their, raw_our)
        } else {
            (raw_our, raw_their)
        };

        let ours_info = yours_entry
            .as_ref()
            .map(|e| side_info_from_entry(repo, e));
        let theirs_info = theirs_entry
            .as_ref()
            .map(|e| side_info_from_entry(repo, e));
        let special_ours_label = replayed_label
            .clone()
            .unwrap_or_else(|| if is_reversed { "replayed" } else { "HEAD" }.to_string());
        let special_theirs_label = onto_label
            .clone()
            .unwrap_or_else(|| if is_reversed { "onto" } else { "Incoming" }.to_string());

        let is_submodule = ancestor_entry
            .as_ref()
            .is_some_and(|e| e.mode == GITLINK_MODE)
            || yours_entry.as_ref().is_some_and(|e| e.mode == GITLINK_MODE)
            || theirs_entry
                .as_ref()
                .is_some_and(|e| e.mode == GITLINK_MODE);
        if is_submodule {
            return Ok(LoadedConflict::Special(SpecialConflictInfo {
                path: path.to_string(),
                kind: SpecialConflictKind::Submodule,
                is_reversed,
                ours: ours_info,
                theirs: theirs_info,
                ours_label: special_ours_label,
                theirs_label: special_theirs_label,
            }));
        }

        let worktree_file_type = std::fs::symlink_metadata(&full_path)
            .ok()
            .map(|m| m.file_type());
        if worktree_file_type.is_some_and(|ft| ft.is_dir()) {
            return Ok(LoadedConflict::Special(SpecialConflictInfo {
                path: path.to_string(),
                kind: SpecialConflictKind::Directory,
                is_reversed,
                ours: ours_info,
                theirs: theirs_info,
                ours_label: special_ours_label,
                theirs_label: special_theirs_label,
            }));
        }

        let read_stage_checked = |entry: &Option<git2::IndexEntry>| -> Result<Option<Vec<u8>>, ()> {
            let Some(e) = entry.as_ref() else {
                return Ok(None);
            };
            match crate::blob_reader::read_blob(repo, e.id, Some(e.mode)) {
                crate::blob_reader::CheckedBlob::Loaded {
                    is_binary: true, ..
                }
                | crate::blob_reader::CheckedBlob::TooLarge { .. } => Err(()),
                crate::blob_reader::CheckedBlob::Loaded { bytes, .. } => Ok(Some(bytes)),
                crate::blob_reader::CheckedBlob::Missing => Ok(None),
            }
        };
        let worktree_checked =
            if worktree_file_type.is_some_and(|ft| ft.is_file() || ft.is_symlink()) {
                match crate::blob_reader::read_worktree_path(&location.workdir, path) {
                    crate::blob_reader::CheckedBlob::Loaded {
                        is_binary: true, ..
                    }
                    | crate::blob_reader::CheckedBlob::TooLarge { .. } => Err(()),
                    crate::blob_reader::CheckedBlob::Loaded { bytes, .. } => Ok(bytes),
                    crate::blob_reader::CheckedBlob::Missing => Ok(Vec::new()),
                }
            } else {
                Ok(Vec::new())
            };

        let (Ok(base_bytes), Ok(yours_bytes), Ok(theirs_bytes), Ok(worktree_bytes)) = (
            read_stage_checked(&ancestor_entry),
            read_stage_checked(&yours_entry),
            read_stage_checked(&theirs_entry),
            worktree_checked,
        ) else {
            return Ok(LoadedConflict::Special(SpecialConflictInfo {
                path: path.to_string(),
                kind: SpecialConflictKind::Binary,
                is_reversed,
                ours: ours_info,
                theirs: theirs_info,
                ours_label: special_ours_label,
                theirs_label: special_theirs_label,
            }));
        };

        let is_binary = base_bytes.as_deref().is_some_and(is_binary_bytes)
            || yours_bytes.as_deref().is_some_and(is_binary_bytes)
            || theirs_bytes.as_deref().is_some_and(is_binary_bytes)
            || is_binary_bytes(&worktree_bytes);
        if is_binary {
            return Ok(LoadedConflict::Special(SpecialConflictInfo {
                path: path.to_string(),
                kind: SpecialConflictKind::Binary,
                is_reversed,
                ours: ours_info,
                theirs: theirs_info,
                ours_label: special_ours_label,
                theirs_label: special_theirs_label,
            }));
        }

        let side_presence = if found_conflict {
            match (yours_entry.is_some(), theirs_entry.is_some()) {
                (true, true) => ConflictSidePresence::BothPresent,
                (true, false) => ConflictSidePresence::DeletedByTheirs,
                (false, true) => ConflictSidePresence::DeletedByOurs,
                (false, false) => ConflictSidePresence::BothDeleted,
            }
        } else {
            ConflictSidePresence::BothPresent
        };

        let worktree_text = String::from_utf8_lossy(&worktree_bytes);
        let base_text = base_bytes
            .as_deref()
            .map(|b| String::from_utf8_lossy(b).into_owned());
        let ours_text = yours_bytes
            .as_deref()
            .map(|b| String::from_utf8_lossy(b).into_owned());
        let theirs_text = theirs_bytes
            .as_deref()
            .map(|b| String::from_utf8_lossy(b).into_owned());

        let parse_opts = ConflictParseOptions {
            is_reversed,
            side_presence,
            default_ours_label: replayed_label.as_deref(),
            default_theirs_label: onto_label.as_deref(),
        };
        let doc = if found_conflict {
            ConflictDocument::from_three_way(
                path,
                &worktree_text,
                base_text.as_deref(),
                ours_text.as_deref(),
                theirs_text.as_deref(),
                parse_opts,
            )
        } else {
            ConflictDocument::parse_with_options(
                path,
                &worktree_text,
                ours_text.as_deref(),
                theirs_text.as_deref(),
                parse_opts,
            )
        };
        Ok(LoadedConflict::Text(doc))
    })
}

pub fn load_repo_snapshot_with_config_sync(
    location: &RepoLocation,
    token: &EpochToken,
    max_commits: usize,
    scan_config: StatusScanConfig,
) -> Result<(RepoSnapshot, Vec<PendingAheadBehind>), EngineError> {
    let (snap, pending, _, _) = load_repo_snapshot_incremental_sync(
        location,
        token,
        max_commits,
        scan_config,
        None,
        SnapshotLoadMode::FullNoReuse,
    )?;
    Ok((snap, pending))
}

/// Loads a [`RepoSnapshot`], preferring a `VcsLogJoiner`-style incremental head
/// refresh against `previous` when `mode == SnapshotLoadMode::Incremental` and
/// falling back safely to a full revwalk whenever history/boundary invariants
/// require it.
pub fn load_repo_snapshot_incremental_sync(
    location: &RepoLocation,
    token: &EpochToken,
    max_commits: usize,
    scan_config: StatusScanConfig,
    previous: Option<&SnapshotCommitCache>,
    mode: SnapshotLoadMode,
) -> Result<
    (
        RepoSnapshot,
        Vec<PendingAheadBehind>,
        SnapshotCommitCache,
        SnapshotLoadKind,
    ),
    EngineError,
> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }

    let max_commits = max_commits.max(1);

    with_thread_repo(&location.git_dir, |repo| {
        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }

        let probe = probe_repo_state(repo);
        let (refs, pending_ahead_behind) = collect_refs(
            repo,
            &probe.head_branch,
            probe.state == jbgit_core::RepositoryState::Detached,
        )?;

        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }

        let shallow_fingerprint = probe
            .is_shallow
            .then(|| {
                std::fs::read(location.common_dir.join("shallow"))
                    .or_else(|_| std::fs::read(location.git_dir.join("shallow")))
                    .ok()
            })
            .flatten();
        let tip_oids = collect_tip_oids(probe.head_oid, &refs);

        let (commits, has_more_commits, load_kind, reuse_index) = if tip_oids.is_empty() {
            (Vec::new(), false, SnapshotLoadKind::Full, None)
        } else if mode == SnapshotLoadMode::Incremental
            && let Some(prev) = previous
            && let Some((commits, has_more, kind)) = try_incremental_commits_refresh(
                repo,
                token,
                max_commits,
                scan_config.load_commit_bodies,
                probe.is_shallow,
                &shallow_fingerprint,
                &tip_oids,
                prev,
            )?
        {
            let idx = (kind == SnapshotLoadKind::IncrementalNoOp).then(|| prev.oid_to_index.clone());
            (commits, has_more, kind, idx)
        } else {
            let reuse = match mode {
                SnapshotLoadMode::FullNoReuse => None,
                SnapshotLoadMode::Incremental | SnapshotLoadMode::Paginate => previous,
            };
            let (commits, has_more) = collect_commits_with_reuse(
                repo,
                token,
                max_commits,
                scan_config.load_commit_bodies,
                reuse,
            )?;
            (commits, has_more, SnapshotLoadKind::Full, None)
        };

        let (user_name, user_email) = match repo.config() {
            Ok(cfg) => (
                cfg.get_string("user.name").ok(),
                cfg.get_string("user.email").ok(),
            ),
            Err(_) => (None, None),
        };

        let recent_branches = {
            let existing: Vec<&str> = refs
                .iter()
                .filter(|r| r.kind == RefKind::LocalBranch)
                .map(|r| r.name.as_str())
                .collect();
            let reflog = read_tail(&location.git_dir.join("logs").join("HEAD"), 64 * 1024);
            jbgit_core::parse_recent_checkouts(
                &reflog,
                probe.head_branch.as_deref(),
                &existing,
                50,
                5,
            )
        };
        let stashes = collect_stashes(repo);
        let ongoing_onto = ["rebase-merge", "rebase-apply"]
            .iter()
            .find_map(|d| std::fs::read_to_string(location.git_dir.join(d).join("onto")).ok())
            .and_then(|hex| hex.trim().parse::<Oid>().ok())
            .map(|oid| {
                refs.iter()
                    .filter(|r| r.target_oid == oid)
                    .min_by_key(|r| match r.kind {
                        RefKind::LocalBranch => 0,
                        RefKind::RemoteBranch => 1,
                        _ => 2,
                    })
                    .map_or_else(|| oid.short(), |r| r.name.clone())
            });

        let remotes = collect_remotes(repo);
        let worktrees = collect_worktrees(location, &probe);
        let submodules = collect_submodules(repo, location);
        let has_lfs = detect_has_lfs(repo, location);
        let loaded_limit = max_commits.max(commits.len());
        let bisect = if probe.state == jbgit_core::RepositoryState::Bisect {
            Some(collect_bisect_info(repo))
        } else {
            None
        };

        let cache = SnapshotCommitCache::build(
            commits.clone(),
            reuse_index,
            tip_oids,
            max_commits,
            has_more_commits,
            probe.is_shallow,
            shallow_fingerprint,
            scan_config.load_commit_bodies,
        );

        Ok((
            RepoSnapshot {
                repo_name: location.repo_name.clone(),
                workdir: location.workdir.clone(),
                git_dir: location.git_dir.clone(),
                common_dir: location.common_dir.clone(),
                state: probe.state,
                bisect,
                is_unborn_head: probe.is_unborn_head,
                head_branch: probe.head_branch,
                head_oid: probe.head_oid,
                refs,
                commits,
                user_name,
                user_email,
                recent_branches,
                stashes,
                ongoing_onto,
                remotes,
                worktrees,
                submodules,
                has_lfs,
                is_shallow: probe.is_shallow,
                loaded_limit,
                has_more_commits,
            },
            pending_ahead_behind,
            cache,
            load_kind,
        ))
    })
}

fn collect_tip_oids(head_oid: Option<Oid>, refs: &[RefInfo]) -> HashSet<Oid> {
    let mut tips = HashSet::with_capacity(refs.len() + usize::from(head_oid.is_some()));
    if let Some(h) = head_oid {
        tips.insert(h);
    }
    for r in refs {
        tips.insert(r.target_oid);
    }
    tips
}

/// Estimates remaining candidate revisions and binary-search steps during an
/// active `git bisect` session from `refs/bisect/bad` and `refs/bisect/good-*`.
fn collect_bisect_info(repo: &Repository) -> jbgit_core::BisectInfo {
    let Ok(bad_oid) = repo.refname_to_id("refs/bisect/bad") else {
        return jbgit_core::BisectInfo::default();
    };
    let mut good_oids = Vec::new();
    if let Ok(refs) = repo.references_glob("refs/bisect/good-*") {
        for r in refs.flatten() {
            if let Some(oid) = r.target() {
                good_oids.push(oid);
            }
        }
    }
    if good_oids.is_empty() {
        return jbgit_core::BisectInfo::default();
    }
    let Ok(mut walk) = repo.revwalk() else {
        return jbgit_core::BisectInfo::default();
    };
    if walk.push(bad_oid).is_err() {
        return jbgit_core::BisectInfo::default();
    }
    for g in good_oids {
        let _ = walk.hide(g);
    }
    // Exclude `bad` itself (already known bad) from remaining candidates.
    let total = walk.take(10_000).filter_map(Result::ok).count();
    let remaining_revs = total.saturating_sub(1);
    let steps = if remaining_revs == 0 {
        0
    } else {
        usize::try_from(remaining_revs.ilog2() + 1).unwrap_or(0)
    };
    jbgit_core::BisectInfo {
        remaining_revisions: Some(remaining_revs),
        remaining_steps: Some(steps),
    }
}

fn detect_has_lfs(repo: &Repository, location: &RepoLocation) -> bool {
    if !location.is_bare
        && let Ok(text) = std::fs::read_to_string(location.workdir.join(".gitattributes"))
    {
        return jbgit_core::has_lfs_filter_in_gitattributes(&text);
    }
    let Ok(head) = repo.head() else {
        return false;
    };
    let Ok(tree) = head.peel_to_tree() else {
        return false;
    };
    let Some(entry) = tree.get_name(".gitattributes") else {
        return false;
    };
    let Ok(blob) = repo.find_blob(entry.id()) else {
        return false;
    };
    if blob.content().len() > 1024 * 1024 {
        return false;
    }
    jbgit_core::has_lfs_filter_in_gitattributes(&String::from_utf8_lossy(blob.content()))
}

/// `git worktree list` — only spawned when `<common>/worktrees` has entries;
/// otherwise the single main working tree is synthesized from the probe.
/// Existing paths are canonicalized so they compare equal to
/// [`RepoLocation::workdir`].
fn collect_worktrees(location: &RepoLocation, probe: &RepoProbeResult) -> Vec<WorktreeInfo> {
    let has_linked = std::fs::read_dir(location.common_dir.join("worktrees"))
        .is_ok_and(|mut d| d.next().is_some());
    if has_linked
        && let Some(mut list) = crate::cli::worktree_list_sync(&location.workdir)
        && !list.is_empty()
    {
        for wt in &mut list {
            if wt.prunable.is_none() && wt.path.exists() {
                wt.path = canonical_or_self(&wt.path);
            }
        }
        return list;
    }
    vec![WorktreeInfo {
        path: location.workdir.clone(),
        head: probe.head_oid,
        branch: probe.head_branch.clone(),
        is_main: true,
        is_bare: location.is_bare,
        detached: !location.is_bare && probe.head_branch.is_none(),
        locked: None,
        prunable: None,
    }]
}

/// Submodule paths from `.gitmodules` (skipped when the file is absent).
fn collect_submodules(repo: &Repository, location: &RepoLocation) -> Vec<String> {
    if location.is_bare || !location.workdir.join(".gitmodules").is_file() {
        return Vec::new();
    }
    let mut out: Vec<String> = repo
        .submodules()
        .map(|subs| {
            subs.iter()
                .map(|s| s.path().to_string_lossy().replace('\\', "/"))
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| jbgit_core::natural_cmp(a, b));
    out
}

/// Reads `remote.*` from the repository config (cheap, no network), sorted
/// with `origin` first then natural order.
fn collect_remotes(repo: &Repository) -> Vec<RemoteInfo> {
    let Ok(names) = repo.remotes() else {
        return Vec::new();
    };
    let mut out: Vec<RemoteInfo> = names
        .iter()
        .flatten()
        .flatten()
        .filter(|s| !s.is_empty())
        .map(|name| {
            let (fetch_url, push_url, push_refspecs) = match repo.find_remote(name) {
                Ok(r) => (
                    r.url().ok().filter(|u| !u.is_empty()).map(ToString::to_string),
                    r.pushurl()
                        .ok()
                        .flatten()
                        .filter(|u| !u.is_empty())
                        .map(ToString::to_string),
                    r.push_refspecs()
                        .ok()
                        .map(|arr| {
                            arr.iter()
                                .flatten()
                                .flatten()
                                .map(ToString::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                ),
                Err(_) => (None, None, Vec::new()),
            };
            RemoteInfo {
                name: name.to_string(),
                fetch_url,
                push_url,
                push_refspecs,
            }
        })
        .collect();
    out.sort_by(|a, b| match (a.name == "origin", b.name == "origin") {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => jbgit_core::natural_cmp(&a.name, &b.name),
    });
    out.dedup_by(|a, b| a.name == b.name);
    out
}

pub fn compute_ahead_behind_batch_sync(
    location: &RepoLocation,
    pending: &[PendingAheadBehind],
    token: &EpochToken,
) -> Result<Vec<AheadBehindUpdate>, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    if pending.is_empty() {
        return Ok(Vec::new());
    }
    with_thread_repo(&location.git_dir, |repo| {
        let mut out = Vec::new();
        for (idx, item) in pending.iter().enumerate() {
            if idx % 16 == 0 && token.is_stale() {
                return Err(EngineError::Cancelled);
            }
            let (Ok(local), Ok(upstream)) = (
                git2::Oid::from_bytes(item.local.as_bytes()),
                git2::Oid::from_bytes(item.upstream.as_bytes()),
            ) else {
                continue;
            };
            if let Ok(ab) = repo.graph_ahead_behind(local, upstream) {
                out.push((item.full_name.clone(), ab));
            }
        }
        Ok(out)
    })
}

/// Reads at most the last `max` bytes of a text file (reflogs can be huge).
fn read_tail(path: &Path, max: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > max {
        let _ = f.seek(SeekFrom::Start(len - max));
    }
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).into_owned();
    if len > max {
        text.split_once('\n')
            .map(|(_, r)| r.to_string())
            .unwrap_or_default()
    } else {
        text
    }
}

/// `stash@{n}` entries from the `refs/stash` reflog (index 0 = newest).
pub(crate) fn collect_stashes(repo: &Repository) -> Vec<jbgit_core::StashEntry> {
    let Ok(reflog) = repo.reflog("refs/stash") else {
        return Vec::new();
    };
    reflog
        .iter()
        .enumerate()
        .map(|(index, entry)| jbgit_core::StashEntry {
            index,
            oid: to_core_oid(entry.id_new()),
            message: entry.message().ok().flatten().unwrap_or("").to_string(),
            timestamp: entry.committer().when().seconds(),
        })
        .collect()
}

struct AheadBehindJob {
    ref_idx: usize,
    full_name: String,
    local: git2::Oid,
    upstream: git2::Oid,
    tip_time: i64,
    is_head: bool,
}

fn collect_refs(
    repo: &Repository,
    head_branch: &Option<String>,
    is_detached: bool,
) -> Result<(Vec<RefInfo>, Vec<PendingAheadBehind>), EngineError> {
    let mut out = Vec::new();
    let mut ahead_behind_jobs: Vec<AheadBehindJob> = Vec::new();

    if let Ok(branches) = repo.branches(None) {
        for item in branches.flatten() {
            let (branch, branch_type) = item;
            let full_name = branch.get().name().ok().unwrap_or_default().to_string();
            let Some(name) = branch.name().ok().flatten().map(ToString::to_string) else {
                continue;
            };
            if branch.get().symbolic_target().ok().flatten().is_some() {
                continue;
            }
            let Ok(commit) = branch.get().peel_to_commit() else {
                continue;
            };
            let target_oid = to_core_oid(commit.id());

            let (kind, upstream, is_head) = match branch_type {
                BranchType::Local => {
                    let is_head = !is_detached && head_branch.as_deref() == Some(name.as_str());
                    let upstream_name = match branch.upstream() {
                        Ok(up) => {
                            if let Ok(up_commit) = up.get().peel_to_commit() {
                                ahead_behind_jobs.push(AheadBehindJob {
                                    ref_idx: out.len(),
                                    full_name: full_name.clone(),
                                    local: commit.id(),
                                    upstream: up_commit.id(),
                                    tip_time: commit.time().seconds(),
                                    is_head,
                                });
                            }
                            up.name().ok().flatten().map(ToString::to_string)
                        }
                        Err(_) => repo
                            .branch_upstream_name(&full_name)
                            .ok()
                            .and_then(|buf| buf.as_str().ok().map(ToString::to_string))
                            .map(|r| r.strip_prefix("refs/remotes/").unwrap_or(&r).to_string()),
                    };
                    (RefKind::LocalBranch, upstream_name, is_head)
                }
                BranchType::Remote => (RefKind::RemoteBranch, None, false),
            };

            out.push(RefInfo {
                name,
                full_name,
                target_oid,
                kind,
                upstream,
                ahead_behind: None,
                is_head,
            });
        }
    }

    ahead_behind_jobs.sort_by_key(|j| (!j.is_head, std::cmp::Reverse(j.tip_time)));
    let started = std::time::Instant::now();
    let mut extra = 0usize;
    let mut deferred = Vec::new();
    let mut budget_exhausted = false;
    for job in ahead_behind_jobs {
        if !job.is_head {
            if budget_exhausted
                || extra >= AHEAD_BEHIND_BRANCH_LIMIT
                || started.elapsed() > AHEAD_BEHIND_TIME_BUDGET
            {
                budget_exhausted = true;
                deferred.push(PendingAheadBehind {
                    full_name: job.full_name,
                    local: to_core_oid(job.local),
                    upstream: to_core_oid(job.upstream),
                });
                continue;
            }
            extra += 1;
        }
        if let Some(info) = out.get_mut(job.ref_idx) {
            info.ahead_behind = repo.graph_ahead_behind(job.local, job.upstream).ok();
        }
    }

    if let Ok(tag_names) = repo.tag_names(None) {
        for tag_name in tag_names.iter().flatten().flatten() {
            let full_name = format!("refs/tags/{tag_name}");
            let Ok(reference) = repo.find_reference(&full_name) else {
                continue;
            };
            let Ok(commit) = reference.peel_to_commit() else {
                continue;
            };
            out.push(RefInfo {
                name: tag_name.to_string(),
                full_name,
                target_oid: to_core_oid(commit.id()),
                kind: RefKind::Tag,
                upstream: None,
                ahead_behind: None,
                is_head: false,
            });
        }
    }

    Ok((out, deferred))
}

#[allow(clippy::too_many_arguments)]
fn try_incremental_commits_refresh(
    repo: &Repository,
    token: &EpochToken,
    max_commits: usize,
    load_commit_bodies: bool,
    is_shallow: bool,
    shallow_fingerprint: &Option<Vec<u8>>,
    new_tip_oids: &HashSet<Oid>,
    prev: &SnapshotCommitCache,
) -> Result<Option<(Vec<CommitSummary>, bool, SnapshotLoadKind)>, EngineError> {
    if prev.commits.is_empty()
        || max_commits != prev.target_limit
        || load_commit_bodies != prev.load_commit_bodies
        || is_shallow != prev.is_shallow
        || *shallow_fingerprint != prev.shallow_fingerprint
    {
        return Ok(None);
    }

    if *new_tip_oids == prev.tip_oids {
        return Ok(Some((
            prev.commits.clone(),
            prev.has_more_commits,
            SnapshotLoadKind::IncrementalNoOp,
        )));
    }

    // Verify every old tip still exists in the ODB so `revwalk.hide` is exact.
    let mut hide_oids = Vec::with_capacity(prev.tip_oids.len());
    for &old_oid in &prev.tip_oids {
        let Ok(raw) = git2::Oid::from_bytes(old_oid.as_bytes()) else {
            return Ok(None);
        };
        if repo.find_commit(raw).is_err() {
            return Ok(None);
        }
        hide_oids.push(raw);
    }

    let mut unseen_new_tips = Vec::new();
    for &tip in new_tip_oids {
        if !prev.tip_oids.contains(&tip) && !prev.oid_to_index.contains_key(&tip) {
            let Ok(raw) = git2::Oid::from_bytes(tip.as_bytes()) else {
                return Ok(None);
            };
            unseen_new_tips.push(raw);
        }
    }

    let first_block = if unseen_new_tips.is_empty() {
        Vec::new()
    } else {
        let mut revwalk = repo.revwalk()?;
        revwalk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
        for raw in unseen_new_tips {
            if revwalk.push(raw).is_err() {
                return Ok(None);
            }
        }
        for raw in hide_oids {
            if revwalk.hide(raw).is_err() {
                return Ok(None);
            }
        }

        let mut block = Vec::new();
        for (idx, oid_res) in revwalk.enumerate() {
            if idx % 256 == 0 && token.is_stale() {
                return Err(EngineError::Cancelled);
            }
            let Ok(raw_oid) = oid_res else {
                continue;
            };
            if block.len() >= MAX_INCREMENTAL_NEW_COMMITS {
                return Ok(None);
            }
            let Ok(commit) = repo.find_commit(raw_oid) else {
                continue;
            };
            block.push(commit_to_summary(&commit, load_commit_bodies));
        }
        block
    };

    let joined = match jbgit_core::graph::join_commits_indexed(
        &prev.commits,
        &prev.oid_to_index,
        &prev.tip_oids,
        &first_block,
        new_tip_oids,
    ) {
        Ok(res) => res,
        Err(_) => return Ok(None),
    };

    // When paginated and pruning shrank below the page target, fall back to a
    // full revwalk so the page stays filled up to `max_commits`.
    if prev.has_more_commits && joined.removed > 0 && joined.commits.len() < max_commits {
        return Ok(None);
    }

    Ok(Some((
        joined.commits,
        prev.has_more_commits,
        SnapshotLoadKind::IncrementalJoined {
            added: joined.added,
            removed: joined.removed,
        },
    )))
}

fn collect_commits_with_reuse(
    repo: &Repository,
    token: &EpochToken,
    max_commits: usize,
    load_commit_bodies: bool,
    reuse: Option<&SnapshotCommitCache>,
) -> Result<(Vec<CommitSummary>, bool), EngineError> {
    let mut revwalk = repo.revwalk()?;
    revwalk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;

    let _ = revwalk.push_head();
    let _ = revwalk.push_glob("refs/heads/*");
    let _ = revwalk.push_glob("refs/remotes/*");
    let _ = revwalk.push_glob("refs/tags/*");

    let reuse_cache = reuse.filter(|c| c.load_commit_bodies == load_commit_bodies);
    let mut commits = Vec::with_capacity(max_commits.min(4096));
    let mut has_more = false;
    for (idx, oid_res) in revwalk.enumerate() {
        if idx % 256 == 0 && token.is_stale() {
            return Err(EngineError::Cancelled);
        }
        let Ok(raw_oid) = oid_res else {
            continue;
        };
        if commits.len() >= max_commits {
            if repo.find_commit(raw_oid).is_ok() {
                has_more = true;
                break;
            }
            continue;
        }
        let core_oid = to_core_oid(raw_oid);
        if let Some(cache) = reuse_cache
            && let Some(&cached_idx) = cache.oid_to_index.get(&core_oid)
            && let Some(cached) = cache.commits.get(cached_idx)
        {
            commits.push(cached.clone());
            continue;
        }
        let Ok(commit) = repo.find_commit(raw_oid) else {
            continue;
        };
        commits.push(commit_to_summary(&commit, load_commit_bodies));
    }

    Ok((commits, has_more))
}

fn commit_to_summary(commit: &git2::Commit<'_>, load_body: bool) -> CommitSummary {
    let parents = commit.parent_ids().map(to_core_oid).collect();
    let author = commit.author();
    let body = if load_body {
        commit
            .body()
            .ok()
            .flatten()
            .unwrap_or("")
            .trim()
            .to_string()
    } else {
        String::new()
    };
    CommitSummary {
        oid: to_core_oid(commit.id()),
        parents,
        author_name: author.name().unwrap_or("Unknown").to_string(),
        author_email: author.email().unwrap_or("").to_string(),
        timestamp: commit.time().seconds(),
        author_timestamp: author.when().seconds(),
        subject: commit.summary().ok().flatten().unwrap_or("").to_string(),
        body,
    }
}
