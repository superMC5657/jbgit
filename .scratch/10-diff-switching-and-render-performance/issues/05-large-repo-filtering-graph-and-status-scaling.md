# 05: 大仓库日志过滤、`git log` 路径筛选、可达分支异步化与状态扫描扩展性

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Blocked by: 03, 04
Milestone: M7 — 性能架构治理与交互零卡顿优化 (P2)

- **关联文件**：
  - `crates/jbgit-core/src/filter.rs`
  - `crates/jbgit-core/src/details.rs`
  - `crates/jbgit-engine/src/details.rs`
  - `crates/jbgit-engine/src/pool.rs`
  - `crates/jbgit-engine/src/supervisor.rs`
  - `crates/jbgit-tui/src/graph/state.rs`
  - `crates/jbgit-tui/src/log_view.rs`
  - `crates/jbgit-tui/src/app.rs`
  - `crates/jbgit-tui/src/config/mod.rs`

---

## 1. 背景与目标

在拥有数万提交（上限 `50,000`）或数万工作区文件的中型/大型仓库（尤其是 Windows 平台）中，以下 4 处扩展性瓶颈会从后台与主线程同时拖慢整体体验：
1. **Log 文本过滤主线程无防抖全量重算**：`app.rs:1549-1557` 每输入/删除一个字符都在 UI 主线程触发 `on_filter_changed()` → `ensure_cache()`（`graph/state.rs:156-230`），对 5 万提交同步扫描文本/正则（其中 `UserFilter::matches` 每提交分配 3 次 `to_lowercase()`）、重建 `VisibleGraph` 并采样 20,000 行算轨道宽；此外 `graph/state.rs:188-213` 在快照加载且存在过滤条件时会先建一遍全量 `VisibleGraph` 再立即重算过滤版；`build_graph_color_map`（`graph/state.rs:90-108`）使用 `Vec::contains` 做 `O(N * unique_colors)` 去重。
2. **包含分支全图 BFS 同步阻塞 UI**：`app.rs:2078-2086` 在选中提交稳定后，于主线程直接运行 `ReachableNodes::containing_branches` 遍历整张 5 万节点的 `PermanentLinearGraph`。
3. **路径过滤未利用 Git Changed-Path Bloom Filter**：`load_path_matches_sync`（`jbgit-engine/src/details.rs:682-726`）逐个对最多 5 万提交执行 `repo.find_commit` + `repo.diff_tree_to_tree`，耗时远高于利用 commit-graph 与 Bloom filter 的原生 `git log`。
4. **Windows 大工作区状态与仓库快照扫描过重**：`load_stage_status_sync`（`pool.rs:655-662`）默认同时启用 `renames_index_to_workdir(true)` + `recurse_untracked_dirs(true)`；`collect_refs`（`pool.rs:532-539`）对全部本地分支串行计算 `graph_ahead_behind`；`collect_commits`（`pool.rs:583-635`）启动与每次分支/引用刷新都一次性加载全部 5 万提交完整 `commit.body()`。

本 Issue 面向大仓库进行系统性降复杂度与异步化扩展改造。

---

## 2. 核心任务清单

- [ ] **Log 文本过滤防抖、去重构建与低分配优化（`app.rs`, `log_view.rs`, `graph/state.rs`, `filter.rs`）**：
  - 在 `FilterBarState` / `AppState` 为 `FilterTextInput`、`FilterTextBackspace` 等文本编辑动作接入 `120 ms` 防抖窗口（并入 `AppState::next_deadline()`）：敲击时立即更新输入框光标与渲染，停止敲击 `120 ms` 后再重建过滤结果（按 `Enter` 完成编辑时立即刷新不等待）。
  - 修复 `LogGraphState::ensure_cache`（`graph/state.rs:188-213`）：当 `snapshot_stale` 时仅构建 `PermanentGraph` 与调色板一次，紧接着统一按最新 `filter.compute_matches(...)` 一次性构建目标 `VisibleGraph`，彻底消除双次构图浪费。
  - 将 `build_graph_color_map`（`graph/state.rs:90-108`）的去重查找由 `Vec::contains` 改为 `FxHashSet`。
  - 在 `UserFilter`（`crates/jbgit-core/src/filter.rs:235-244`）预先归一化/小写化用户查询 token，并使用零分配大小写不敏感子串检查（ASCII 快路径 + `to_lowercase()` 仅在必要时复用），消除每提交 3 次堆分配。
  - 将 5 万提交规模下的 `VisibleGraph` / `compute_matches` 重算探索性或确定性地移交后台工作任务（由 `graph_generation` / `epoch` 管控丢弃过期结果），确保即使用户持续操作也不卡死主渲染循环。
- [ ] **异步化并缓存 `containing_branches`（`app.rs`, `pool.rs` / `supervisor.rs`）**：
  - 将 `ReachableNodes::containing_branches(&cached.permanent.linear, node, &s.refs)` 从主线程 `reconcile_log_requests`（`app.rs:2078-2086`）迁至后台工作任务（携带 `PermanentLinearGraph` 共享 `Arc` 引用与 `EpochToken`），或者附带小型 `LruCache<Oid, Vec<String>>` 命中缓存；在详情未算完前先行显示提交基本元数据与变更文件树，算完后静默回填分支/标签归属。
- [ ] **`load_path_matches_sync` 接入原生 `git log --simplify-merges`（`crates/jbgit-engine/src/details.rs`）**：
  - 优先采用只读原生命令：
    ```text
    git -c core.quotepath=false log --format=%H --simplify-merges --max-count=<snapshot.commits.len()> -- <paths...>
    ```
    （强制 `GIT_OPTIONAL_LOCKS=0`、`GIT_TERMINAL_PROMPT=0`，复用 `prepare_pathspec_args` 参数护栏与 `--all` / 覆盖当前快照分支范围对齐，并在读写过程校验 `token.is_stale()` 及时 kill 过期子进程）。
  - 当系统不存在 `git` CLI 时优雅回退原有 `git2::diff_tree_to_tree` 实现；对齐原有 merge `--simplify-merges` TREESAME 语义并通过跨 `path_filter` 集成测试校验两种路径结果交集完全一致。
- [ ] **Windows 大工作区状态与仓库快照扫描调优（`jbgit-engine/src/pool.rs`, `config`）**：
  - 优化 `load_stage_status_sync`：评估并实施可配置的 `renames_index_to_workdir` / `recurse_untracked_dirs` 策略或大工作区阈值保护（例如未跟踪文件数/目录层级超限时保留 `renames_head_to_index`，把昂贵的 `renames_index_to_workdir` 做成可配或按阈值关闭选项，确保 Windows 工作区扫描不拖垮自动刷新）。
  - 优化 `collect_refs`（`pool.rs:532-539`）：避免每次刷新对仓库内成百上千个历史本地分支串行跑全量 `graph_ahead_behind`——优先计算当前 `HEAD` 及有限活跃跟踪分支，其余分支可异步延迟补算或按分支列表可见状态按需计算。
  - 评估并优化 `collect_commits`（`pool.rs:583-635`）：仅在真正有全文消息筛选或明确需求时按需补充完整 `commit.body()`，或将 `body` 延迟至需要时再加载以大幅降低 5 万提交仓库冷启动峰值内存与 IO 耗时。
- [ ] **补齐大仓库基准与一致性测试**：
  - `load_path_matches_sync` 对比测试：构造含多分支与合并提交（TREESAME 合并）临时仓库，断言 `git log --simplify-merges` 路径结果与原 `git2` 逻辑完全一致。
  - `LogGraphState` 单次构图测试：验证带激活过滤器的快照首次构建不再构造全量无过滤 `VisibleGraph`。
  - 防抖与异步包含分支测试：连续快速输入 5 个字符仅在 120 ms 防抖后触发 1 次过滤重算；切换大提交时 `containing_branches` 不阻塞主线程即可响应下次按键。

---

## 3. 验收标准 (Acceptance Criteria)

- [ ] `cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
- [ ] 在数万提交测试仓库中，在 Log 过滤框连续打字输入时键盘回显与光标响应实时顺滑（零逐字符全图阻塞），快照刷新+过滤仅构建一次 `VisibleGraph`。
- [ ] 在含大量提交历史的仓库启用 `Paths` 路径筛选时，优先走 `git log --simplify-merges`（借助 commit-graph / Bloom filter），取消/切换路径时旧子进程可被中止。
- [ ] Windows 大工作区下 `load_stage_status_sync`、多本地分支 `collect_refs` 与提交快照加载均有清晰的策略控制或显著耗时下降，交互切换 Diff 全程无阻塞感。

---

## Comments

- 2026-11-21 (agent): 在完成 P0（Issue 01-02）与 P1（Issue 03-04）基础上，汇总大仓库扩展性与平台特定状态扫描热点形成本 P2 专项工单。
