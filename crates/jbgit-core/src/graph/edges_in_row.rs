use std::collections::VecDeque;

use rustc_hash::FxHashSet;

use crate::graph::permanent::VisibleRowIdx;
use crate::graph::visible::{GraphEdge, VisibleLinearGraph};

pub const BLOCK_SIZE: usize = 40;
pub const WALK_SIZE: usize = 1000;
pub const CACHE_SIZE: usize = 20;

/// Persistent block-anchor cache shared across frames for a given [`VisibleLinearGraph`].
#[derive(Debug, Clone)]
pub struct EdgesInRowCache {
    above_anchor_cache: BlockLruCache,
    below_anchor_cache: BlockLruCache,
    /// Edges spanning strictly more than `WALK_SIZE` rows, ensuring exactness at arbitrary offsets.
    ultra_long_edges: Vec<GraphEdge>,
    hits: usize,
}

impl EdgesInRowCache {
    pub fn new(graph: &VisibleLinearGraph) -> Self {
        let ultra_long_edges = graph
            .down_edges_data
            .iter()
            .copied()
            .filter(|e| (e.down - e.up) as usize > WALK_SIZE)
            .collect();
        Self {
            above_anchor_cache: BlockLruCache::new(CACHE_SIZE),
            below_anchor_cache: BlockLruCache::new(CACHE_SIZE),
            ultra_long_edges,
            hits: 0,
        }
    }

    pub fn cache_hits(&self) -> usize {
        self.hits
    }
}

enum CacheStorage<'a> {
    Owned(EdgesInRowCache),
    Borrowed(&'a mut EdgesInRowCache),
}

impl CacheStorage<'_> {
    #[inline]
    fn get_mut(&mut self) -> &mut EdgesInRowCache {
        match self {
            Self::Owned(c) => c,
            Self::Borrowed(c) => c,
        }
    }
}

/// Bidirectional block-cached active-edge scanner (mirroring IDEA `EdgesInRowGenerator.java`).
///
/// For any `row`, `edges_in_row(row)` returns all edges crossing the gap between `row` and
/// `row + 1` (i.e., `edge.up <= row && row < edge.down`).
pub struct EdgesInRowGenerator<'a> {
    graph: &'a VisibleLinearGraph,
    cache: CacheStorage<'a>,
}

#[derive(Debug, Clone)]
struct BlockLruCache {
    capacity: usize,
    entries: VecDeque<(usize, FxHashSet<GraphEdge>)>,
}

impl BlockLruCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: VecDeque::with_capacity(capacity),
        }
    }

    fn get(&mut self, key: usize) -> Option<FxHashSet<GraphEdge>> {
        let pos = self.entries.iter().position(|(k, _)| *k == key)?;
        if pos + 1 != self.entries.len()
            && let Some(entry) = self.entries.remove(pos)
        {
            self.entries.push_back(entry);
        }
        self.entries.back().map(|(_, v)| v.clone())
    }

    fn insert(&mut self, key: usize, value: FxHashSet<GraphEdge>) {
        if let Some(pos) = self.entries.iter().position(|(k, _)| *k == key) {
            self.entries.remove(pos);
        } else if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back((key, value));
    }
}

impl<'a> EdgesInRowGenerator<'a> {
    pub fn new(graph: &'a VisibleLinearGraph) -> Self {
        Self {
            graph,
            cache: CacheStorage::Owned(EdgesInRowCache::new(graph)),
        }
    }

    pub fn with_cache(graph: &'a VisibleLinearGraph, cache: &'a mut EdgesInRowCache) -> Self {
        Self {
            graph,
            cache: CacheStorage::Borrowed(cache),
        }
    }

    /// Returns all active edges crossing between `row` and `row + 1` (`edge.up <= row < edge.down`).
    pub fn edges_in_row(&mut self, row: VisibleRowIdx) -> Vec<GraphEdge> {
        let n = self.graph.visible_count() as usize;
        let r = row as usize;
        if r >= n {
            return Vec::new();
        }

        let mut active = self.edges_from_above(r);
        let below = self.edges_from_below(r, n);
        active.extend(below);
        for &e in &self.cache.get_mut().ultra_long_edges {
            if e.up <= row && row < e.down {
                active.insert(e);
            }
        }

        let mut out: Vec<GraphEdge> = active.into_iter().collect();
        out.sort_by_key(|e| (e.up, e.down));
        out
    }

    /// Downward scan from `up_anchor = (row / BLOCK_SIZE) * BLOCK_SIZE` catching edges whose `up`
    /// endpoint is within `WALK_SIZE` above `up_anchor`.
    fn edges_from_above(&mut self, row: usize) -> FxHashSet<GraphEdge> {
        let up_anchor = (row / BLOCK_SIZE) * BLOCK_SIZE;
        let cached = {
            let cache = self.cache.get_mut();
            let hit = cache.above_anchor_cache.get(up_anchor);
            if hit.is_some() {
                cache.hits += 1;
            }
            hit
        };
        let mut set = if let Some(cached) = cached {
            cached
        } else {
            let start = up_anchor.saturating_sub(WALK_SIZE);
            let mut s = FxHashSet::default();
            for cur in start..up_anchor {
                self.one_down_step(cur, &mut s);
            }
            self.cache
                .get_mut()
                .above_anchor_cache
                .insert(up_anchor, s.clone());
            s
        };

        for cur in up_anchor..=row {
            self.one_down_step(cur, &mut set);
        }
        set
    }

    /// Upward scan from `down_anchor = min(n - 1, up_anchor + BLOCK_SIZE - 1)` catching edges whose
    /// `down` endpoint is within `WALK_SIZE` below `down_anchor`.
    fn edges_from_below(&mut self, row: usize, n: usize) -> FxHashSet<GraphEdge> {
        let up_anchor = (row / BLOCK_SIZE) * BLOCK_SIZE;
        let down_anchor = (up_anchor + BLOCK_SIZE - 1).min(n - 1);

        let cached = {
            let cache = self.cache.get_mut();
            let hit = cache.below_anchor_cache.get(down_anchor);
            if hit.is_some() {
                cache.hits += 1;
            }
            hit
        };
        let mut set = if let Some(cached) = cached {
            cached
        } else {
            let end = (down_anchor + WALK_SIZE).min(n - 1);
            let mut s = FxHashSet::default();
            for cur in ((down_anchor + 1)..=end).rev() {
                self.one_up_step(cur, n, &mut s);
            }
            self.cache
                .get_mut()
                .below_anchor_cache
                .insert(down_anchor, s.clone());
            s
        };

        for cur in ((row + 1)..=down_anchor).rev() {
            self.one_up_step(cur, n, &mut set);
        }
        set
    }

    #[inline]
    fn one_down_step(&self, row: usize, set: &mut FxHashSet<GraphEdge>) {
        // Edges ending at `row` no longer cross `(row .. row + 1)`.
        for e in self.graph.up_edges(row as VisibleRowIdx) {
            set.remove(e);
        }
        // Edges starting at `row` begin crossing `(row .. row + 1)`.
        for &e in self.graph.down_edges(row as VisibleRowIdx) {
            if (e.down - e.up) as usize <= WALK_SIZE {
                set.insert(e);
            }
        }
    }

    #[inline]
    fn one_up_step(&self, row_plus_1: usize, n: usize, set: &mut FxHashSet<GraphEdge>) {
        if row_plus_1 >= n {
            return;
        }
        // Moving from gap (row_plus_1 ..) upward to gap (row .. row_plus_1):
        // Edges starting at `row_plus_1` do NOT cross (row .. row_plus_1).
        for e in self.graph.down_edges(row_plus_1 as VisibleRowIdx) {
            set.remove(e);
        }
        // Edges ending at `row_plus_1` DO cross (row .. row_plus_1).
        for &e in self.graph.up_edges(row_plus_1 as VisibleRowIdx) {
            if (e.down - e.up) as usize <= WALK_SIZE {
                set.insert(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::permanent::{PermanentGraph, RawCommitNode};
    use crate::graph::visible::{GraphViewOptions, VisibleGraph};
    use crate::{Oid, RefInfo, RefKind};

    fn oid(n: u64) -> Oid {
        let mut bytes = [0u8; 20];
        bytes[..8].copy_from_slice(&n.to_le_bytes());
        Oid::from_bytes(bytes)
    }

    #[test]
    fn edges_in_row_matches_bruteforce_including_ultra_long_edges() {
        let n = 2600usize;
        let mut commits = Vec::with_capacity(n);
        for i in 0..n {
            let mut parents = Vec::new();
            if i + 1 < n {
                parents.push(oid((i + 1) as u64));
            }
            // Add medium (120) and ultra-long (1800) cross-links
            if i == 10 {
                parents.push(oid(1900));
            }
            if i == 50 {
                parents.push(oid(200));
            }
            if i == 1200 {
                parents.push(oid(2500));
            }
            commits.push(RawCommitNode {
                oid: oid(i as u64),
                parents,
                timestamp: (n - i) as i64,
            });
        }
        let refs = vec![RefInfo {
            name: "main".into(),
            full_name: "refs/heads/main".into(),
            target_oid: oid(0),
            kind: RefKind::LocalBranch,
            upstream: None,
            ahead_behind: None,
            is_head: true,
        }];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));
        let vis = VisibleGraph::build(&pg, GraphViewOptions::default(), None);

        let mut edges_gen = EdgesInRowGenerator::new(&vis.linear);
        // Query in shuffled / jumping order to stress LRU cache hits and misses
        let probe_rows = [
            0, 39, 40, 41, 1500, 1499, 1200, 1199, 10, 1900, 2500, 2599, 777,
        ];
        for &r in &probe_rows {
            let actual = edges_gen.edges_in_row(r as VisibleRowIdx);
            let mut expected: Vec<GraphEdge> = vis
                .linear
                .down_edges_data
                .iter()
                .copied()
                .filter(|e| e.up <= r as u32 && (r as u32) < e.down)
                .collect();
            expected.sort_by_key(|e| (e.up, e.down));
            assert_eq!(actual, expected, "mismatch at row {r}");
        }
    }

    #[test]
    fn persistent_edges_cache_records_hits_on_consecutive_viewport_queries() {
        let n = 200usize;
        let mut commits = Vec::with_capacity(n);
        for i in 0..n {
            let mut parents = Vec::new();
            if i + 1 < n {
                parents.push(oid((i + 1) as u64));
            }
            if i == 5 {
                parents.push(oid(80));
            }
            commits.push(RawCommitNode {
                oid: oid(i as u64),
                parents,
                timestamp: (n - i) as i64,
            });
        }
        let refs = vec![RefInfo {
            name: "main".into(),
            full_name: "refs/heads/main".into(),
            target_oid: oid(0),
            kind: RefKind::LocalBranch,
            upstream: None,
            ahead_behind: None,
            is_head: true,
        }];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));
        let vis = VisibleGraph::build(&pg, GraphViewOptions::default(), None);

        let mut cache = EdgesInRowCache::new(&vis.linear);
        assert_eq!(cache.cache_hits(), 0);

        // Frame 1: populate cache anchors around row 10..25
        {
            let mut gen1 = EdgesInRowGenerator::with_cache(&vis.linear, &mut cache);
            for r in 10..25 {
                let _ = gen1.edges_in_row(r as VisibleRowIdx);
            }
        }
        let initial_hits = cache.cache_hits();

        // Frame 2: simulate scrolling/spinner redraw on a persistent cache
        {
            let mut gen2 = EdgesInRowGenerator::with_cache(&vis.linear, &mut cache);
            for r in 12..27 {
                let _ = gen2.edges_in_row(r as VisibleRowIdx);
            }
        }
        assert!(
            cache.cache_hits() > initial_hits,
            "subsequent frame must hit persistent anchor cache: before={initial_hits}, after={}",
            cache.cache_hits()
        );
    }
}
