use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

use fixedbitset::FixedBitSet;
use rustc_hash::FxHashMap;

use crate::refs::{
    BranchColorId, MAIN_BRANCH_COLOR_ID, branch_color_id, is_main_branch_name, natural_cmp,
};
use crate::{CommitSummary, Oid, RefInfo, RefKind};

/// Dense node index in the permanent graph (`0 .. nodes_count`), where `0` is the newest/topmost commit.
pub type PermNodeId = u32;
/// Row index in a projected visible graph (`0 .. visible_count`).
pub type VisibleRowIdx = u32;
/// Monotonic track/lane identifier starting at `1` (`1` is the leftmost main spine).
pub type LayoutIndex = u32;

/// Lightweight commit metadata stored in [`PermanentLinearGraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitMetaLite {
    pub oid: Oid,
    pub timestamp: i64,
}

/// Raw input commit record consumed by [`PermanentLinearGraphBuilder`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCommitNode {
    pub oid: Oid,
    pub parents: Vec<Oid>,
    pub timestamp: i64,
}

impl From<&CommitSummary> for RawCommitNode {
    fn from(c: &CommitSummary) -> Self {
        Self {
            oid: c.oid,
            parents: c.parents.clone(),
            timestamp: c.timestamp,
        }
    }
}

/// Typed downward CSR target avoiding sentinel integer encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DownTarget {
    Loaded(PermNodeId),
    Unloaded(u32),
}

impl DownTarget {
    #[inline]
    pub fn as_loaded(self) -> Option<PermNodeId> {
        match self {
            Self::Loaded(id) => Some(id),
            Self::Unloaded(_) => None,
        }
    }

    #[inline]
    pub fn unloaded_index(self) -> Option<usize> {
        match self {
            Self::Loaded(_) => None,
            Self::Unloaded(idx) => Some(idx as usize),
        }
    }
}

/// Compact CSR representation of the full commit DAG (mirroring IDEA `PermanentLinearGraphImpl`).
#[derive(Debug, Clone)]
pub struct PermanentLinearGraph {
    pub nodes_count: u32,
    pub commits: Vec<CommitMetaLite>,
    pub oid_to_node: FxHashMap<Oid, PermNodeId>,
    /// CSR downward edges (to parents), distinguishing loaded nodes from shallow boundary parents.
    pub down_offsets: Vec<u32>,
    pub down_targets: Vec<DownTarget>,
    /// CSR upward edges (to children).
    pub up_offsets: Vec<u32>,
    pub up_targets: Vec<PermNodeId>,
    /// Unloaded boundary parent OIDs indexed by [`DownTarget::Unloaded`].
    pub unloaded_oids: Vec<Oid>,
    /// Mapping from `PermNodeId` back to the input slice index passed to the builder.
    pub perm_to_input: Vec<usize>,
}

impl PermanentLinearGraph {
    #[inline]
    pub fn nodes_count(&self) -> u32 {
        self.nodes_count
    }

    #[inline]
    pub fn commit(&self, node: PermNodeId) -> &CommitMetaLite {
        &self.commits[node as usize]
    }

    #[inline]
    pub fn node_of_oid(&self, oid: &Oid) -> Option<PermNodeId> {
        self.oid_to_node.get(oid).copied()
    }

    #[inline]
    pub fn down_nodes(&self, node: PermNodeId) -> &[DownTarget] {
        let idx = node as usize;
        let start = self.down_offsets[idx] as usize;
        let end = self.down_offsets[idx + 1] as usize;
        &self.down_targets[start..end]
    }

    #[inline]
    pub fn loaded_down_nodes(&self, node: PermNodeId) -> impl Iterator<Item = PermNodeId> + '_ {
        self.down_nodes(node).iter().filter_map(|&t| t.as_loaded())
    }

    #[inline]
    pub fn up_nodes(&self, node: PermNodeId) -> &[PermNodeId] {
        let idx = node as usize;
        let start = self.up_offsets[idx] as usize;
        let end = self.up_offsets[idx + 1] as usize;
        &self.up_targets[start..end]
    }

    #[inline]
    pub fn has_up_nodes(&self, node: PermNodeId) -> bool {
        let idx = node as usize;
        self.up_offsets[idx] < self.up_offsets[idx + 1]
    }

    pub fn unloaded_oid(&self, target: DownTarget) -> Option<Oid> {
        target
            .unloaded_index()
            .and_then(|idx| self.unloaded_oids.get(idx).copied())
    }
}

/// Builds a [`PermanentLinearGraph`] from commit records.
///
/// Handles duplicate parent OIDs (`DuplicateParentFixer`), unloaded shallow/truncated
/// parents (negative boundary IDs `-(unloaded_idx + 2)`), and automatically repairs
/// input slices that are not already in reverse topological order.
pub struct PermanentLinearGraphBuilder;

impl PermanentLinearGraphBuilder {
    pub fn build(commits: &[RawCommitNode]) -> PermanentLinearGraph {
        let n = commits.len();
        let mut input_oid_map: FxHashMap<Oid, usize> =
            FxHashMap::with_capacity_and_hasher(n, Default::default());
        for (idx, c) in commits.iter().enumerate() {
            input_oid_map.entry(c.oid).or_insert(idx);
        }

        // Fast check: is input already in strict reverse topological order (child < parent)?
        let mut is_valid_topo = input_oid_map.len() == n;
        if is_valid_topo {
            'check: for (idx, c) in commits.iter().enumerate() {
                for p_oid in &c.parents {
                    if let Some(&p_idx) = input_oid_map.get(p_oid)
                        && p_idx <= idx
                    {
                        is_valid_topo = false;
                        break 'check;
                    }
                }
            }
        }

        let perm_to_input: Vec<usize> = if is_valid_topo {
            (0..n).collect()
        } else {
            repair_topo_order(commits, &input_oid_map)
        };

        let m = perm_to_input.len();
        let mut oid_to_node: FxHashMap<Oid, PermNodeId> =
            FxHashMap::with_capacity_and_hasher(m, Default::default());
        let mut meta = Vec::with_capacity(m);
        for (perm_idx, &orig_idx) in perm_to_input.iter().enumerate() {
            let c = &commits[orig_idx];
            oid_to_node.insert(c.oid, perm_idx as PermNodeId);
            meta.push(CommitMetaLite {
                oid: c.oid,
                timestamp: c.timestamp,
            });
        }

        let mut down_offsets = Vec::with_capacity(m + 1);
        let mut down_targets: Vec<DownTarget> = Vec::with_capacity(m.saturating_mul(2));
        let mut up_counts = vec![0u32; m];
        let mut unloaded_oids = Vec::new();
        let mut unloaded_map: FxHashMap<Oid, DownTarget> = FxHashMap::default();
        let mut seen_parents: Vec<DownTarget> = Vec::with_capacity(8);

        down_offsets.push(0);
        for &orig_idx in &perm_to_input {
            let c = &commits[orig_idx];
            seen_parents.clear();
            for p_oid in &c.parents {
                if *p_oid == c.oid {
                    // Guard against malformed self-loop commits.
                    continue;
                }
                let target = if let Some(&p_id) = oid_to_node.get(p_oid) {
                    DownTarget::Loaded(p_id)
                } else if let Some(&unloaded) = unloaded_map.get(p_oid) {
                    unloaded
                } else {
                    let unloaded = DownTarget::Unloaded(unloaded_oids.len() as u32);
                    unloaded_oids.push(*p_oid);
                    unloaded_map.insert(*p_oid, unloaded);
                    unloaded
                };

                // DuplicateParentFixer: deduplicate while preserving parent[0] order.
                if !seen_parents.contains(&target) {
                    seen_parents.push(target);
                    down_targets.push(target);
                    if let DownTarget::Loaded(p_id) = target {
                        up_counts[p_id as usize] += 1;
                    }
                }
            }
            down_offsets.push(down_targets.len() as u32);
        }

        let mut up_offsets = Vec::with_capacity(m + 1);
        up_offsets.push(0);
        let mut running = 0u32;
        for &cnt in &up_counts {
            running += cnt;
            up_offsets.push(running);
        }

        let mut up_cursors = up_offsets[..m].to_vec();
        let mut up_targets = vec![0u32; running as usize];
        for node in 0..m {
            let start = down_offsets[node] as usize;
            let end = down_offsets[node + 1] as usize;
            for &target in &down_targets[start..end] {
                if let DownTarget::Loaded(p_id) = target {
                    let p = p_id as usize;
                    let slot = up_cursors[p] as usize;
                    up_targets[slot] = node as PermNodeId;
                    up_cursors[p] += 1;
                }
            }
        }

        PermanentLinearGraph {
            nodes_count: m as u32,
            commits: meta,
            oid_to_node,
            down_offsets,
            down_targets,
            up_offsets,
            up_targets,
            unloaded_oids,
            perm_to_input,
        }
    }
}

fn repair_topo_order(
    commits: &[RawCommitNode],
    input_oid_map: &FxHashMap<Oid, usize>,
) -> Vec<usize> {
    // Deduplicate input commits by first occurrence
    let mut unique_indices: Vec<usize> = Vec::with_capacity(input_oid_map.len());
    let mut seen_oid: FxHashMap<Oid, usize> =
        FxHashMap::with_capacity_and_hasher(input_oid_map.len(), Default::default());
    for (idx, c) in commits.iter().enumerate() {
        if let std::collections::hash_map::Entry::Vacant(e) = seen_oid.entry(c.oid) {
            let compact_idx = unique_indices.len();
            e.insert(compact_idx);
            unique_indices.push(idx);
        }
    }

    let k = unique_indices.len();
    let mut in_degree = vec![0u32; k];
    let mut parents_of: Vec<Vec<usize>> = vec![Vec::new(); k];
    for (u, &orig_idx) in unique_indices.iter().enumerate() {
        for p_oid in &commits[orig_idx].parents {
            if let Some(&v) = seen_oid.get(p_oid)
                && v != u
                && !parents_of[u].contains(&v)
            {
                parents_of[u].push(v);
                in_degree[v] += 1;
            }
        }
    }

    let mut heap: BinaryHeap<Reverse<usize>> = BinaryHeap::new();
    for (u, &deg) in in_degree.iter().enumerate() {
        if deg == 0 {
            heap.push(Reverse(u));
        }
    }

    let mut out = Vec::with_capacity(k);
    let mut emitted = vec![false; k];
    while let Some(Reverse(u)) = heap.pop() {
        emitted[u] = true;
        out.push(unique_indices[u]);
        for &v in &parents_of[u] {
            in_degree[v] -= 1;
            if in_degree[v] == 0 {
                heap.push(Reverse(v));
            }
        }
    }

    // Defensive fallback for malformed cyclic graphs
    if out.len() < k {
        for (u, &orig_idx) in unique_indices.iter().enumerate() {
            if !emitted[u] {
                out.push(orig_idx);
            }
        }
    }
    out
}

/// Strategic priority rank matching IDEA `GitBranchLayoutComparator` (smaller = further left).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BranchLayoutPriority {
    OriginMaster = 0,
    RemoteBranch = 1,
    Master = 2,
    LocalBranch = 3,
    Tag = 4,
    CurrentBranch = 5,
    Head = 6,
    Other = 7,
    Dangling = 8,
}

pub fn ref_layout_priority(r: &RefInfo) -> BranchLayoutPriority {
    match r.kind {
        RefKind::RemoteBranch => {
            if matches!(r.name.as_str(), "origin/main" | "origin/master") {
                BranchLayoutPriority::OriginMaster
            } else {
                BranchLayoutPriority::RemoteBranch
            }
        }
        RefKind::LocalBranch => {
            if is_main_branch_name(&r.name) {
                BranchLayoutPriority::Master
            } else if r.is_head {
                BranchLayoutPriority::CurrentBranch
            } else {
                BranchLayoutPriority::LocalBranch
            }
        }
        RefKind::Tag => BranchLayoutPriority::Tag,
        RefKind::Head => BranchLayoutPriority::Head,
    }
}

pub fn compare_refs_for_layout(a: &RefInfo, b: &RefInfo) -> Ordering {
    ref_layout_priority(a)
        .cmp(&ref_layout_priority(b))
        .then_with(|| natural_cmp(&a.name, &b.name))
}

/// Collects all ref-pointed nodes and topological head nodes (`!has_up_nodes`),
/// deduplicates them, and sorts them using `HeadCommitsComparator` + `GitBranchLayoutComparator`.
pub fn sort_head_nodes(
    graph: &PermanentLinearGraph,
    refs: &[RefInfo],
    head_oid: Option<Oid>,
) -> Vec<PermNodeId> {
    let n = graph.nodes_count as usize;
    let mut best_ref_for_node: Vec<Option<&RefInfo>> = vec![None; n];
    let mut has_detached_head: Vec<bool> = vec![false; n];
    let mut candidates: Vec<PermNodeId> = Vec::new();
    let mut seen = FixedBitSet::with_capacity(n);

    for r in refs {
        if let Some(node) = graph.node_of_oid(&r.target_oid) {
            if !seen.put(node as usize) {
                candidates.push(node);
            }
            let slot = &mut best_ref_for_node[node as usize];
            match slot {
                None => *slot = Some(r),
                Some(prev) => {
                    if compare_refs_for_layout(r, prev) == Ordering::Less {
                        *slot = Some(r);
                    }
                }
            }
        }
    }

    if let Some(h_oid) = head_oid
        && let Some(node) = graph.node_of_oid(&h_oid)
    {
        if !seen.put(node as usize) {
            candidates.push(node);
        }
        if best_ref_for_node[node as usize].is_none() {
            has_detached_head[node as usize] = true;
        }
    }

    for node in 0..graph.nodes_count {
        if !graph.has_up_nodes(node) && !seen.put(node as usize) {
            candidates.push(node);
        }
    }

    candidates.sort_by(|&a, &b| {
        let ra = best_ref_for_node[a as usize];
        let rb = best_ref_for_node[b as usize];
        let pa = ra.map(ref_layout_priority).unwrap_or_else(|| {
            if has_detached_head[a as usize] {
                BranchLayoutPriority::Head
            } else {
                BranchLayoutPriority::Dangling
            }
        });
        let pb = rb.map(ref_layout_priority).unwrap_or_else(|| {
            if has_detached_head[b as usize] {
                BranchLayoutPriority::Head
            } else {
                BranchLayoutPriority::Dangling
            }
        });

        pa.cmp(&pb)
            .then_with(|| match (ra, rb) {
                (Some(ra), Some(rb)) => natural_cmp(&ra.name, &rb.name),
                (None, None) => {
                    let na = if has_detached_head[a as usize] {
                        "HEAD"
                    } else {
                        ""
                    };
                    let nb = if has_detached_head[b as usize] {
                        "HEAD"
                    } else {
                        ""
                    };
                    natural_cmp(na, nb)
                }
                _ => Ordering::Equal,
            })
            .then_with(|| a.cmp(&b))
    });

    candidates
}

/// Output of the iterative DFS track layout (`GraphLayoutImpl` port).
#[derive(Debug, Clone)]
pub struct PermanentGraphLayout {
    /// Track number (`>= 1`) assigned to each `PermNodeId`.
    pub layout_index: Vec<LayoutIndex>,
    /// Ordered dominant head nodes that seeded new DFS components.
    pub head_node_ids: Vec<PermNodeId>,
    /// Dominant head node that first reached each `PermNodeId`.
    pub head_of_node: Vec<PermNodeId>,
    /// Highest `layout_index` assigned in the graph (`0` if empty).
    pub max_layout_index: LayoutIndex,
}

/// Iterative explicit-stack DFS track allocator (mirroring `GraphLayoutBuilder.kt`).
pub struct GraphLayoutBuilder;

impl GraphLayoutBuilder {
    pub fn build(
        graph: &PermanentLinearGraph,
        sorted_heads: &[PermNodeId],
    ) -> PermanentGraphLayout {
        let n = graph.nodes_count as usize;
        let mut layout_index = vec![0u32; n];
        let mut head_of_node = vec![0u32; n];
        let mut important_heads = Vec::new();
        let mut current_layout_index: LayoutIndex = 1;
        let mut stack: Vec<PermNodeId> = Vec::with_capacity(256);

        let run_dfs = |head: PermNodeId,
                       layout_index: &mut [LayoutIndex],
                       head_of_node: &mut [PermNodeId],
                       important_heads: &mut Vec<PermNodeId>,
                       current_layout_index: &mut LayoutIndex,
                       stack: &mut Vec<PermNodeId>| {
            if layout_index[head as usize] != 0 {
                return;
            }
            important_heads.push(head);
            stack.push(head);

            while let Some(&curr) = stack.last() {
                let curr_idx = curr as usize;
                if layout_index[curr_idx] == 0 {
                    layout_index[curr_idx] = *current_layout_index;
                    head_of_node[curr_idx] = head;
                }

                // First-parent priority: parent[0] is checked before parent[1..].
                let next_unvisited_parent = graph
                    .loaded_down_nodes(curr)
                    .find(|&p| layout_index[p as usize] == 0);

                match next_unvisited_parent {
                    Some(parent) => {
                        stack.push(parent);
                    }
                    None => {
                        if layout_index[curr_idx] == *current_layout_index {
                            *current_layout_index += 1;
                        }
                        stack.pop();
                    }
                }
            }
        };

        for &head in sorted_heads {
            run_dfs(
                head,
                &mut layout_index,
                &mut head_of_node,
                &mut important_heads,
                &mut current_layout_index,
                &mut stack,
            );
        }

        // Defensive pass for any node not reached (e.g. synthetic cyclic graphs).
        for node in 0..graph.nodes_count {
            if layout_index[node as usize] == 0 {
                run_dfs(
                    node,
                    &mut layout_index,
                    &mut head_of_node,
                    &mut important_heads,
                    &mut current_layout_index,
                    &mut stack,
                );
            }
        }

        PermanentGraphLayout {
            layout_index,
            head_node_ids: important_heads,
            head_of_node,
            max_layout_index: current_layout_index.saturating_sub(1),
        }
    }
}

/// Fast bitset-based reachability queries over [`PermanentLinearGraph`].
pub struct ReachableNodes;

impl ReachableNodes {
    /// Returns a bitset of all commits reachable downward from `head_nodes`
    /// (inclusive of `head_nodes`). Used by `CurrentBranchHighlighter`.
    pub fn contained_in_branch(
        graph: &PermanentLinearGraph,
        head_nodes: &[PermNodeId],
    ) -> FixedBitSet {
        let n = graph.nodes_count as usize;
        let mut visited = FixedBitSet::with_capacity(n);
        let mut stack = Vec::with_capacity(256);

        for &head in head_nodes {
            if (head as usize) < n && !visited.put(head as usize) {
                stack.push(head);
            }
        }

        while let Some(curr) = stack.pop() {
            for parent in graph.loaded_down_nodes(curr) {
                if !visited.put(parent as usize) {
                    stack.push(parent);
                }
            }
        }

        visited
    }

    /// Returns indices into `refs` for all refs whose commit can reach `target`
    /// (i.e., branches/tags containing `target`).
    pub fn containing_branches<'a>(
        graph: &PermanentLinearGraph,
        target: PermNodeId,
        refs: &'a [RefInfo],
    ) -> Vec<&'a RefInfo> {
        let n = graph.nodes_count as usize;
        if (target as usize) >= n {
            return Vec::new();
        }
        let mut ancestors = FixedBitSet::with_capacity(n);
        let mut stack = Vec::with_capacity(128);
        ancestors.insert(target as usize);
        stack.push(target);

        while let Some(curr) = stack.pop() {
            for &child in graph.up_nodes(curr) {
                if !ancestors.put(child as usize) {
                    stack.push(child);
                }
            }
        }

        refs.iter()
            .filter(|r| {
                graph
                    .node_of_oid(&r.target_oid)
                    .is_some_and(|node| ancestors.contains(node as usize))
            })
            .collect()
    }
}

/// High-level immutable permanent graph combining topology, DFS track layout,
/// and deterministic branch color assignments per track/node.
#[derive(Debug, Clone)]
pub struct PermanentGraph {
    pub linear: PermanentLinearGraph,
    pub layout: PermanentGraphLayout,
    pub sorted_heads: Vec<PermNodeId>,
    /// Deterministic color id for each `PermNodeId`.
    pub node_colors: Vec<BranchColorId>,
}

impl PermanentGraph {
    pub fn build(commits: &[RawCommitNode], refs: &[RefInfo], head_oid: Option<Oid>) -> Self {
        let linear = PermanentLinearGraphBuilder::build(commits);
        let sorted_heads = sort_head_nodes(&linear, refs, head_oid);
        let layout = GraphLayoutBuilder::build(&linear, &sorted_heads);
        let node_colors = compute_node_colors(&linear, &layout, &sorted_heads, refs);

        Self {
            linear,
            layout,
            sorted_heads,
            node_colors,
        }
    }

    #[inline]
    pub fn node_color(&self, node: PermNodeId) -> BranchColorId {
        self.node_colors
            .get(node as usize)
            .copied()
            .unwrap_or(MAIN_BRANCH_COLOR_ID)
    }

    #[inline]
    pub fn layout_index(&self, node: PermNodeId) -> LayoutIndex {
        self.layout.layout_index[node as usize]
    }
}

fn compute_node_colors(
    graph: &PermanentLinearGraph,
    layout: &PermanentGraphLayout,
    sorted_heads: &[PermNodeId],
    refs: &[RefInfo],
) -> Vec<BranchColorId> {
    let n = graph.nodes_count as usize;
    if n == 0 {
        return Vec::new();
    }

    // Best ref for each node (prefer branch refs over tags for color identity).
    let mut best_ref_for_node: Vec<Option<&RefInfo>> = vec![None; n];
    for r in refs {
        if r.kind == RefKind::Head {
            continue;
        }
        if let Some(node) = graph.node_of_oid(&r.target_oid) {
            let slot = &mut best_ref_for_node[node as usize];
            match slot {
                None => *slot = Some(r),
                Some(prev) => {
                    if compare_refs_for_layout(r, prev) == Ordering::Less {
                        *slot = Some(r);
                    }
                }
            }
        }
    }

    // Assign each layout_index track the color of its highest-priority ref head
    // (e.g. merged feature branch whose ref points to parent[1] still claims its track's color).
    let num_tracks = (layout.max_layout_index as usize) + 1;
    let mut track_colors: Vec<Option<BranchColorId>> = vec![None; num_tracks];

    for &head in sorted_heads {
        let li = layout.layout_index[head as usize] as usize;
        if li < num_tracks
            && track_colors[li].is_none()
            && let Some(r) = best_ref_for_node[head as usize]
        {
            track_colors[li] = Some(branch_color_id(&r.name));
        }
    }

    // Also allow any named ref along a track to color its track if the tip head had no ref.
    for (node_idx, maybe_ref) in best_ref_for_node.iter().enumerate() {
        if let Some(r) = maybe_ref {
            let li = layout.layout_index[node_idx] as usize;
            if li < num_tracks && track_colors[li].is_none() {
                track_colors[li] = Some(branch_color_id(&r.name));
            }
        }
    }

    layout
        .layout_index
        .iter()
        .map(|&li| {
            let idx = li as usize;
            if idx < num_tracks
                && let Some(cid) = track_colors[idx]
            {
                return cid;
            }
            if li <= 1 {
                MAIN_BRANCH_COLOR_ID
            } else {
                // Non-zero deterministic color id for unnamed/merged branch fragments.
                BranchColorId((li as i32).saturating_sub(1))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(n: u64) -> Oid {
        let mut bytes = [0u8; 20];
        bytes[..8].copy_from_slice(&n.to_le_bytes());
        Oid::from_bytes(bytes)
    }

    fn raw(id: u64, parents: &[u64], ts: i64) -> RawCommitNode {
        RawCommitNode {
            oid: oid(id),
            parents: parents.iter().copied().map(oid).collect(),
            timestamp: ts,
        }
    }

    fn branch_ref(name: &str, target: u64, kind: RefKind, is_head: bool) -> RefInfo {
        RefInfo {
            name: name.to_string(),
            full_name: match kind {
                RefKind::LocalBranch => format!("refs/heads/{name}"),
                RefKind::RemoteBranch => format!("refs/remotes/{name}"),
                RefKind::Tag => format!("refs/tags/{name}"),
                RefKind::Head => "HEAD".to_string(),
            },
            target_oid: oid(target),
            kind,
            upstream: None,
            ahead_behind: None,
            is_head,
        }
    }

    #[test]
    fn duplicate_parents_and_unloaded_shallow_parents() {
        let commits = vec![raw(1, &[2, 2, 999], 10), raw(2, &[999, 888], 5)];
        let g = PermanentLinearGraphBuilder::build(&commits);
        assert_eq!(g.nodes_count(), 2);
        assert_eq!(
            g.down_nodes(0),
            &[DownTarget::Loaded(1), DownTarget::Unloaded(0)]
        );
        assert_eq!(
            g.down_nodes(1),
            &[DownTarget::Unloaded(0), DownTarget::Unloaded(1)]
        );
        assert_eq!(g.unloaded_oid(DownTarget::Unloaded(0)), Some(oid(999)));
        assert_eq!(g.unloaded_oid(DownTarget::Unloaded(1)), Some(oid(888)));
        assert_eq!(g.up_nodes(1), &[0]);
        assert!(!g.has_up_nodes(0));
    }

    #[test]
    fn origin_main_keeps_layout_index_1_when_head_is_on_feature() {
        // 0: feature/a (HEAD), parent 1 -> 2 (root)
        // 3: main, parent 4 -> 2 (root)
        // 5: origin/main, parent 3
        let commits = vec![
            raw(5, &[3], 50),
            raw(0, &[1], 45),
            raw(1, &[2], 40),
            raw(3, &[4], 30),
            raw(4, &[2], 20),
            raw(2, &[], 10),
        ];
        let refs = vec![
            branch_ref("feature/a", 0, RefKind::LocalBranch, true),
            branch_ref("main", 3, RefKind::LocalBranch, false),
            branch_ref("origin/main", 5, RefKind::RemoteBranch, false),
        ];

        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));
        let n5 = pg.linear.node_of_oid(&oid(5)).unwrap();
        let n3 = pg.linear.node_of_oid(&oid(3)).unwrap();
        let n4 = pg.linear.node_of_oid(&oid(4)).unwrap();
        let n2 = pg.linear.node_of_oid(&oid(2)).unwrap();
        let n0 = pg.linear.node_of_oid(&oid(0)).unwrap();
        let n1 = pg.linear.node_of_oid(&oid(1)).unwrap();

        assert_eq!(pg.layout_index(n5), 1);
        assert_eq!(pg.layout_index(n3), 1);
        assert_eq!(pg.layout_index(n4), 1);
        assert_eq!(pg.layout_index(n2), 1);
        assert_eq!(pg.layout_index(n0), 2);
        assert_eq!(pg.layout_index(n1), 2);
        assert_eq!(pg.node_color(n5), MAIN_BRANCH_COLOR_ID);
        assert_eq!(pg.node_color(n0), branch_color_id("feature/a"));
    }

    #[test]
    fn multi_root_orphan_and_octopus_merge() {
        // Component 1: 0 (merge [1, 2, 3]) -> 1, 2, 3 -> 4 (root A)
        // Component 2 (orphan): 5 -> 6 (root B)
        let commits = vec![
            raw(0, &[1, 2, 3], 70),
            raw(1, &[4], 60),
            raw(2, &[4], 50),
            raw(3, &[4], 40),
            raw(4, &[], 30),
            raw(5, &[6], 20),
            raw(6, &[], 10),
        ];
        let refs = vec![
            branch_ref("main", 0, RefKind::LocalBranch, true),
            branch_ref("gh-pages", 5, RefKind::LocalBranch, false),
        ];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));
        for node in 0..pg.linear.nodes_count() {
            assert!(pg.layout_index(node) >= 1);
        }
        assert_eq!(pg.layout_index(0), 1);
        assert_eq!(pg.layout_index(1), 1);
        assert_eq!(pg.layout_index(4), 1);
        assert_eq!(pg.layout_index(2), 2);
        assert_eq!(pg.layout_index(3), 3);
        assert_eq!(pg.layout_index(5), 4);
        assert_eq!(pg.layout_index(6), 4);
    }

    #[test]
    fn deep_linear_history_200k_no_stack_overflow() {
        let count = 200_000usize;
        let mut commits = Vec::with_capacity(count);
        for i in 0..count {
            let parents = if i + 1 < count {
                vec![oid((i + 1) as u64)]
            } else {
                vec![]
            };
            commits.push(RawCommitNode {
                oid: oid(i as u64),
                parents,
                timestamp: (count - i) as i64,
            });
        }
        let refs = vec![branch_ref("main", 0, RefKind::LocalBranch, true)];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));
        assert_eq!(pg.linear.nodes_count(), count as u32);
        assert_eq!(pg.layout_index(0), 1);
        assert_eq!(pg.layout_index((count - 1) as u32), 1);
    }

    #[test]
    fn reachability_contained_and_containing() {
        // 0 (main) -> [1, 2], 1 -> [3], 2 (feat) -> [3], 3 -> []
        let commits = vec![
            raw(0, &[1, 2], 40),
            raw(1, &[3], 30),
            raw(2, &[3], 20),
            raw(3, &[], 10),
        ];
        let refs = vec![
            branch_ref("main", 0, RefKind::LocalBranch, true),
            branch_ref("feat", 2, RefKind::LocalBranch, false),
        ];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));
        let bs = ReachableNodes::contained_in_branch(&pg.linear, &[2]);
        assert!(bs.contains(2));
        assert!(bs.contains(3));
        assert!(!bs.contains(0));
        assert!(!bs.contains(1));

        let containing_2 = ReachableNodes::containing_branches(&pg.linear, 2, &refs);
        let names: Vec<_> = containing_2.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["main", "feat"]);

        let containing_1 = ReachableNodes::containing_branches(&pg.linear, 1, &refs);
        let names1: Vec<_> = containing_1.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names1, vec!["main"]);
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(1000))]
        #[test]
        fn proptest_1000_random_dags_layout_invariants(
            n in 1usize..=45,
            raw_choices in proptest::collection::vec((0u8..10, proptest::collection::vec(0usize..64, 0..=3)), 45),
            head_raw in 0usize..64,
            feat_raw in 0usize..64,
        ) {
            let mut commits = Vec::with_capacity(n);
            for (i, (kind_roll, parent_rolls)) in raw_choices.iter().take(n).enumerate() {
                let mut parents = Vec::new();
                if i + 1 < n {
                    let p_count = match kind_roll {
                        0 => 0,     // orphan/multiple roots
                        1..=7 => 1, // normal
                        8 => 2,     // merge
                        _ => 3,     // octopus
                    };
                    let remaining = n - 1 - i;
                    for &roll in parent_rolls.iter().take(p_count.min(remaining)) {
                        let p = i + 1 + (roll % remaining);
                        parents.push(oid(p as u64));
                    }
                }
                commits.push(RawCommitNode {
                    oid: oid(i as u64),
                    parents,
                    timestamp: (n - i) as i64,
                });
            }

            let mut refs = Vec::new();
            let head_target = (head_raw % n) as u64;
            refs.push(branch_ref("main", 0, RefKind::LocalBranch, head_target == 0));
            if n > 2 {
                refs.push(branch_ref("origin/main", 0, RefKind::RemoteBranch, false));
                refs.push(branch_ref(
                    "feature/test",
                    (feat_raw % n) as u64,
                    RefKind::LocalBranch,
                    head_target != 0,
                ));
            }

            let pg = PermanentGraph::build(&commits, &refs, Some(oid(head_target)));
            proptest::prop_assert_eq!(pg.linear.nodes_count(), n as u32);

            // Invariant 1: every node has layout_index >= 1
            for node in 0..pg.linear.nodes_count() {
                proptest::prop_assert!(pg.layout_index(node) >= 1);
            }

            // Invariant 2: for every important head, following first-parent while unvisited by higher heads
            // keeps layout_index constant
            for &head in &pg.layout.head_node_ids {
                let expected_li = pg.layout_index(head);
                let mut curr = head;
                loop {
                    let next = pg.linear.loaded_down_nodes(curr).next();
                    match next {
                        Some(p)
                            if pg.layout.head_of_node[p as usize] == head
                                && pg.layout_index(p) == expected_li =>
                        {
                            curr = p;
                        }
                        _ => break,
                    }
                }
            }
        }
    }
}
