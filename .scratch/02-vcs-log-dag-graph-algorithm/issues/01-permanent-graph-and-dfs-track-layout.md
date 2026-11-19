# 01: 实现 `PermanentLinearGraph`、`GitBranchLayoutComparator` 与 DFS 连续轨道布局算法

Status: ready-for-agent
Blocked by: none
Spec: [../spec.md](../spec.md)
Milestone: M2 — 非交叉 DAG 图与 Pill 徽标

- **参考源码**：
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/permanent/PermanentLinearGraphBuilder.java`
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/permanent/DuplicateParentFixer.java`
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/permanent/GraphLayoutBuilder.kt`
  - `reference/plugins/git4idea/backend/src/log/GitRefManager.kt` (`GitBranchLayoutComparator`)
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/graph/HeadCommitsComparator.java`

---

## 1. 背景与目标

在 `crates/jbgit-core/src/graph/` 中高保真移植 IDEA `vcs-log` 的静态永久图构建器与主干优先 DFS 连续轨道布局器（`GraphLayoutBuilder`），确保主干（`origin/main` / `main`）永远占据 `layout_index = 1` 的最左连续直轨，且任意深度的线性历史不会引发递归栈溢出。

---

## 2. 核心实现任务清单

- [x] **紧凑 CSR 永久图构建器 (`PermanentLinearGraphBuilder`)**：
  - 输入按拓扑逆序排列的提交切片 `&[RawCommitNode]`（含 `oid`、`parents: SmallVec<[Oid; 2]>`、`timestamp`）。
  - 实现对标 `DuplicateParentFixer.java` 的异常提交去重（剔除罕见畸形 Git 提交中重复出现的同一父 Oid）。
  - 处理浅克隆（Shallow Clone）或增量截断场景下未加载的父提交：为其分配负值边界 ID `-(unloaded_idx + 2)`，保留向下悬空边信息。
  - 生成零指针追逐开销的 CSR 邻接表（`down_offsets`、`down_targets`、`up_offsets`、`up_targets`）。
- [x] **头节点收集与 `GitBranchLayoutComparator` 排序**：
  - 合并所有分支/标签引用指向的节点集合与图拓扑入度为 0 的顶端节点（`!has_up_nodes`）。
  - 严格按 `ORIGIN_MASTER` (`origin/main` | `origin/master`) < `REMOTE_BRANCH` < `MASTER` (`main` | `master`) < `LOCAL_BRANCH` < `TAG` < `CURRENT_BRANCH` < `HEAD` < `OTHER` < `无引用悬空头` 排序，同级按自然引用名排序。
- [x] **显式栈迭代 DFS 轨道分配 (`GraphLayoutBuilder::build`)**：
  - 严格遵循 `GraphLayoutBuilder.kt` 第 68-104 行语义，用迭代显式栈（零递归）沿 `down_nodes`（保证 `parent[0]` First-Parent 最先访问！）赋予单调递增 `layout_index: u32`（从 `1` 起步），并记录每个节点的 `head_of_node: PermNodeId`。
- [x] **可达性快速查询工具 (`ReachableNodes`)**：
  - 实现基于 `FixedBitSet` 的 `contained_in_branch(head_nodes)`（供中屏 `CurrentBranchHighlighter` 毫秒级着色）与 `containing_branches(node_id)`（供右屏提交详情展示所属分支）。

---

## 3. 边界情况与不变量

- **多根孤儿分支仓库（如同时包含 `gh-pages` 或 `--orphan` 分支，存在多个根提交）**：DFS 应对每个连通分量正常走完并递增 `layout_index`，不得遗漏任何 `layout_index == 0` 的未访问节点。
- **章鱼合并（Octopus Merge，父提交数 ≥ 3）**：`parent[0]` 继承当前 `layout_index`，`parent[1]`、`parent[2]`... 在回溯时依次获得递增的新 `layout_index`。
- **超深线性历史（200,000 个连续单亲提交）**：堆上显式栈运行平稳，绝不栈溢出，构建耗时 < 50ms。

---

## 4. 验收标准 (Acceptance Criteria)

- [x] 属性测试（`proptest`）：随机生成 1,000 组任意合法 DAG 拓扑，断言输出中所有节点 `layout_index >= 1`，且沿任意 `head` 的空闲 `first-parent` 路径 `layout_index` 恒定不变。
- [x] 单元测试验证：当仓库同时存在 `origin/main`、`main`、`feature/a`（当前 HEAD 指向 `feature/a`）时，`origin/main` 的主线恒获 `layout_index == 1`，切换 `HEAD` 不会导致 `main` 让出第 1 轨道。
- [x] `criterion` 基准测试：100,000 节点永久图构建 + DFS 布局总耗时 ≤ 80ms（Release 模式）。

---

## Comments

### 2026-11-18 — implemented

- 代码实现位于 `crates/jbgit-core/src/graph/permanent.rs`。
- `PermanentLinearGraphBuilder` 实现了紧凑 CSR 双向邻接表与对标 IDEA 的去重父提交和未加载浅克隆边界处理。
- `GitBranchLayoutComparator` 实现了完整的优先级与分支排序，保证 `origin/main` / `main` 恒占最左轨道。
- 显式堆栈 DFS 遍历避免递归，实测 200,000 连续线性提交平稳通过无栈溢出。
- `ReachableNodes` 封装基于 `FixedBitSet` 的分支可达性查询。
- 基准测试 `crates/jbgit-core/benches/graph_layout.rs` 支持 release 模式压测。
