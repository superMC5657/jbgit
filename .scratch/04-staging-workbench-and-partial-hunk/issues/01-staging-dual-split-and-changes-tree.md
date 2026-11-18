# 01: 实现暂存工作台双重分屏与 `GitStageTree`（五态分组、双出现机制与快捷暂存）

Status: ready-for-agent
Blocked by: 00-architecture-and-git-engine/issues/01-workspace-tui-runtime-and-hit-test.md, 00-architecture-and-git-engine/issues/02-hybrid-git-engine-and-watcher.md, 01-visual-system-darcula-and-widgets/issues/01-darcula-theme-14-color-palette-and-icons.md
Spec: [../spec.md](../spec.md)
Milestone: M4 — 变更暂存工作台与局部暂存

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/index/ui/GitStagePanel.kt`
  - `reference/plugins/git4idea/backend/src/index/ui/GitStageTree.kt`

---

## 1. 背景与目标

实现 `[1 Commit]` 变更暂存工作台的整体双重分屏骨架（左栏上下分屏：`GitStageTree` + `CommitPanel`；右栏：`StageDiffWorkbench`）以及核心状态树 `GitStageTree`，精准还原 IDEA Git Staging Area 的五态分类、部分暂存文件双出现机制（同时位于 `Staged` 与 `Unstaged`）及鼠标悬停/空格一键暂存体验。

---

## 2. 核心实现任务清单

- [ ] **双重分屏布局装配 (`StageWorkbenchView`)**：
  - 左侧主工作栏（默认宽 35%）内部通过水平 `OneCellSplitter` 划分为上方 `GitStageTree`（高 62%）与下方 `CommitPanel`（高 38%）；右侧（宽 65%）为交互式 Diff 预览与局部暂存工作台（可通过工具栏 `[👁]` 折叠右侧退化为纯窄栏视图）。
  - 在顶栏标签 `[1 Commit (N)]` 上实时显示待处理变更文件总数徽标。
- [ ] **`GitStageTree` 五态节点模型 (`NodeKind`)**：
  - 严格按 `Merge Conflicts` -> `Staged` -> `Unstaged` -> `Untracked`（>50 个未跟踪文件时自动独立折叠，否则混排在 `Unstaged` 下） -> `Ignored`（可选开启）顺序构建。
  - **支持同一路径双分组并存（Dual Presence）**：当某文件同时存在 `HEAD != Index` 与 `Index != Worktree` 时，同时在 `Staged` 与 `Unstaged` 下生成对应 `GitFileStatusNode`，各自展示该阶段的独立变更类型（例如在 `Staged` 中为 `A Added`，在 `Unstaged` 中为 `M Modified`）。
  - 支持目录分组 / 扁平列表切换，以及跨次刷新的节点展开状态与光标锚点保持（对标 `GitStageTreeStateStrategy`：暂存一个文件后光标自动顺移至下一个未暂存文件，支持一路连按 `Space` 快速审阅暂存！）。
- [ ] **整文件/整目录极速 Stage / Unstage / Rollback**：
  - 键盘：`Space` / `s` / `+` 暂存选中文件或整个文件夹；`Space` / `u` / `-` 取消暂存选中节点；`Ctrl+A` 全选；工具栏 `[+ Stage All Tracked]` 一键暂存所有已跟踪修改。
  - 鼠标：复刻 `GitStageHoverIcon`，光标悬停行尾浮现绿色 `[+]` 或橙色 `[-]` 热区，单击直接流转该文件或文件夹！

---

## 3. 验收标准 (Acceptance Criteria)

- [ ] 修改一个已暂存文件使其同时拥有 Staged 与 Unstaged 更改，`GitStageTree` 在两个分组下同时列出该文件，且选中不同分组下的同名节点时右侧 Diff 准确切换对比基准（`HEAD ↔ Index` vs `Index ↔ Worktree`）。
- [ ] 在包含 10 个未暂存文件的列表中连续按 `Space` 键，文件逐个移入 `Staged` 且焦点平滑顺延不跳回树顶。
- [ ] 存在超过 50 个未跟踪文件时自动折叠进 `▶ Untracked (N)` 节点，避免阻塞树浏览。

---

## Comments
