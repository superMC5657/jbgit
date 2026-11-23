use std::path::Path;

use git2::{BranchType, Repository, Sort};
use jbgit_core::{
    CommitSummary, ConflictDocument, EpochToken, Oid, RefInfo, RefKind, RemoteInfo, RepoSnapshot,
    WorktreeInfo,
};

use crate::error::EngineError;
use crate::probe::{
    RepoLocation, RepoProbeResult, canonical_or_self, probe_repo_state, to_core_oid,
};

use super::scan_config::{
    AHEAD_BEHIND_BRANCH_LIMIT, AHEAD_BEHIND_TIME_BUDGET, AheadBehindUpdate, PendingAheadBehind,
    StatusScanConfig,
};
use super::worker_pool::with_thread_repo;

pub(crate) fn load_conflict_document_sync(
    location: &RepoLocation,
    path: &str,
) -> Result<ConflictDocument, EngineError> {
    let worktree_bytes = std::fs::read(location.workdir.join(path)).unwrap_or_default();
    let worktree_text = String::from_utf8_lossy(&worktree_bytes);
    let (index_ours, index_theirs) = with_thread_repo(&location.git_dir, |repo| {
        let mut ours = None;
        let mut theirs = None;
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
                if entry_path.as_deref() == Some(path) {
                    if let Some(our) = item.our
                        && let Ok(blob) = repo.find_blob(our.id)
                    {
                        ours = Some(String::from_utf8_lossy(blob.content()).into_owned());
                    }
                    if let Some(their) = item.their
                        && let Ok(blob) = repo.find_blob(their.id)
                    {
                        theirs = Some(String::from_utf8_lossy(blob.content()).into_owned());
                    }
                    break;
                }
            }
        }
        Ok((ours, theirs))
    })?;
    Ok(ConflictDocument::parse(
        path,
        &worktree_text,
        index_ours.as_deref(),
        index_theirs.as_deref(),
    ))
}

pub fn load_repo_snapshot_with_config_sync(
    location: &RepoLocation,
    token: &EpochToken,
    max_commits: usize,
    scan_config: StatusScanConfig,
) -> Result<(RepoSnapshot, Vec<PendingAheadBehind>), EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }

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

        let commits = if probe.is_unborn_head {
            Vec::new()
        } else {
            collect_commits(repo, token, max_commits, scan_config.load_commit_bodies)?
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

        Ok((
            RepoSnapshot {
                repo_name: location.repo_name.clone(),
                workdir: location.workdir.clone(),
                git_dir: location.git_dir.clone(),
                common_dir: location.common_dir.clone(),
                state: probe.state,
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
            },
            pending_ahead_behind,
        ))
    })
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
            let (fetch_url, push_url) = match repo.find_remote(name) {
                Ok(r) => (
                    r.url().ok().filter(|u| !u.is_empty()).map(ToString::to_string),
                    r.pushurl()
                        .ok()
                        .flatten()
                        .filter(|u| !u.is_empty())
                        .map(ToString::to_string),
                ),
                Err(_) => (None, None),
            };
            RemoteInfo {
                name: name.to_string(),
                fetch_url,
                push_url,
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
                        Err(_) => None,
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

fn collect_commits(
    repo: &Repository,
    token: &EpochToken,
    max_commits: usize,
    load_commit_bodies: bool,
) -> Result<Vec<CommitSummary>, EngineError> {
    let mut revwalk = repo.revwalk()?;
    revwalk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;

    let _ = revwalk.push_head();
    let _ = revwalk.push_glob("refs/heads/*");
    let _ = revwalk.push_glob("refs/remotes/*");
    let _ = revwalk.push_glob("refs/tags/*");

    let mut commits = Vec::with_capacity(max_commits.min(1024));
    for (idx, oid_res) in revwalk.enumerate() {
        if idx % 256 == 0 && token.is_stale() {
            return Err(EngineError::Cancelled);
        }
        if idx >= max_commits {
            break;
        }
        let Ok(raw_oid) = oid_res else {
            continue;
        };
        let Ok(commit) = repo.find_commit(raw_oid) else {
            continue;
        };
        commits.push(commit_to_summary(&commit, load_commit_bodies));
    }

    Ok(commits)
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
