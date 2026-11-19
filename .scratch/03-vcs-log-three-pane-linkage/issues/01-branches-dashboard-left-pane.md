# 01: 实现左侧可折叠 Branches Dashboard 树面板（目录分组、`↑↓` 同步状态、星标收藏与筛选联动）

Status: ready-for-agent
Blocked by: 00-architecture-and-git-engine/issues/01-workspace-tui-runtime-and-hit-test.md, 00-architecture-and-git-engine/issues/02-hybrid-git-engine-and-watcher.md, 01-visual-system-darcula-and-widgets/issues/01-darcula-theme-14-color-palette-and-icons.md
Spec: [../spec.md](../spec.md)
Milestone: M3 — Git Log 三屏联动工作台

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/ui/branch/dashboard/BranchesInGitLogUiFactoryProvider.kt`
  - `reference/plugins/git4idea/backend/src/ui/branch/dashboard/BranchesDashboardTreeModel.kt`
  - `reference/plugins/git4idea/backend/src/ui/branch/dashboard/BranchTreeNodeComparator.kt`

---

## 1. 背景与目标

构建 `[2 Log]` 三屏工作台的左屏 **Branches Dashboard** 导航树组件，支持按 `/` 层级目录展示本地分支、远程分支与标签，呈现星标收藏 `★` 与远程同步偏离指标 `↑N ↓M`，并打通向中屏提交图的 **分支过滤（Filter）** 与 **提交定位（Navigate）** 双向联动。

---

## 2. 核心实现任务清单

- [ ] **层级分支树模型构建器 (`BranchesTreeModel`)**：
  - 构建顶层分类节点：`HEAD (<current_branch>)`、`Local`、`Remote`（按远端名如 `origin` 二级分组）、`Tags`（可通过设置开关显示/隐藏）。
  - 支持 **`Group by Directory`** 开关（默认开启）：将带 `/` 的分支名自动构建为可折叠文件夹树，并对单子目录链支持紧凑展示或递归展开。
  - 实现对标 `BranchTreeNodeComparator` 的节点排序：星标收藏分支 `★` 置顶 -> `main/master` -> 文件夹节点 -> 普通分支按自然序排列。
  - 持久化星标收藏列表（保存在仓库级或用户配置目录）与节点展开状态（对标 `git.branches.panel.persist.tree.state = true`）。
- [ ] **同步指示徽标与内联速查**：
  - 在本地分支行右侧对齐渲染 `↑N`（待推送，绿色）与 `↓M`（待拉取，蓝色）。
  - 焦点位于左屏时直接键入字符激活顶部内联搜索栏（`nucleo` 模糊过滤），自动展开包含匹配叶子的所有祖先目录，`Esc` 清除搜索恢复原折叠态。
- [ ] **折叠侧边条与中屏联动协议**：
  - 支持 `Alt+B` 或点击面板右上角折叠按钮 `[«]` 收起左屏，收起后在最左缘保留 2 列宽可点击展开条 `[»]`（对标 `ExpandStripeButton`）。
  - 双击节点或按 `Enter`：收集所选节点（若为目录节点则递归收集其下全部子分支！）更新中屏 `BranchFilter`（支持 `Ctrl+Click` / 多选并集过滤）。
  - 按 `J` 或点击定位按钮 `[⌖]`（或开启选中即定位配置）：不改动过滤器，直接通知中屏跳转并居中高亮目标分支尖端提交。

---

## 3. 验收标准 (Acceptance Criteria)

- [ ] 包含 `feature/auth/login`、`feature/auth/oauth`、`bugfix/ui` 的仓库能正确渲染可逐级展开/折叠的目录树，在 `feature` 文件夹上按 `Enter` 能同时过滤出两个子分支的全部提交。
- [ ] 本地领先远程 2 个提交、落后 1 个提交时，树节点准确渲染 `↑2 ↓1`。
- [ ] 终端总宽度缩窄至 `< 110` 列时左屏自动优雅折叠为侧边窄条，拉宽后自动恢复。

---

## Comments

- 2026-11-18 (agent): Implemented. Model in `crates/jbgit-core/src/branch_tree.rs` (HEAD/Local/Remote-by-remote/Tags, group-by-directory with compacted single-child folders, ★ favorites with main/master defaults, natural-order comparator, `nucleo-matcher` fuzzy speed search). UI in `crates/jbgit-tui/src/views/log_branches.rs` + `log_view.rs`: Enter / double-click filters (folders recurse, Ctrl+Click union), `[Clear Branch Filter]` chip, `J` / `[⌖]` navigates without touching filters, Space toggles ★, `Alt+B` / `[«]` / `»` stripe, auto-collapse below 110 cols. Favorites, expansion and view options persist to `<common-dir>/jbgit-branches-panel`.
