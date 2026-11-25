mod scan_config;
mod snapshot;
mod stage_status;
mod worker_pool;

#[cfg(test)]
pub(crate) use scan_config::AHEAD_BEHIND_BRANCH_LIMIT;
pub use scan_config::{
    AheadBehindUpdate, DEFAULT_COMMIT_PAGE_SIZE, LARGE_INDEX_ENTRIES, StatusScanConfig,
    StatusScanMode, StatusScanPolicy,
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
}
