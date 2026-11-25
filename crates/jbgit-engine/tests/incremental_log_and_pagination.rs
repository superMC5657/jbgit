use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::time::Duration;

use git2::{Repository, Signature, Time};
use jbgit_core::graph::{
    DownTarget, PermanentLinearGraph, PermanentLinearGraphBuilder, RawCommitNode,
};
use jbgit_core::{Oid, RequestEpoch};
use jbgit_engine::{
    EngineBackend, EngineEvent, EngineSupervisor, RepoLocation, SnapshotLoadKind, SnapshotLoadMode,
    StatusScanConfig, load_repo_snapshot_incremental_sync,
};

fn init_repo() -> (tempfile::TempDir, Repository) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = Repository::init(tmp.path()).expect("init repo");
    {
        let mut cfg = repo.config().expect("config");
        cfg.set_str("user.name", "Scale Dev").unwrap();
        cfg.set_str("user.email", "scale@example.com").unwrap();
    }
    (tmp, repo)
}

fn write_commit(
    repo: &Repository,
    ref_name: Option<&str>,
    file: &str,
    content: &str,
    message: &str,
    parents: &[git2::Oid],
    timestamp: i64,
) -> git2::Oid {
    let workdir = repo.workdir().expect("workdir");
    std::fs::write(workdir.join(file), content).expect("write file");
    let mut index = repo.index().expect("index");
    index.add_path(Path::new(file)).expect("add path");
    let tree_id = index.write_tree().expect("write tree");
    let tree = repo.find_tree(tree_id).expect("find tree");
    let sig =
        Signature::new("Scale Dev", "scale@example.com", &Time::new(timestamp, 0)).expect("sig");
    let parent_commits: Vec<git2::Commit<'_>> = parents
        .iter()
        .map(|&id| repo.find_commit(id).expect("parent commit"))
        .collect();
    let parent_refs: Vec<&git2::Commit<'_>> = parent_commits.iter().collect();
    repo.commit(ref_name, &sig, &sig, message, &tree, &parent_refs)
        .expect("commit")
}

fn graph_edges(g: &PermanentLinearGraph) -> BTreeMap<Oid, (Vec<Oid>, BTreeSet<Oid>)> {
    let mut map = BTreeMap::new();
    for id in 0..g.nodes_count() {
        let oid = g.commit(id).oid;
        let downs: Vec<Oid> = g
            .down_nodes(id)
            .iter()
            .map(|&t| match t {
                DownTarget::Loaded(p) => {
                    assert!(
                        p > id,
                        "reverse topological invariant violated: {id} -> {p}"
                    );
                    g.commit(p).oid
                }
                DownTarget::Unloaded(_) => g.unloaded_oid(t).expect("unloaded oid"),
            })
            .collect();
        let ups: BTreeSet<Oid> = g.up_nodes(id).iter().map(|&u| g.commit(u).oid).collect();
        map.insert(oid, (downs, ups));
    }
    map
}

fn assert_equivalent_to_full(
    loc: &RepoLocation,
    incremental_commits: &[jbgit_core::CommitSummary],
    max_commits: usize,
) {
    let epoch = RequestEpoch::new();
    let token = epoch.issue();
    let (full_snap, _, _, full_kind) = load_repo_snapshot_incremental_sync(
        loc,
        &token,
        max_commits,
        StatusScanConfig::default(),
        None,
        SnapshotLoadMode::FullNoReuse,
    )
    .expect("full snapshot");
    assert_eq!(full_kind, SnapshotLoadKind::Full);

    let inc_raw: Vec<RawCommitNode> = incremental_commits
        .iter()
        .map(RawCommitNode::from)
        .collect();
    let full_raw: Vec<RawCommitNode> = full_snap.commits.iter().map(RawCommitNode::from).collect();
    let inc_graph = PermanentLinearGraphBuilder::build(&inc_raw);
    let full_graph = PermanentLinearGraphBuilder::build(&full_raw);

    assert_eq!(
        graph_edges(&inc_graph),
        graph_edges(&full_graph),
        "incremental PermanentLinearGraph must match full reload 100%"
    );
}

#[test]
fn incremental_refresh_matches_full_scan_across_git_mutations() {
    let (tmp, repo) = init_repo();
    let c1 = write_commit(
        &repo,
        Some("HEAD"),
        "a.txt",
        "1\n",
        "c1",
        &[],
        1_700_000_100,
    );
    let c2 = write_commit(
        &repo,
        Some("HEAD"),
        "a.txt",
        "2\n",
        "c2",
        &[c1],
        1_700_000_200,
    );
    let c3 = write_commit(
        &repo,
        Some("HEAD"),
        "a.txt",
        "3\n",
        "c3",
        &[c2],
        1_700_000_300,
    );

    let head_ref_name = repo.head().unwrap().name().unwrap().to_string();
    let loc = RepoLocation::discover(tmp.path()).expect("discover");
    let epoch = RequestEpoch::new();
    let cfg = StatusScanConfig::default();

    // 1. Initial load -> Full
    let (snap0, _, mut cache, kind0) = load_repo_snapshot_incremental_sync(
        &loc,
        &epoch.issue(),
        1_000,
        cfg,
        None,
        SnapshotLoadMode::Incremental,
    )
    .expect("initial");
    assert_eq!(kind0, SnapshotLoadKind::Full);
    assert_eq!(snap0.commits.len(), 3);
    assert!(!snap0.has_more_commits);

    // 2. No-op refresh -> IncrementalNoOp (zero revwalk)
    let (snap_noop, _, next_cache, kind_noop) = load_repo_snapshot_incremental_sync(
        &loc,
        &epoch.issue(),
        1_000,
        cfg,
        Some(&cache),
        SnapshotLoadMode::Incremental,
    )
    .expect("noop");
    assert_eq!(kind_noop, SnapshotLoadKind::IncrementalNoOp);
    assert_eq!(snap_noop.commits, snap0.commits);
    cache = next_cache;

    // 3. Normal commit -> IncrementalJoined (+1, -0)
    let c4 = write_commit(
        &repo,
        Some("HEAD"),
        "a.txt",
        "4\n",
        "c4",
        &[c3],
        1_700_000_400,
    );
    let (snap_commit, _, next_cache, kind_commit) = load_repo_snapshot_incremental_sync(
        &loc,
        &epoch.issue(),
        1_000,
        cfg,
        Some(&cache),
        SnapshotLoadMode::Incremental,
    )
    .expect("commit");
    assert_eq!(
        kind_commit,
        SnapshotLoadKind::IncrementalJoined {
            added: 1,
            removed: 0
        }
    );
    assert_equivalent_to_full(&loc, &snap_commit.commits, 1_000);
    cache = next_cache;

    // 4. Side branch + merge commit -> IncrementalJoined (+2, -0)
    let f1 = write_commit(
        &repo,
        Some("refs/heads/feat"),
        "feat.txt",
        "f1\n",
        "feat 1",
        &[c2],
        1_700_000_450,
    );
    let m1 = write_commit(
        &repo,
        Some("HEAD"),
        "merge.txt",
        "m1\n",
        "merge feat",
        &[c4, f1],
        1_700_000_500,
    );
    let (snap_merge, _, next_cache, kind_merge) = load_repo_snapshot_incremental_sync(
        &loc,
        &epoch.issue(),
        1_000,
        cfg,
        Some(&cache),
        SnapshotLoadMode::Incremental,
    )
    .expect("merge");
    assert_eq!(
        kind_merge,
        SnapshotLoadKind::IncrementalJoined {
            added: 2,
            removed: 0
        }
    );
    assert_equivalent_to_full(&loc, &snap_merge.commits, 1_000);
    cache = next_cache;

    // 5. Branch fast-forward (`feat` -> `m1`) + new tag on `c2`
    repo.reference("refs/heads/feat", m1, true, "ff feat")
        .unwrap();
    repo.reference("refs/tags/v0.1", c2, true, "tag c2")
        .unwrap();
    let (snap_ff, _, next_cache, kind_ff) = load_repo_snapshot_incremental_sync(
        &loc,
        &epoch.issue(),
        1_000,
        cfg,
        Some(&cache),
        SnapshotLoadMode::Incremental,
    )
    .expect("ff+tag");
    assert_eq!(
        kind_ff,
        SnapshotLoadKind::IncrementalJoined {
            added: 0,
            removed: 0
        }
    );
    assert_equivalent_to_full(&loc, &snap_ff.commits, 1_000);
    cache = next_cache;

    // 6. Side branch with 2 unique commits, then delete branch (prunes both commits)
    let d1 = write_commit(
        &repo,
        Some("refs/heads/disposable"),
        "d.txt",
        "d1\n",
        "disp 1",
        &[m1],
        1_700_000_600,
    );
    let _d2 = write_commit(
        &repo,
        Some("refs/heads/disposable"),
        "d.txt",
        "d2\n",
        "disp 2",
        &[d1],
        1_700_000_650,
    );
    let (snap_disp, _, next_cache, kind_disp) = load_repo_snapshot_incremental_sync(
        &loc,
        &epoch.issue(),
        1_000,
        cfg,
        Some(&cache),
        SnapshotLoadMode::Incremental,
    )
    .expect("disp");
    assert_eq!(
        kind_disp,
        SnapshotLoadKind::IncrementalJoined {
            added: 2,
            removed: 0
        }
    );
    assert_equivalent_to_full(&loc, &snap_disp.commits, 1_000);
    cache = next_cache;

    repo.find_reference("refs/heads/disposable")
        .unwrap()
        .delete()
        .unwrap();
    let (snap_del, _, next_cache, kind_del) = load_repo_snapshot_incremental_sync(
        &loc,
        &epoch.issue(),
        1_000,
        cfg,
        Some(&cache),
        SnapshotLoadMode::Incremental,
    )
    .expect("delete branch");
    assert_eq!(
        kind_del,
        SnapshotLoadKind::IncrementalJoined {
            added: 0,
            removed: 2
        }
    );
    assert_equivalent_to_full(&loc, &snap_del.commits, 1_000);
    cache = next_cache;

    // 7. Force-push / reset rewrite on HEAD (drop `feat` pointing at `m1`, rewrite HEAD to `r1 -> c3`)
    repo.find_reference("refs/heads/feat")
        .unwrap()
        .delete()
        .unwrap();
    let r1 = write_commit(
        &repo,
        None,
        "rewrite.txt",
        "r1\n",
        "rewritten head",
        &[c3],
        1_700_000_700,
    );
    repo.reference(&head_ref_name, r1, true, "force reset")
        .unwrap();
    let (snap_rewrite, _, next_cache, kind_rewrite) = load_repo_snapshot_incremental_sync(
        &loc,
        &epoch.issue(),
        1_000,
        cfg,
        Some(&cache),
        SnapshotLoadMode::Incremental,
    )
    .expect("rewrite");
    assert_eq!(
        kind_rewrite,
        SnapshotLoadKind::IncrementalJoined {
            added: 1,
            removed: 3 // m1, c4, f1 all became unreachable
        }
    );
    assert_equivalent_to_full(&loc, &snap_rewrite.commits, 1_000);
    cache = next_cache;

    // 8. Orphan branch (new root commit) -> safely falls back to Full reload
    let _orphan = write_commit(
        &repo,
        Some("refs/heads/orphan"),
        "orphan.txt",
        "o\n",
        "orphan root",
        &[],
        1_700_000_800,
    );
    let (snap_orphan, _, _, kind_orphan) = load_repo_snapshot_incremental_sync(
        &loc,
        &epoch.issue(),
        1_000,
        cfg,
        Some(&cache),
        SnapshotLoadMode::Incremental,
    )
    .expect("orphan fallback");
    assert_eq!(kind_orphan, SnapshotLoadKind::Full);
    assert_equivalent_to_full(&loc, &snap_orphan.commits, 1_000);
}

async fn recv_snapshot(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<EngineEvent>,
    expected_epoch: u64,
) -> std::sync::Arc<jbgit_core::RepoSnapshot> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let ev = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("timed out waiting for RepoSnapshotLoaded")
            .expect("engine event channel closed");
        if let EngineEvent::RepoSnapshotLoaded { snapshot, epoch } = ev
            && epoch == expected_epoch
        {
            return snapshot;
        }
    }
}

#[tokio::test]
async fn progressive_pagination_loads_12_commits_with_page_size_5_via_supervisor() {
    let (tmp, repo) = init_repo();
    let mut prev = None;
    for i in 1..=12usize {
        let parents: Vec<git2::Oid> = prev.into_iter().collect();
        let id = write_commit(
            &repo,
            Some("HEAD"),
            "history.txt",
            &format!("v{i}\n"),
            &format!("commit {i:02}"),
            &parents,
            1_700_000_000 + (i as i64) * 60,
        );
        prev = Some(id);
    }

    let loc = RepoLocation::discover(tmp.path()).expect("discover");
    let cfg = StatusScanConfig {
        commit_page_size: 5,
        ..Default::default()
    };
    let (handle, mut rx) =
        EngineSupervisor::start_with_config(loc.clone(), false, cfg).expect("start supervisor");
    assert_eq!(handle.commit_page_size(), 5);

    // Page 1: 5 commits, has_more_commits == true
    let e1 = handle.request_repo_snapshot();
    let s1 = recv_snapshot(&mut rx, e1).await;
    assert_eq!(s1.commits.len(), 5);
    assert_eq!(s1.loaded_limit, 5);
    assert!(s1.has_more_commits);
    assert!(!s1.is_shallow);

    // Page 2: 10 commits, has_more_commits == true, prefix preserved, zero duplicates
    let e2 = handle.request_load_more_commits();
    let s2 = recv_snapshot(&mut rx, e2).await;
    assert_eq!(s2.commits.len(), 10);
    assert_eq!(s2.loaded_limit, 10);
    assert!(s2.has_more_commits);
    assert_eq!(
        s2.commits[..5].iter().map(|c| c.oid).collect::<Vec<_>>(),
        s1.commits.iter().map(|c| c.oid).collect::<Vec<_>>()
    );
    let uniq2: HashSet<Oid> = s2.commits.iter().map(|c| c.oid).collect();
    assert_eq!(uniq2.len(), 10);

    // Page 3: all 12 commits, has_more_commits == false, zero duplicates
    let e3 = handle.request_load_more_commits();
    let s3 = recv_snapshot(&mut rx, e3).await;
    assert_eq!(s3.commits.len(), 12);
    assert_eq!(s3.loaded_limit, 15);
    assert!(!s3.has_more_commits);
    let uniq3: HashSet<Oid> = s3.commits.iter().map(|c| c.oid).collect();
    assert_eq!(uniq3.len(), 12);
    assert_equivalent_to_full(&loc, &s3.commits, 100);

    // Subsequent incremental refresh after adding commit 13 keeps the expanded limit (15)
    let _c13 = write_commit(
        &repo,
        Some("HEAD"),
        "history.txt",
        "v13\n",
        "commit 13",
        &[prev.unwrap()],
        1_700_002_000,
    );
    let e4 = handle.request_repo_snapshot();
    let s4 = recv_snapshot(&mut rx, e4).await;
    assert_eq!(s4.commits.len(), 13);
    assert!(!s4.has_more_commits);
    assert_equivalent_to_full(&loc, &s4.commits, 100);

    assert!(handle.shutdown(Duration::from_secs(2)).await);
}

#[tokio::test]
async fn exact_page_boundary_reports_has_more_false() {
    let (tmp, repo) = init_repo();
    let mut prev = None;
    for i in 1..=5usize {
        let parents: Vec<git2::Oid> = prev.into_iter().collect();
        let id = write_commit(
            &repo,
            Some("HEAD"),
            "exact.txt",
            &format!("v{i}\n"),
            &format!("exact {i}"),
            &parents,
            1_700_000_000 + (i as i64) * 60,
        );
        prev = Some(id);
    }

    let loc = RepoLocation::discover(tmp.path()).expect("discover");
    let (handle, mut rx) = EngineSupervisor::start_with_config(
        loc,
        false,
        StatusScanConfig {
            commit_page_size: 5,
            ..Default::default()
        },
    )
    .expect("start supervisor");

    let e1 = handle.request_repo_snapshot();
    let s1 = recv_snapshot(&mut rx, e1).await;
    assert_eq!(s1.commits.len(), 5);
    assert_eq!(s1.loaded_limit, 5);
    assert!(
        !s1.has_more_commits,
        "exact page boundary must not report phantom extra commits"
    );

    assert!(handle.shutdown(Duration::from_secs(2)).await);
}
