mod scan_config;
mod snapshot;
mod stage_status;
mod worker_pool;

#[cfg(test)]
pub(crate) use scan_config::AHEAD_BEHIND_BRANCH_LIMIT;
pub use scan_config::{
    AheadBehindUpdate, COPY_SCAN_MAX_ADDED, COPY_SCAN_MAX_INDEX_ENTRIES, DEFAULT_COMMIT_PAGE_SIZE,
    LARGE_INDEX_ENTRIES, StatusScanConfig, StatusScanMode, StatusScanPolicy,
    copies_from_unmodified_allowed,
};
pub use snapshot::{
    MAX_INCREMENTAL_NEW_COMMITS, SnapshotCommitCache, SnapshotLoadKind, SnapshotLoadMode,
    load_repo_snapshot_incremental_sync, load_repo_snapshot_with_config_sync,
};
pub(crate) use snapshot::{compute_ahead_behind_batch_sync, load_conflict_document_sync};
pub(crate) use stage_status::{delta_to_kind, load_stage_status_with_config_sync};
pub(crate) use worker_pool::{Git2WorkerPool, TaskPriority, with_thread_repo};

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use git2::Repository;
    use jbgit_core::RequestEpoch;

    use crate::error::EngineError;

    use super::*;

    #[test]
    fn interactive_job_runs_while_background_queue_is_saturated() {
        let pool = Git2WorkerPool::new(4);
        let (gate_tx, gate_rx) = crossbeam_channel::bounded::<()>(0);
        let started = Arc::new(AtomicUsize::new(0));

        for _ in 0..12 {
            let gate_rx = gate_rx.clone();
            let started = Arc::clone(&started);
            pool.spawn_with_priority(TaskPriority::Background, move || {
                started.fetch_add(1, Ordering::SeqCst);
                let _ = gate_rx.recv();
            });
        }

        let (done_tx, done_rx) = crossbeam_channel::bounded(1);
        pool.spawn_with_priority(TaskPriority::Interactive, move || {
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "interactive job starved behind background work"
        );
        assert!(started.load(Ordering::SeqCst) <= 3);

        drop(gate_tx);
        drop(pool);
        assert_eq!(started.load(Ordering::SeqCst), 12);
    }

    #[test]
    fn single_worker_prefers_interactive_over_queued_background() {
        let pool = Git2WorkerPool::new(1);
        let order = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = crossbeam_channel::bounded(1);
        let (gate_tx, gate_rx) = crossbeam_channel::bounded::<()>(0);

        {
            let order = Arc::clone(&order);
            pool.spawn_with_priority(TaskPriority::Background, move || {
                let _ = started_tx.send(());
                let _ = gate_rx.recv();
                order.lock().unwrap().push("bg-blocker");
            });
        }
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        for label in ["bg-1", "bg-2"] {
            let order = Arc::clone(&order);
            pool.spawn_with_priority(TaskPriority::Background, move || {
                order.lock().unwrap().push(label);
            });
        }
        {
            let order = Arc::clone(&order);
            pool.spawn_with_priority(TaskPriority::Interactive, move || {
                order.lock().unwrap().push("interactive");
            });
        }
        drop(gate_tx);
        drop(pool);
        assert_eq!(
            *order.lock().unwrap(),
            vec!["bg-blocker", "interactive", "bg-1", "bg-2"]
        );
    }

    #[test]
    fn status_policy_drops_worktree_renames_on_large_index() {
        assert!(StatusScanPolicy::for_index_entries(0).worktree_renames);
        assert!(StatusScanPolicy::for_index_entries(LARGE_INDEX_ENTRIES).worktree_renames);
        assert!(!StatusScanPolicy::for_index_entries(LARGE_INDEX_ENTRIES + 1).worktree_renames);
        assert!(
            StatusScanPolicy::for_index_entries(LARGE_INDEX_ENTRIES + 1).recurse_untracked_dirs
        );

        let custom = StatusScanConfig {
            large_index_entries: 100,
            worktree_renames: StatusScanMode::Always,
            recurse_untracked_dirs: StatusScanMode::Auto,
            load_commit_bodies: false,
            commit_page_size: DEFAULT_COMMIT_PAGE_SIZE,
        };
        let small = custom.policy(50);
        assert!(small.worktree_renames);
        assert!(small.recurse_untracked_dirs);
        let big = custom.policy(150);
        assert!(big.worktree_renames);
        assert!(!big.recurse_untracked_dirs);
    }

    #[test]
    fn collect_refs_defers_excess_ahead_behind_and_backfills_in_batch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let repo = Repository::init(root).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "Test").unwrap();
            cfg.set_str("user.email", "test@example.com").unwrap();
        }
        repo.remote("origin", "https://example.com/repo.git")
            .unwrap();
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new("a.txt")).unwrap();
        let tree_id = idx.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let c1 = repo
            .commit(Some("HEAD"), &sig, &sig, "init\n\nbody text", &tree, &[])
            .unwrap();
        let c1_commit = repo.find_commit(c1).unwrap();

        let total_extra = AHEAD_BEHIND_BRANCH_LIMIT + 5;
        for i in 0..total_extra {
            let remote_ref = format!("refs/remotes/origin/b-{i:02}");
            repo.reference(&remote_ref, c1, true, "remote").unwrap();
            let mut branch = repo.branch(&format!("b-{i:02}"), &c1_commit, true).unwrap();
            branch
                .set_upstream(Some(&format!("origin/b-{i:02}")))
                .unwrap();
        }
        std::fs::write(root.join("a.txt"), "v2\n").unwrap();
        idx.add_path(Path::new("a.txt")).unwrap();
        let tree2 = repo.find_tree(idx.write_tree().unwrap()).unwrap();
        let _c2 = repo
            .commit(
                Some("refs/heads/b-00"),
                &sig,
                &sig,
                "second\n\nsecond body",
                &tree2,
                &[&c1_commit],
            )
            .unwrap();

        let loc = crate::probe::RepoLocation::discover(root).unwrap();
        let epoch = RequestEpoch::new();
        let token = epoch.issue();
        let (snap_no_body, pending) = load_repo_snapshot_with_config_sync(
            &loc,
            &token,
            100,
            StatusScanConfig {
                load_commit_bodies: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(snap_no_body.commits.iter().all(|c| c.body.is_empty()));
        assert_eq!(pending.len(), 5, "excess branches deferred for backfill");

        let updates = compute_ahead_behind_batch_sync(&loc, &pending, &token).unwrap();
        assert_eq!(updates.len(), 5);
        assert!(updates.iter().all(|(_, ab)| *ab == (0, 0)));
    }

    #[tokio::test]
    async fn superseded_epoch_job_is_skipped_at_dequeue() {
        let pool = Git2WorkerPool::new(2);
        let tracker = RequestEpoch::new();
        let stale = tracker.issue();
        let _newer = tracker.issue();
        let ran = Arc::new(AtomicUsize::new(0));
        let ran_job = Arc::clone(&ran);
        let res = pool
            .run_epoch(TaskPriority::Background, stale, move |_| {
                ran_job.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .await;
        assert!(matches!(res, Err(EngineError::Cancelled)));
        assert_eq!(ran.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unshallow_evicts_thread_repo_and_invalidates_commit_cache_parents() {
        use std::process::Command;

        use jbgit_core::{DetailsKey, GitOp};

        use crate::details::load_commit_details_sync;
        use crate::probe::RepoLocation;
        use crate::supervisor::{EngineBackend, EngineEvent, EngineSupervisor, TaskRequest};

        let run_git = |cwd: &Path, args: &[&str]| {
            let out = Command::new("git")
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .args(args)
                .output()
                .expect("failed to execute git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        // 1. Build an upstream repository with 4 commits.
        let upstream_dir = tempfile::tempdir().unwrap();
        let up = upstream_dir.path();
        run_git(up, &["init", "-b", "main"]);
        for idx in 1..=4 {
            std::fs::write(up.join("f.txt"), format!("step {idx}\n")).unwrap();
            run_git(up, &["add", "f.txt"]);
            run_git(up, &["commit", "-m", &format!("commit {idx}")]);
        }

        // 2. Shallow-clone with --depth 1.
        let parent_dir = tempfile::tempdir().unwrap();
        let clone_path = parent_dir.path().join("shallow");
        run_git(
            parent_dir.path(),
            &[
                "clone",
                "--no-local",
                "--depth",
                "1",
                &up.to_string_lossy(),
                &clone_path.to_string_lossy(),
            ],
        );

        let loc = RepoLocation::discover(&clone_path).unwrap();
        let pool = Git2WorkerPool::new(1);
        let epoch = RequestEpoch::new();

        // 3. Load shallow snapshot + commit details on the single worker thread so
        //    THREAD_REPOS caches the git2::Repository with shallow_grafts loaded.
        let loc1 = loc.clone();
        let tok1 = epoch.issue();
        let (shallow_snap, shallow_cache) = pool
            .run_epoch(TaskPriority::Background, tok1, move |tok| {
                let (snap, _, cache, _) = load_repo_snapshot_incremental_sync(
                    &loc1,
                    tok,
                    100,
                    StatusScanConfig::default(),
                    None,
                    SnapshotLoadMode::Incremental,
                )?;
                Ok((snap, cache))
            })
            .await
            .unwrap();
        assert!(shallow_snap.is_shallow);
        assert_eq!(shallow_snap.commits.len(), 1);
        let boundary_oid = shallow_snap.commits[0].oid;
        assert!(
            shallow_snap.commits[0].parents.is_empty(),
            "shallow boundary commit initially appears root-like"
        );

        let loc_det1 = loc.clone();
        let tok_det1 = epoch.issue();
        let shallow_details = pool
            .run_epoch(TaskPriority::Interactive, tok_det1, move |tok| {
                load_commit_details_sync(&loc_det1, &DetailsKey::single(boundary_oid), tok)
            })
            .await
            .unwrap();
        assert!(
            shallow_details.details[0].parents.is_empty(),
            "shallow boundary commit details initially have no parents"
        );

        // 4. Execute GitOp::Unshallow via EngineSupervisor.
        let (handle, mut rx) = EngineSupervisor::start(loc.clone(), false).unwrap();
        let _ = handle.dispatch(TaskRequest::RunGitOp {
            op: GitOp::Unshallow {
                remote: "origin".into(),
            },
        });
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            while let Some(ev) = rx.recv().await {
                if let EngineEvent::GitOpFinished(outcome) = ev {
                    return outcome;
                }
            }
            panic!("engine channel closed before GitOp::Unshallow completed");
        })
        .await
        .expect("timed out waiting for GitOp::Unshallow");
        assert!(outcome.success, "GitOp::Unshallow failed: {outcome:?}");
        let _ = handle.shutdown(Duration::from_secs(5)).await;

        // 5. On the EXACT SAME 1-thread worker pool (reusing THREAD_REPOS) and passing
        //    `Some(&shallow_cache)` in Incremental mode, reload snapshot and commit details.
        let loc2 = loc.clone();
        let tok2 = epoch.issue();
        let (full_snap, load_kind) = pool
            .run_epoch(TaskPriority::Background, tok2, move |tok| {
                let (snap, _, _, kind) = load_repo_snapshot_incremental_sync(
                    &loc2,
                    tok,
                    100,
                    StatusScanConfig::default(),
                    Some(&shallow_cache),
                    SnapshotLoadMode::Incremental,
                )?;
                Ok((snap, kind))
            })
            .await
            .unwrap();
        assert!(!full_snap.is_shallow, "repo must no longer be shallow");
        assert_eq!(load_kind, SnapshotLoadKind::Full);
        assert_eq!(
            full_snap.commits.len(),
            4,
            "all 4 commits must be visible after unshallow on cached worker thread"
        );
        let boundary_after = full_snap
            .commits
            .iter()
            .find(|c| c.oid == boundary_oid)
            .expect("boundary commit still present");
        assert_eq!(
            boundary_after.parents.len(),
            1,
            "former shallow boundary commit must now report its real parent"
        );
        let expected_parent = boundary_after.parents[0];

        // 6. load_commit_details_sync on the same cached worker thread must also see the parent.
        let loc_det2 = loc.clone();
        let tok_det2 = epoch.issue();
        let full_details = pool
            .run_epoch(TaskPriority::Interactive, tok_det2, move |tok| {
                load_commit_details_sync(&loc_det2, &DetailsKey::single(boundary_oid), tok)
            })
            .await
            .unwrap();
        assert_eq!(
            full_details.details[0].parents,
            vec![expected_parent],
            "load_commit_details_sync must return real parent after unshallow"
        );
    }
}
