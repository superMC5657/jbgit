use std::collections::HashSet;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use jbgit_core::graph::{
    EdgesInRowCache, FixedBitSet, GraphViewOptions, PermanentGraph, PrintElementGenerator,
    RawCommitNode, VisibleGraph, VisibleRowIdx, join_commits_indexed,
};
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

/// `MAX_INCREMENTAL_NEW_COMMITS`-sized join (e.g. a large `git pull`): 2,000 new
/// commits with colliding timestamps and periodic merges into a 100k log, which
/// exercised the former `O(K^2)` bucket scan / mid-`Vec` insert in the joiner.
fn bench_incremental_head_join_100k_2000_new(c: &mut Criterion) {
    let (commits, _, _) = build_100k_dag();
    let oid_to_index: FxHashMap<Oid, usize> = commits
        .iter()
        .enumerate()
        .map(|(i, c)| (c.oid, i))
        .collect();
    let prev_refs: HashSet<Oid> = [oid(0)].into_iter().collect();
    let k = 2_000u64;
    let first_block: Vec<RawCommitNode> = (0..k)
        .map(|i| {
            let id = 1_000_000 + i;
            let mut parents = vec![if i + 1 < k { oid(id + 1) } else { oid(0) }];
            if i % 8 == 3 {
                parents.push(oid((i * 7) % 512));
            }
            RawCommitNode {
                oid: oid(id),
                parents,
                timestamp: 200_000 + ((i * 31) % 17) as i64,
            }
        })
        .collect();
    let new_refs: HashSet<Oid> = [oid(1_000_000)].into_iter().collect();
    c.bench_function("incremental_head_join_100k_2000_new_commits", |b| {
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

/// 100k DAG with short merges plus medium (`> LONG_EDGE_SIZE`) and ultra-long
/// (`> WALK_SIZE`) cross edges: `VisibleGraph` projection (unfiltered and a
/// ~50% filter), `recommended_track_width`, and a 60-row viewport of
/// `print_elements` on a persistent `EdgesInRowCache`.
fn bench_visible_graph_track_width_and_viewport_100k(c: &mut Criterion) {
    let (mut commits, refs, head) = build_100k_dag();
    let count = commits.len();
    for (i, commit) in commits.iter_mut().enumerate() {
        if i % 257 == 5 && i + 120 < count {
            commit.parents.push(oid((i + 120) as u64));
        }
        if i % 4099 == 7 && i + 1800 < count {
            commit.parents.push(oid((i + 1800) as u64));
        }
    }
    let pg = PermanentGraph::build(&commits, &refs, head);
    let mut filter = FixedBitSet::with_capacity(count);
    for i in (0..count).filter(|i| i % 3 != 1) {
        filter.insert(i);
    }
    c.bench_function("visible_graph_build_unfiltered_100k", |b| {
        b.iter(|| {
            let vis = VisibleGraph::build(black_box(&pg), GraphViewOptions::default(), None);
            black_box(vis.linear.visible_count())
        });
    });
    c.bench_function("visible_graph_build_filtered_100k", |b| {
        b.iter(|| {
            let vis = VisibleGraph::build(
                black_box(&pg),
                GraphViewOptions::default(),
                Some(black_box(&filter)),
            );
            black_box(vis.linear.visible_count())
        });
    });
    let vis = VisibleGraph::build(&pg, GraphViewOptions::default(), None);
    c.bench_function("recommended_track_width_100k", |b| {
        b.iter(|| {
            let mut cache = EdgesInRowCache::new(&vis.linear);
            let w = PrintElementGenerator::with_cache(&pg, &vis, head, &mut cache)
                .recommended_track_width();
            black_box(w)
        });
    });
    let mut cache = EdgesInRowCache::new(&vis.linear);
    c.bench_function("print_elements_viewport_60_rows_100k", |b| {
        let mut start = 0u32;
        b.iter(|| {
            let mut elem_gen = PrintElementGenerator::with_cache(&pg, &vis, head, &mut cache);
            let mut total = 0usize;
            for row in start..start + 60 {
                total += elem_gen.print_elements(row as VisibleRowIdx).len();
            }
            start = (start + 7) % 90_000;
            black_box(total)
        });
    });
}

criterion_group!(
    benches,
    bench_permanent_graph_100k,
    bench_incremental_head_join_100k,
    bench_incremental_head_join_100k_2000_new,
    bench_visible_graph_track_width_and_viewport_100k
);
criterion_main!(benches);
