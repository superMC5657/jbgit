use std::collections::{BTreeMap, HashSet};

use rustc_hash::{FxHashMap, FxHashSet};

use crate::{CommitSummary, Oid};

use super::RawCommitNode;

/// Minimal commit interface required by the incremental log joiner.
pub trait JoinCommit: Clone {
    fn oid(&self) -> Oid;
    fn parents(&self) -> &[Oid];
    fn timestamp(&self) -> i64;
}

impl JoinCommit for CommitSummary {
    #[inline]
    fn oid(&self) -> Oid {
        self.oid
    }

    #[inline]
    fn parents(&self) -> &[Oid] {
        &self.parents
    }

    #[inline]
    fn timestamp(&self) -> i64 {
        self.timestamp
    }
}

impl JoinCommit for RawCommitNode {
    #[inline]
    fn oid(&self) -> Oid {
        self.oid
    }

    #[inline]
    fn parents(&self) -> &[Oid] {
        &self.parents
    }

    #[inline]
    fn timestamp(&self) -> i64 {
        self.timestamp
    }
}

/// Reason an incremental head join cannot be completed safely and must fall back
/// to a full repository revwalk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinError {
    /// A new ref or parent of `first_block` could not be linked into `saved_log`
    /// (e.g. orphan branch, deep unrelated merge, or commit beyond pagination window).
    NotEnoughData,
    /// Red/green reachability pruning reached an inconsistent state or unloaded
    /// parent boundary; caller must fall back to full reload.
    IllegalState,
}

/// Successful output of [`join_commits`] / [`join_commits_indexed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinResult<C> {
    /// Resulting commit table in deterministic reverse topological order.
    pub commits: Vec<C>,
    /// Number of genuinely new commits inserted into the log.
    pub added: usize,
    /// Number of unreachable (red) commits removed from `saved_log`.
    pub removed: usize,
    /// Prefix length of `saved_log` that had to be rebuilt (`saved_log[unsafe_prefix..]`
    /// was reused verbatim).
    pub unsafe_prefix: usize,
}

/// Attaches `first_block` (newest commits from an incremental revwalk) onto
/// `saved_log`, pruning commits that became unreachable when refs moved from
/// `previous_refs` to `new_refs` (mirroring IDEA `VcsLogJoiner`).
///
/// Returns `Err(JoinError)` whenever the join cannot be proven 100% consistent
/// with a full scan so the caller can safely fall back to a full reload.
pub fn join_commits<C: JoinCommit>(
    saved_log: &[C],
    previous_refs: &HashSet<Oid>,
    first_block: &[C],
    new_refs: &HashSet<Oid>,
) -> Result<JoinResult<C>, JoinError> {
    join_commits_impl(saved_log, None, previous_refs, first_block, new_refs)
}

/// Variant of [`join_commits`] that uses a pre-built `oid -> index` map for
/// `O(|unresolved|)` green-boundary lookup and instant missing-link rejection.
pub fn join_commits_indexed<C: JoinCommit>(
    saved_log: &[C],
    oid_to_index: &FxHashMap<Oid, usize>,
    previous_refs: &HashSet<Oid>,
    first_block: &[C],
    new_refs: &HashSet<Oid>,
) -> Result<JoinResult<C>, JoinError> {
    join_commits_impl(
        saved_log,
        Some(oid_to_index),
        previous_refs,
        first_block,
        new_refs,
    )
}

fn join_commits_impl<C: JoinCommit>(
    saved_log: &[C],
    oid_to_index: Option<&FxHashMap<Oid, usize>>,
    previous_refs: &HashSet<Oid>,
    first_block: &[C],
    new_refs: &HashSet<Oid>,
) -> Result<JoinResult<C>, JoinError> {
    if saved_log.is_empty() {
        return Err(JoinError::NotEnoughData);
    }

    let (save_green_index, new_commits) = get_new_commits_and_saved_green_index(
        saved_log,
        oid_to_index,
        previous_refs,
        first_block,
        new_refs,
    )?;

    let (save_red_index, remove_commits) = get_red_commits_and_saved_red_index(
        saved_log,
        oid_to_index,
        previous_refs,
        first_block,
        new_refs,
    )?;

    let unsafe_block_size = save_green_index.max(save_red_index);
    let added = new_commits.len();
    let removed = remove_commits.len();

    if unsafe_block_size == 0 && added == 0 && removed == 0 {
        return Ok(JoinResult {
            commits: saved_log.to_vec(),
            added: 0,
            removed: 0,
            unsafe_prefix: 0,
        });
    }

    let unsafe_part =
        build_unsafe_part_saved_log(saved_log, unsafe_block_size, &remove_commits, first_block);
    let integrated_prefix = integrate_new_commits(unsafe_part, new_commits);

    let mut result =
        Vec::with_capacity(integrated_prefix.len() + saved_log.len() - unsafe_block_size);
    result.extend(integrated_prefix);
    result.extend_from_slice(&saved_log[unsafe_block_size..]);

    Ok(JoinResult {
        commits: result,
        added,
        removed,
        unsafe_prefix: unsafe_block_size,
    })
}

fn get_new_commits_and_saved_green_index<C: JoinCommit>(
    saved_log: &[C],
    oid_to_index: Option<&FxHashMap<Oid, usize>>,
    previous_refs: &HashSet<Oid>,
    first_block: &[C],
    new_refs: &HashSet<Oid>,
) -> Result<(usize, Vec<C>), JoinError> {
    let mut unresolved: FxHashSet<Oid> = FxHashSet::default();
    for oid in new_refs {
        if !previous_refs.contains(oid) {
            unresolved.insert(*oid);
        }
    }
    for commit in first_block {
        unresolved.insert(commit.oid());
        for &p in commit.parents() {
            unresolved.insert(p);
        }
    }
    for commit in first_block {
        if !commit.parents().is_empty() {
            unresolved.remove(&commit.oid());
        }
    }

    let save_green_index = get_first_untracked_index(saved_log, oid_to_index, unresolved)?;

    let existing_in_prefix: FxHashSet<Oid> = saved_log[..save_green_index]
        .iter()
        .map(JoinCommit::oid)
        .collect();
    let mut seen_new: FxHashSet<Oid> = FxHashSet::default();
    let mut all_new_commits = Vec::new();
    for commit in first_block {
        let id = commit.oid();
        let already_in_saved = match oid_to_index {
            Some(map) => map.contains_key(&id),
            None => existing_in_prefix.contains(&id),
        };
        if !already_in_saved && seen_new.insert(id) {
            all_new_commits.push(commit.clone());
        }
    }

    Ok((save_green_index, all_new_commits))
}

fn get_first_untracked_index<C: JoinCommit>(
    commits: &[C],
    oid_to_index: Option<&FxHashMap<Oid, usize>>,
    mut search_hashes: FxHashSet<Oid>,
) -> Result<usize, JoinError> {
    if search_hashes.is_empty() {
        return Ok(0);
    }
    if let Some(map) = oid_to_index {
        let mut max_idx = 0usize;
        for oid in &search_hashes {
            let Some(&idx) = map.get(oid) else {
                return Err(JoinError::NotEnoughData);
            };
            if idx >= commits.len() {
                return Err(JoinError::NotEnoughData);
            }
            if idx + 1 > max_idx {
                max_idx = idx + 1;
            }
        }
        return Ok(max_idx);
    }

    for (last_index, commit) in commits.iter().enumerate() {
        if search_hashes.is_empty() {
            return Ok(last_index);
        }
        search_hashes.remove(&commit.oid());
    }
    if !search_hashes.is_empty() {
        return Err(JoinError::NotEnoughData);
    }
    Ok(commits.len())
}

fn get_red_commits_and_saved_red_index<C: JoinCommit>(
    saved_log: &[C],
    oid_to_index: Option<&FxHashMap<Oid, usize>>,
    previous_refs: &HashSet<Oid>,
    first_block: &[C],
    new_refs: &HashSet<Oid>,
) -> Result<(usize, FxHashSet<Oid>), JoinError> {
    let mut current_red: FxHashSet<Oid> = FxHashSet::default();
    for oid in previous_refs {
        if !new_refs.contains(oid) {
            current_red.insert(*oid);
        }
    }

    let mut recent_parents: FxHashMap<Oid, &[Oid]> = FxHashMap::default();
    let mut current_green: FxHashSet<Oid> = new_refs.iter().copied().collect();
    for commit in first_block {
        recent_parents.insert(commit.oid(), commit.parents());
        current_green.insert(commit.oid());
        for &p in commit.parents() {
            current_green.insert(p);
        }
    }

    // Track the deepest index in `saved_log` whose parent list changed in
    // `first_block` (e.g., shallow/graft parent drop) so red-pruning never
    // exits early before visiting that commit.
    let mut last_changed_parent_idx: Option<usize> = None;
    if !recent_parents.is_empty() {
        if let Some(map) = oid_to_index {
            for commit in first_block {
                if let Some(&idx) = map.get(&commit.oid())
                    && saved_log
                        .get(idx)
                        .is_some_and(|saved| saved.parents() != commit.parents())
                {
                    last_changed_parent_idx =
                        Some(last_changed_parent_idx.map_or(idx, |m| m.max(idx)));
                }
            }
        } else {
            for (idx, c) in saved_log.iter().enumerate() {
                if let Some(&updated) = recent_parents.get(&c.oid())
                    && updated != c.parents()
                {
                    last_changed_parent_idx = Some(idx);
                }
            }
        }
    }

    if current_red.is_empty() && last_changed_parent_idx.is_none() {
        return Ok((0, FxHashSet::default()));
    }

    let mut all_red: FxHashSet<Oid> = FxHashSet::default();
    for (last_index, commit) in saved_log.iter().enumerate() {
        let id = commit.oid();
        if current_green.contains(&id) {
            current_red.remove(&id);
            if let Some(&updated_parents) = recent_parents.get(&id) {
                for &old_p in commit.parents() {
                    if !updated_parents.contains(&old_p) {
                        current_red.insert(old_p);
                    }
                }
            } else {
                for &p in commit.parents() {
                    current_green.insert(p);
                }
            }
        } else {
            if !current_red.remove(&id) {
                return Err(JoinError::IllegalState);
            }
            all_red.insert(id);
            for &p in commit.parents() {
                current_red.insert(p);
            }
        }

        if current_red.is_empty()
            && last_changed_parent_idx.is_none_or(|max_idx| last_index >= max_idx)
        {
            return Ok((last_index + 1, all_red));
        }
    }

    Err(JoinError::IllegalState)
}

fn build_unsafe_part_saved_log<C: JoinCommit>(
    saved_log: &[C],
    unsafe_block_size: usize,
    remove_commits: &FxHashSet<Oid>,
    first_block: &[C],
) -> Vec<C> {
    let recent_by_id: FxHashMap<Oid, &C> = first_block.iter().map(|c| (c.oid(), c)).collect();
    let mut out = Vec::with_capacity(unsafe_block_size);
    for commit in &saved_log[..unsafe_block_size] {
        let id = commit.oid();
        if !remove_commits.contains(&id) {
            if let Some(&fresh) = recent_by_id.get(&id) {
                out.push(fresh.clone());
            } else {
                out.push(commit.clone());
            }
        }
    }
    out
}

/// Deterministic indexed `NewCommitIntegrator`: inserts each new commit into
/// `list` before the first commit that is its parent or has a strictly smaller
/// timestamp, processing parents before children so reverse topological order
/// is strictly preserved.
fn integrate_new_commits<C: JoinCommit>(list: Vec<C>, new_commits: Vec<C>) -> Vec<C> {
    if new_commits.is_empty() {
        return list;
    }

    let order: Vec<Oid> = new_commits.iter().map(JoinCommit::oid).collect();
    let mut new_commits_map: FxHashMap<Oid, C> =
        FxHashMap::with_capacity_and_hasher(new_commits.len(), Default::default());
    let mut parent_oid_set: FxHashSet<Oid> = FxHashSet::default();
    for commit in new_commits {
        for &p in commit.parents() {
            parent_oid_set.insert(p);
        }
        new_commits_map.insert(commit.oid(), commit);
    }

    // Precompute `prefix_min_ts[i] = min_{0 <= j <= i} list[j].timestamp()`
    // (monotonically non-increasing) and parent indices within `list`.
    let mut prefix_min_ts = Vec::with_capacity(list.len());
    let mut parent_indexes: FxHashMap<Oid, usize> = FxHashMap::default();
    let mut min_ts = i64::MAX;
    for (idx, commit) in list.iter().enumerate() {
        min_ts = min_ts.min(commit.timestamp());
        prefix_min_ts.push(min_ts);
        let id = commit.oid();
        if parent_oid_set.contains(&id) {
            parent_indexes.insert(id, idx);
        }
    }

    let mut inserted_by_index: BTreeMap<usize, Vec<C>> = BTreeMap::new();
    let mut stack: Vec<C> = Vec::new();

    for root_oid in order {
        let Some(root) = new_commits_map.remove(&root_oid) else {
            continue;
        };
        stack.push(root);
        while let Some(current) = stack.last() {
            let next_parent = current
                .parents()
                .iter()
                .copied()
                .find(|p| new_commits_map.contains_key(p));
            if let Some(parent_oid) = next_parent {
                if let Some(parent_commit) = new_commits_map.remove(&parent_oid) {
                    stack.push(parent_commit);
                }
                continue;
            }
            let commit = stack.pop().expect("stack non-empty");
            insert_indexed(
                commit,
                &prefix_min_ts,
                &parent_indexes,
                &mut inserted_by_index,
            );
        }
    }

    let inserted_count: usize = inserted_by_index.values().map(Vec::len).sum();
    let mut result = Vec::with_capacity(list.len() + inserted_count);
    let mut from = 0usize;
    let mut list_iter = list.into_iter();
    for (idx, bucket) in inserted_by_index {
        let take_n = idx - from;
        for _ in 0..take_n {
            if let Some(item) = list_iter.next() {
                result.push(item);
            }
        }
        result.extend(bucket);
        from = idx;
    }
    result.extend(list_iter);
    result
}

fn first_older_index(prefix_min_ts: &[i64], timestamp: i64) -> usize {
    let mut low = 0usize;
    let mut high = prefix_min_ts.len();
    while low < high {
        let mid = low + (high - low) / 2;
        if prefix_min_ts[mid] < timestamp {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    low
}

fn insert_indexed<C: JoinCommit>(
    commit: C,
    prefix_min_ts: &[i64],
    parent_indexes: &FxHashMap<Oid, usize>,
    inserted_by_index: &mut BTreeMap<usize, Vec<C>>,
) {
    let ts = commit.timestamp();
    let mut index = first_older_index(prefix_min_ts, ts);
    for p in commit.parents() {
        if let Some(&parent_idx) = parent_indexes.get(p)
            && parent_idx < index
        {
            index = parent_idx;
        }
    }

    for (_, bucket) in inserted_by_index.range_mut(..=index) {
        if let Some(pos) = bucket
            .iter()
            .position(|other| other.timestamp() < ts || commit.parents().contains(&other.oid()))
        {
            bucket.insert(pos, commit);
            return;
        }
    }

    inserted_by_index.entry(index).or_default().push(commit);
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashSet};

    use super::*;
    use crate::graph::{PermanentLinearGraph, PermanentLinearGraphBuilder};

    fn oid(n: u64) -> Oid {
        let mut bytes = [0u8; 20];
        bytes[..8].copy_from_slice(&n.to_le_bytes());
        Oid::from_bytes(bytes)
    }

    fn node(id: u64, parents: &[u64], ts: i64) -> RawCommitNode {
        RawCommitNode {
            oid: oid(id),
            parents: parents.iter().copied().map(oid).collect(),
            timestamp: ts,
        }
    }

    fn refs(ids: &[u64]) -> HashSet<Oid> {
        ids.iter().copied().map(oid).collect()
    }

    fn index_map(commits: &[RawCommitNode]) -> FxHashMap<Oid, usize> {
        commits
            .iter()
            .enumerate()
            .map(|(i, c)| (c.oid, i))
            .collect()
    }

    /// Asserts that two `PermanentLinearGraph`s have the exact same commit set,
    /// strict reverse-topological ordering, and identical parent/child edges.
    fn assert_graph_equivalent(a: &PermanentLinearGraph, b: &PermanentLinearGraph) {
        assert_eq!(a.nodes_count(), b.nodes_count(), "node count mismatch");
        let edges_of = |g: &PermanentLinearGraph| -> BTreeMap<Oid, (Vec<Oid>, BTreeSet<Oid>)> {
            let mut map = BTreeMap::new();
            for id in 0..g.nodes_count() {
                let c_oid = g.commit(id).oid;
                let downs: Vec<Oid> = g
                    .down_nodes(id)
                    .iter()
                    .map(|&t| match t {
                        crate::graph::DownTarget::Loaded(p) => {
                            assert!(p > id, "reverse topo violated: {id} -> {p}");
                            g.commit(p).oid
                        }
                        crate::graph::DownTarget::Unloaded(_) => {
                            g.unloaded_oid(t).expect("unloaded oid")
                        }
                    })
                    .collect();
                let ups: BTreeSet<Oid> = g.up_nodes(id).iter().map(|&u| g.commit(u).oid).collect();
                map.insert(c_oid, (downs, ups));
            }
            map
        };
        assert_eq!(edges_of(a), edges_of(b));
    }

    #[test]
    fn normal_commit_fast_forward_and_indexed_parity() {
        let saved = vec![node(3, &[2], 300), node(2, &[1], 200), node(1, &[], 100)];
        let first_block = vec![node(4, &[3], 400)];
        let full = vec![
            node(4, &[3], 400),
            node(3, &[2], 300),
            node(2, &[1], 200),
            node(1, &[], 100),
        ];

        let r1 = join_commits(&saved, &refs(&[3]), &first_block, &refs(&[4])).unwrap();
        let r2 = join_commits_indexed(
            &saved,
            &index_map(&saved),
            &refs(&[3]),
            &first_block,
            &refs(&[4]),
        )
        .unwrap();
        assert_eq!(r1, r2);
        assert_eq!(r1.added, 1);
        assert_eq!(r1.removed, 0);
        assert_eq!(r1.unsafe_prefix, 1);
        assert_graph_equivalent(
            &PermanentLinearGraphBuilder::build(&r1.commits),
            &PermanentLinearGraphBuilder::build(&full),
        );
    }

    #[test]
    fn merge_commit_with_side_branch_and_clock_skew() {
        // Saved: main at 3 -> 2 -> 1.
        // New: merge 6 (parents 3, 5) with 5 -> 4 -> 2, where 4 has clock skew (ts=150 < ts(2)=200).
        let saved = vec![node(3, &[2], 300), node(2, &[1], 200), node(1, &[], 100)];
        let first_block = vec![
            node(6, &[3, 5], 600),
            node(5, &[4], 500),
            node(4, &[2], 150),
        ];
        let full = vec![
            node(6, &[3, 5], 600),
            node(5, &[4], 500),
            node(3, &[2], 300),
            node(4, &[2], 150),
            node(2, &[1], 200),
            node(1, &[], 100),
        ];

        let joined = join_commits_indexed(
            &saved,
            &index_map(&saved),
            &refs(&[3]),
            &first_block,
            &refs(&[6]),
        )
        .unwrap();
        assert_eq!(joined.added, 3);
        assert_eq!(joined.removed, 0);
        // Verify commit 4 comes before its parent 2 despite clock skew.
        let pos4 = joined.commits.iter().position(|c| c.oid == oid(4)).unwrap();
        let pos2 = joined.commits.iter().position(|c| c.oid == oid(2)).unwrap();
        assert!(
            pos4 < pos2,
            "child 4 must precede parent 2 despite clock skew"
        );
        assert_graph_equivalent(
            &PermanentLinearGraphBuilder::build(&joined.commits),
            &PermanentLinearGraphBuilder::build(&full),
        );
    }

    #[test]
    fn reset_and_force_push_prune_unreachable_commits() {
        // Saved: 4 -> 3 -> 2 -> 1, main=4.
        // Force-push / amend replaces 4,3 with 5 -> 2.
        let saved = vec![
            node(4, &[3], 400),
            node(3, &[2], 300),
            node(2, &[1], 200),
            node(1, &[], 100),
        ];
        let first_block = vec![node(5, &[2], 500)];
        let full = vec![node(5, &[2], 500), node(2, &[1], 200), node(1, &[], 100)];

        let joined = join_commits_indexed(
            &saved,
            &index_map(&saved),
            &refs(&[4]),
            &first_block,
            &refs(&[5]),
        )
        .unwrap();
        assert_eq!(joined.added, 1);
        assert_eq!(joined.removed, 2);
        assert_graph_equivalent(
            &PermanentLinearGraphBuilder::build(&joined.commits),
            &PermanentLinearGraphBuilder::build(&full),
        );

        // Pure `git reset --hard HEAD~2` (no new commits, just moving ref back to 2).
        let reset = join_commits_indexed(&saved, &index_map(&saved), &refs(&[4]), &[], &refs(&[2]))
            .unwrap();
        assert_eq!(reset.added, 0);
        assert_eq!(reset.removed, 2);
        assert_eq!(
            reset.commits.iter().map(|c| c.oid).collect::<Vec<_>>(),
            vec![oid(2), oid(1)]
        );
    }

    #[test]
    fn delete_branch_and_new_tag_on_existing_commit() {
        // main=3 -> 2 -> 1, feat=5 -> 4 -> 2
        let saved = vec![
            node(5, &[4], 500),
            node(4, &[2], 400),
            node(3, &[2], 300),
            node(2, &[1], 200),
            node(1, &[], 100),
        ];

        // Delete feat (5): removes 5 and 4, keeps 3, 2, 1.
        let del =
            join_commits_indexed(&saved, &index_map(&saved), &refs(&[5, 3]), &[], &refs(&[3]))
                .unwrap();
        assert_eq!(del.added, 0);
        assert_eq!(del.removed, 2);
        assert_eq!(
            del.commits.iter().map(|c| c.oid).collect::<Vec<_>>(),
            vec![oid(3), oid(2), oid(1)]
        );

        // New tag on existing commit 2: no commits added or removed.
        let tagged = join_commits_indexed(
            &saved,
            &index_map(&saved),
            &refs(&[5, 3]),
            &[],
            &refs(&[5, 3, 2]),
        )
        .unwrap();
        assert_eq!(tagged.added, 0);
        assert_eq!(tagged.removed, 0);
        assert_eq!(tagged.commits, saved);
    }

    #[test]
    fn orphan_branch_or_unloaded_boundary_falls_back_cleanly() {
        let saved = vec![node(3, &[2], 300), node(2, &[1], 200)]; // truncated: parent 1 is unloaded
        // New orphan root commit 99 (no parents) -> cannot link into saved_log -> NotEnoughData.
        let orphan = join_commits_indexed(
            &saved,
            &index_map(&saved),
            &refs(&[3]),
            &[node(99, &[], 990)],
            &refs(&[3, 99]),
        );
        assert_eq!(orphan.unwrap_err(), JoinError::NotEnoughData);

        // New commit 10 whose parent 0 is outside saved_log -> NotEnoughData.
        let missing_parent = join_commits_indexed(
            &saved,
            &index_map(&saved),
            &refs(&[3]),
            &[node(10, &[0], 400)],
            &refs(&[3, 10]),
        );
        assert_eq!(missing_parent.unwrap_err(), JoinError::NotEnoughData);

        // Rewrite past the unloaded boundary (delete ref 3 when 1 is unloaded) -> IllegalState.
        let past_boundary =
            join_commits_indexed(&saved, &index_map(&saved), &refs(&[3]), &[], &refs(&[]));
        assert_eq!(past_boundary.unwrap_err(), JoinError::IllegalState);
    }

    #[test]
    fn graft_or_reshallow_parent_drop_past_index_zero_prunes_unreachable_ancestors() {
        // Saved log: 3 -> 2 -> 1 (single ref at 3).
        let saved = vec![node(3, &[2], 300), node(2, &[1], 200), node(1, &[], 100)];
        // Refs unchanged ({3}), but commit 2 (at saved_log[1], idx > 0) dropped parent 1.
        let first_block = vec![node(3, &[2], 300), node(2, &[], 200)];
        let res = join_commits_indexed(
            &saved,
            &index_map(&saved),
            &refs(&[3]),
            &first_block,
            &refs(&[3]),
        )
        .unwrap();
        assert_eq!(res.removed, 1);
        assert_eq!(
            res.commits.iter().map(|c| c.oid).collect::<Vec<_>>(),
            vec![oid(3), oid(2)]
        );
        assert!(res.commits[1].parents.is_empty());

        // Also verify the unindexed `join_commits` path.
        let res_unindexed = join_commits(&saved, &refs(&[3]), &first_block, &refs(&[3])).unwrap();
        assert_eq!(res_unindexed.removed, 1);
        assert_eq!(res_unindexed.commits, res.commits);
    }
}
