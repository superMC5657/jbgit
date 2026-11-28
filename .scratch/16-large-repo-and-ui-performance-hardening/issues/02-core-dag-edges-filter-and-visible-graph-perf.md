# 02: `recommended_track_width` 单趟线性化

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M13 — 大仓库、渲染热路径与引擎并发性能二期治理
Priority: P0

- **关联文件**：
  - `crates/jbgit-core/src/graph/print_elements.rs`（`recommended_track_width`、`passing_edge_counts`）
  - `crates/jbgit-core/src/graph/edges_in_row.rs`（`#[cfg(test)] cached_block_count`）
  - `crates/jbgit-core/benches/graph_layout.rs`

---

## 1. 根因定位

在 `crates/jbgit-core/src/graph/print_elements.rs` 中，当 `visible_count >= 40_000` 时，`recommended_track_width` 以 `step = (n / 20_000).max(1) >= 2` 跳跃采样并调用 `40,000` 次 `edges_in_row`，横跨数千个 block 彻底击穿容量仅 `20` 的 `above_anchor_cache`，并把首屏缓存全部挤出。10 万提交 DAG 实测耗时 ≈ 190 ms（每次构图 / 筛选 / 折叠都会执行）。

---

## 2. 任务清单

- [x] **重写 `recommended_track_width` 为单趟 `O(V + E)` 差分扫描**：
  - 在 `crates/jbgit-core/src/graph/print_elements.rs` 中由私有 `passing_edge_counts` 按 `edge_is_visible_in_gap` 语义算出每条去重后边的占位行区间，用差分数组单趟 `O(V + E)` 求出每行穿行边数，再按原 `step` 采样与 `avg + K * std_dev` 公式求值，**完全不读写 `EdgesInRowCache`**。
- [x] **补齐 `graph_layout` 相关 Criterion 基准**：
  - 新增 `visible_graph_build_*`、`recommended_track_width_100k`、`print_elements_viewport_60_rows_100k` 基准。

---

## 3. 验收标准与回归测试

- `print_elements.rs` (`recommended_track_width_matches_reference_and_leaves_cache_untouched`)：在随机多分支 DAG（2,500 与 41,000 提交 × `show_long_edges` 开/关 × 过滤/不过滤）上与旧版 `count_row_slots` 参考实现宽度精确一致，且调用后 `EdgesInRowCache` 缓存块数与命中数均为 `0`。
- 基准 `recommended_track_width_100k`：189.64 ms → 348.27 µs。

## Comments

- **2026-11-28（Commit `a4b0cc6`）**：完成 `recommended_track_width` 单趟差分扫描及 Criterion 基准，其余微优化项经实测收益低，已从本工单裁剪。
