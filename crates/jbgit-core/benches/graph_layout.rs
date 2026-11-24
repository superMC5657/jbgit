use std::collections::HashSet;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use jbgit_core::graph::{PermanentGraph, RawCommitNode, join_commits_indexed};
use jbgit_core::{Oid, RefInfo, RefKind};
use rustc_hash::FxHashMap;

fn oid(n: u64) -> Oid {
    let mut bytes = [0u8; 20];
    bytes[..8].copy_from_slice(&n.to_le_bytes());
    Oid::from_bytes(bytes)
}

fn build_100k_dag() -> (Vec<RawCommitNode>, Vec<RefInfo>, Option<Oid>) {
    let count = 100_000usize;
    let mut commits = Vec::with_capacity(count);
    for i in 0..count {
        let mut parents = Vec::with_capacity(2);
        if i + 1 < count {
            parents.push(oid((i + 1) as u64));
        }
        // Periodic merge edges every 64 commits to exercise multi-track DFS
        if i % 64 == 0 && i + 17 < count {
            parents.push(oid((i + 17) as u64));
        }
        commits.push(RawCommitNode {
            oid: oid(i as u64),
            parents,
            timestamp: (count - i) as i64,
        });
    }
    let refs = vec![
        RefInfo {
            name: "origin/main".into(),
            full_name: "refs/remotes/origin/main".into(),
            target_oid: oid(0),
            kind: RefKind::RemoteBranch,
            upstream: None,
            ahead_behind: None,
            is_head: false,
        },
        RefInfo {
            name: "main".into(),
            full_name: "refs/heads/main".into(),
            target_oid: oid(0),
            kind: RefKind::LocalBranch,
            upstream: Some("origin/main".into()),
            ahead_behind: None,
            is_head: true,
        },
    ];
    (commits, refs, Some(oid(0)))
}

fn bench_permanent_graph_100k(c: &mut Criterion) {
    let (commits, refs, head) = build_100k_dag();
    c.bench_function("permanent_graph_and_dfs_layout_100k", |b| {
        b.iter(|| {
            let pg = PermanentGraph::build(black_box(&commits), black_box(&refs), black_box(head));
            black_box(pg.layout.max_layout_index)
        });
    });
}

fn bench_incremental_head_join_100k(c: &mut Criterion) {
    let (commits, _, _) = build_100k_dag();
    let oid_to_index: FxHashMap<Oid, usize> = commits
        .iter()
        .enumerate()
        .map(|(i, c)| (c.oid, i))
        .collect();
    let prev_refs: HashSet<Oid> = [oid(0)].into_iter().collect();
    let new_oid = oid(999_999);
    let new_refs: HashSet<Oid> = [new_oid].into_iter().collect();
    let first_block = vec![RawCommitNode {
        oid: new_oid,
        parents: vec![oid(0)],
        timestamp: 200_000,
    }];
    c.bench_function("incremental_head_join_100k_single_commit", |b| {
        b.iter(|| {
            let res = join_commits_indexed(
                black_box(&commits),
                black_box(&oid_to_index),
                black_box(&prev_refs),
                black_box(&first_block),
                black_box(&new_refs),
            )
            .expect("join succeeds");
            black_box(res.commits.len())
        });
    });
}

criterion_group!(
    benches,
    bench_permanent_graph_100k,
    bench_incremental_head_join_100k
);
criterion_main!(benches);
