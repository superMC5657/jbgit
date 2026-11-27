# 02: DAG 边查询去重、`recommended_track_width` 线性化、`VisibleGraph` 分配瘦身与零分配 Filter/OID 匹配

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M13 — 大仓库、渲染热路径与引擎并发性能二期治理
Priority: P0

- **关联文件**：
  - `crates/jbgit-core/src/graph/edges_in_row.rs`（`EdgesInRowCache` `:23-36`，`edges_in_row` `:117-136`，`edges_from_above` `:140-169`，`edges_from_below` `:173-204`，`one_up_step` `:221-236`）
  - `crates/jbgit-core/src/graph/print_elements.rs`（`print_elements` `:258-415`，`recommended_track_width` `:419-459`，`count_row_slots` `:473-486`，`build_row_slots` `:488-514`，`slot_of_edge` `:518-528`）
  - `crates/jbgit-core/src/graph/visible.rs`（`build_visible_linear` `:259-567`）
  - `crates/jbgit-core/src/graph/permanent.rs`（`PermanentLinearGraphBuilder::build` `:145-185`，`sort_head_nodes` `:376-396`，`compute_node_colors` `:674-690`）
  - `crates/jbgit-core/src/graph/linear_bek.rs`（`apply_linear_bek` `:125-233`，`MergeFragment` `:68-91`，`find_fragment` `:294-379`，`calculate_magic_set` `:381-403`）
  - `crates/jbgit-core/src/oid.rs`（`short` `:31-34`，`matches_hex_prefix` `:38-47`）
  - `crates/jbgit-core/src/filter.rs`（`TextFilter::compile` `:50-79`，`compute_matches_with_favorites` `:840-868`，`glob_match` `:874-899`）
  - `crates/jbgit-core/src/branch_tree.rs`（`fuzzy_match` `:618-636`，`natural_cmp` `:640-675`）
  - `crates/jbgit-core/src/refs.rs`（`natural_cmp` `:339-379`）
  - `crates/jbgit-core/src/repo.rs`（`walk_ancestors` `:510-513`，`scan_cleanup_branches` `:584-646`，`has_merge_commits_in_range` `:651-658`，`is_upstream_gone` `:662-670`）
  - `crates/jbgit-tui/src/log_view/filter_bar.rs`（`:499`）、`crates/jbgit-tui/src/log_view/file_history.rs`（`:226-228`）

---

## 1. 根因定位

### 1.1 `EdgesInRowGenerator::edges_from_below` 100% 数学冗余且 `ultra_long_edges` 线性全扫
- 在 `crates/jbgit-core/src/graph/edges_in_row.rs:124-236` 中，`one_down_step`（`:214`）与 `one_up_step`（`:232`）均只收录跨度 `(e.down - e.up) <= WALK_SIZE (1000)` 的边（更长边由 `ultra_long_edges` 单独处理）。
- 对于任意满足 `e.up <= row < e.down` 且 `e.down - e.up <= WALK_SIZE` 的边，必有 `e.up >= row + 1 - WALK_SIZE > up_anchor.saturating_sub(WALK_SIZE)`，因此 `edges_from_above(row)` **100% 包含且仅包含** `edges_from_below(row)` 的全部边！
- 保留 `edges_from_below` 直接使锚点缓存内存、1,000 行未命中扫描量与每次命中时的 `FxHashSet::clone()` 全部翻倍。同时 `ultra_long_edges`（`:127-131`）每次查询都被线性遍历；每次 `edges_in_row` 都把 `FxHashSet` 收集为 `Vec` 并排序，即使调用方不依赖顺序。

### 1.2 `recommended_track_width` 在 ≥ 4 万提交仓库打穿块缓存并驱逐首屏行
- 在 `crates/jbgit-core/src/graph/print_elements.rs:419-459` 中，当 `visible_count >= 40_000` 时，`step = (n / 20_000).max(1) >= 2`，导致无法复用 `prev_vis`，对每个采样点执行两次 `visible_edges_in_gap`——共 **40,000 次** `edges_in_row` 调用，横跨数千个 block 彻底击穿容量仅 `20` 的 `above_anchor_cache`，并把刚刚或即将渲染的第 `0..40` 行缓存全部挤出。
- 在 `print_elements` 内部（`:258-528`），单行查询 4 个间隙、60 行视口同一间隙被查 ~4 遍、同一行槽位布局被建 3 遍；且 `count_row_slots`/`build_row_slots`/`slot_of_edge` 存在基于 `Vec::contains` / `.position()` 的 `O(W²)` 循环（在开启 `show_long_edges` 的多合并仓库中 `W` 可达数百）。

### 1.3 `VisibleGraph` / `PermanentGraph` / `LinearBek` 的大量小堆 `Vec` 与重复哈希图构造
- `crates/jbgit-core/src/graph/visible.rs:330-516`：`build_visible_linear` 在 10万–20万 commits 上预先分配 `perm_edges`（20 万个 `Vec`）和 `nearest_down`（20 万个 `Vec`），哪怕走无过滤 `all_visible` 快路径也先分配 `perm_edges`；在过滤压缩阶段对约 90% 的单父隐藏提交仍然克隆构造新 `Vec` 并重复 `sort_unstable() + dedup()`；最后 CSR 装配再临时分配 10万–20万个 `row_edges: Vec<GraphEdge>`。
- `crates/jbgit-core/src/graph/permanent.rs:145-185`：在 `is_valid_topo == true` 的常规快路径下，`input_oid_map` 与 `oid_to_node` 构建了两份完全等价的 10万–20万条 `FxHashMap`；`RawCommitNode` 强迫调用方深拷贝所有 `parents: Vec<Oid>`。
- `crates/jbgit-core/src/graph/linear_bek.rs:135-167`：`apply_linear_bek` 预先分配 `m` 个 `down_adj` 向量，并在逆序扫描中对 90% 只有 1 个父节点的行仍分配并排序 `down_nodes`，且内部使用 SipHash `HashMap`/`HashSet`。

### 1.4 `Oid::matches_hex_prefix` 堆分配、`LogFilterSpec` 谓词逆序与 `fuzzy_match`/`natural_cmp`/`repo.rs` 热开销
- `crates/jbgit-core/src/oid.rs:38-47`：`matches_hex_prefix` 每次都先调用 `self.to_string()`（20 次 `write!("{byte:02x}")` 分配 40 字节 `String`）。搜索任意 4–40 位十六进制词（包含 `"dead"`、`"face"`、`"cafe"`、`"feed"` 等常见单词或 SHA）时，每敲一字对 20 万提交做 **20万–60万次堆分配**。`Oid::short()`（`:31-34`）同理两次堆分配。
- `crates/jbgit-core/src/filter.rs:855-864`：`compute_matches_with_favorites` 把最重的 `text.matches_commit`（正则匹配 + hex 字符串分配）放在 `date.matches`、`path_matches.contains`、`range.contains` 等 `O(1)`/`O(log K)` 检查**之前**。`glob_match`（`:874-899`）每次分配两个 `Vec<char>` 而无字面量快路径。
- `crates/jbgit-core/src/branch_tree.rs:618-675`（及 `refs.rs:339-379`）：`fuzzy_match` 每次调用都从零构造 `nucleo_matcher::Matcher` 与 `Pattern`、`Vec<char>`（被 `filter_bar.rs:499` 与 `file_history.rs:226` 在热循环中逐项调用）；`natural_cmp` 在每次数字段比较都分配两个 `String`，在 1 万个带版本号 tag 排序时产生数十万次小分配。
- `crates/jbgit-core/src/repo.rs:510-670`：`walk_ancestors` 每次调用都把全量 `commits` 重建为 SipHash `HashMap<Oid, &CommitSummary>`，`has_merge_commits_in_range` 单次调用连建 **3 遍**；`scan_cleanup_branches` 对每分支调用线性 `is_upstream_gone` 形成 `O(R²)`。

---

## 2. 任务清单

- [ ] **精简 `EdgesInRowGenerator` 并索引化 `ultra_long_edges`**：
  - 在 `crates/jbgit-core/src/graph/edges_in_row.rs` 中移除冗余的 `edges_from_below`、`below_anchor_cache` 与 `one_up_step`，仅保留 `edges_from_above` 与单倍锚点缓存；
  - 对 `ultra_long_edges` 按 `up` 排序或建立轻量区间索引（例如按 `up` 排序后使用 `partition_point` 截取 `up <= row` 前缀，或按块分桶），避免每次查询全量线性扫描；
  - 提供按需不排序/复用缓冲的内部接口供只计槽位或随后按 `layout_index` 重排的调用方使用。
- [ ] **重写 `recommended_track_width` 为单趟 `O(V + E)` 扫描并消除 `print_elements` 冗余工作**：
  - 在 `crates/jbgit-core/src/graph/print_elements.rs` 中将 `recommended_track_width` 改为单趟线性维护当前活跃可见边集合（或复用增量边界统计），既将复杂度降至 `O(V + E)`，又**完全不触碰/不污染 `EdgesInRowCache`**；
  - 在 `count_row_slots`、`build_row_slots` 与 `slot_of_edge` 中消除 `O(W²)` 的 `Vec::contains` / `.position()` 线性查找（如利用集合/序号映射或预排序去重）；
  - 为 `print_elements` 增加视口连续行批量/滑动窗口复用辅助（或缓存相邻间隙结果），减少每帧 4× 重复间隙查询与 3× 槽位排序。
- [ ] **瘦身 `VisibleGraph`、`PermanentGraph` 与 `LinearBek` 堆分配**：
  - `crates/jbgit-core/src/graph/visible.rs`：在 `all_visible` 快路径下跳过 `perm_edges` 分配，直接基于 `PermanentLinearGraph` 扁平 CSR 构建；在过滤压缩路径中，单父隐藏提交直接以引用/共享索引透传而不克隆并冗余 `sort_unstable + dedup`；CSR 构建直接向 `down_edges_data` 写入而非每行建临时 `row_edges` `Vec`；
  - `crates/jbgit-core/src/graph/permanent.rs`：`is_valid_topo == true` 快路径直接复用 `input_oid_map` 为 `oid_to_node`；将 `PermanentLinearGraphBuilder` 输入抽象为轻量借用 trait（复用 `JoinCommit` 或新 trait），消除调用方（`graph/state.rs:96`）为构图而克隆 `parents: Vec<Oid>`；合并 `sort_head_nodes` 与 `compute_node_colors` 中的重复最优 ref 查找；
  - `crates/jbgit-core/src/graph/linear_bek.rs`：单父节点行直接 `continue` 跳过 `down_nodes` 分配与排序；将内部 `HashMap`/`HashSet` 替换为 `FxHashMap`/`FxHashSet` 并按需延迟分配 `BinaryHeap`。
- [ ] **零分配 `Oid`、`Filter` 谓词重排、`FuzzyMatcher` 复用与 `natural_cmp`/`repo.rs` 优化**：
  - `crates/jbgit-core/src/oid.rs`：重写 `matches_hex_prefix` 直接按字节对比 nibble（零堆分配、首字节即淘汰 ~99.6% 不匹配项），`Oid::short()` 直接格式化 7 个字符至定长栈缓冲再转 `String`；
  - `crates/jbgit-core/src/filter.rs`：重排 `compute_matches_with_favorites` 谓词顺序，将 `O(1)`/`O(log K)` 条件（`no_merges`、`date`、`path_matches`、`range`）移至 `user` 与 `text`（正则表达式）之前；在 `glob_match` 为不含 `*`/`?` 的模式提供零分配精确比较快路径；
  - `crates/jbgit-core/src/branch_tree.rs` & `refs.rs`：提取可复用的 `FuzzyMatcher` 结构体（封装 `Matcher` + 复用 `buf: Vec<char>` + 预解析 `Pattern`），并在 `jbgit-tui` 的 `filter_bar.rs:499` 与 `file_history.rs:226` 中跨元素复用；消除 `natural_cmp` 中的数字 `String` 分配并合并 `branch_tree.rs` 与 `refs.rs` 的重复实现；
  - `crates/jbgit-core/src/repo.rs`：抽出一份复用的 `FxHashMap<Oid, usize>` / 索引映射供 `walk_ancestors` / `has_merge_commits_in_range` 共用（避免连续重建 2–3 次全量 SipHash map），将 `scan_cleanup_branches` 的 `is_upstream_gone` 查找转为 `FxHashSet` `O(1)` 查询。

---

## 3. 验收标准与回归测试

- **等价性与复杂度验证单测**：
  - `edges_in_row.rs`：构造含跨度 `1..=5000` 混合边（覆盖 `WALK_SIZE` 边界与 `ultra_long_edges`）的随机/多分支 DAG，用原双侧参考算法比对去除 `edges_from_below` 后的全行输出 **100% 逐项一致**。
  - `print_elements.rs`：验证新版 `recommended_track_width` 与旧版在 1万–5万节点随机 DAG 上结果精确一致（或误差在可控浮点舍入内），且调用后 `EdgesInRowCache` 命中/驱逐不被污染。
  - `oid.rs` & `filter.rs`：覆盖大小写混合 hex 前缀（长度 0、1、7、40、41 及非法字符）等价性测试，以及复合筛选（日期 + 路径 + 正则文本）结果一致性。
  - `branch_tree.rs` & `refs.rs`：覆盖前导零（`"v01"` vs `"v1"`）、多段数字与大小写混合的 `natural_cmp` 回归测试，以及可复用 `FuzzyMatcher` 与原 `fuzzy_match` 结果一致性。
- **全量检查**：
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace --locked`

## Comments

- **2026-11-28**：基于 `jbgit-core` 性能审计创建本 P0 工单，彻底消除 DAG 边计算 2× 冗余、40k 提交缓存击穿、`VisibleGraph` 40万+ 小 `Vec` 分配与筛选/哈希前缀热路径堆分配。
