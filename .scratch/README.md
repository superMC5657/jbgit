# jGit Feature Specs & Issues 索引

---

## 1. Feature 与下属 Issues 总览

| Feature 目录 | Spec 规格书 | 下属 Issues (编号从 `01` 起) | 对应里程碑 |
| :--- | :--- | :--- | :--- |
| **`00-architecture-and-git-engine`** | [`spec.md`](./00-architecture-and-git-engine/spec.md) | • [`01-workspace-tui-runtime-and-hit-test.md`](./00-architecture-and-git-engine/issues/01-workspace-tui-runtime-and-hit-test.md)<br>• [`02-hybrid-git-engine-and-watcher.md`](./00-architecture-and-git-engine/issues/02-hybrid-git-engine-and-watcher.md) | M1 |
| **`01-visual-system-darcula-and-widgets`** | [`spec.md`](./01-visual-system-darcula-and-widgets/spec.md) | • [`01-darcula-theme-14-color-palette-and-icons.md`](./01-visual-system-darcula-and-widgets/issues/01-darcula-theme-14-color-palette-and-icons.md)<br>• [`02-ref-pill-badges-and-grouping.md`](./01-visual-system-darcula-and-widgets/issues/02-ref-pill-badges-and-grouping.md) | M1 / M2 |
| **`02-vcs-log-dag-graph-algorithm`** | [`spec.md`](./02-vcs-log-dag-graph-algorithm/spec.md) | • [`01-permanent-graph-and-dfs-track-layout.md`](./02-vcs-log-dag-graph-algorithm/issues/01-permanent-graph-and-dfs-track-layout.md)<br>• [`02-non-crossing-print-elements-and-bek-sort.md`](./02-vcs-log-dag-graph-algorithm/issues/02-non-crossing-print-elements-and-bek-sort.md)<br>• [`03-tui-graph-cell-rasterizer-and-hit-testing.md`](./02-vcs-log-dag-graph-algorithm/issues/03-tui-graph-cell-rasterizer-and-hit-testing.md)<br>• [`04-kitty-sixel-vector-graph-backend.md`](./02-vcs-log-dag-graph-algorithm/issues/04-kitty-sixel-vector-graph-backend.md) *(可选 M6)* | M2 / M6 |
| **`03-vcs-log-three-pane-linkage`** | [`spec.md`](./03-vcs-log-three-pane-linkage/spec.md) | • [`01-branches-dashboard-left-pane.md`](./03-vcs-log-three-pane-linkage/issues/01-branches-dashboard-left-pane.md)<br>• [`02-vcs-log-table-and-filter-bar.md`](./03-vcs-log-three-pane-linkage/issues/02-vcs-log-table-and-filter-bar.md)<br>• [`03-commit-changes-details-and-diff-linkage.md`](./03-vcs-log-three-pane-linkage/issues/03-commit-changes-details-and-diff-linkage.md) | M3 |
| **`04-staging-workbench-and-partial-hunk`** | [`spec.md`](./04-staging-workbench-and-partial-hunk/spec.md) | • [`01-staging-dual-split-and-changes-tree.md`](./04-staging-workbench-and-partial-hunk/issues/01-staging-dual-split-and-changes-tree.md)<br>• [`02-three-doc-line-tracker-and-partial-staging.md`](./04-staging-workbench-and-partial-hunk/issues/02-three-doc-line-tracker-and-partial-staging.md)<br>• [`03-commit-editor-amend-and-commit-workflow.md`](./04-staging-workbench-and-partial-hunk/issues/03-commit-editor-amend-and-commit-workflow.md) | M4 |
| **`05-branches-popup-context-menus-and-workflows`** | [`spec.md`](./05-branches-popup-context-menus-and-workflows/spec.md) | • [`01-branches-popup-and-speed-search.md`](./05-branches-popup-context-menus-and-workflows/issues/01-branches-popup-and-speed-search.md)<br>• [`02-context-menu-framework-and-mouse-ux.md`](./05-branches-popup-context-menus-and-workflows/issues/02-context-menu-framework-and-mouse-ux.md)<br>• [`03-advanced-git-ops-and-conflict-state-banner.md`](./05-branches-popup-context-menus-and-workflows/issues/03-advanced-git-ops-and-conflict-state-banner.md) | M5 |

---

## 2. 跨 Feature 并行开发波次

- **Wave 1（无阻塞起点，可 4 路并行）**：
  - `00-architecture-and-git-engine/issues/01`（TUI 运行时 & HitTest）
  - `00-architecture-and-git-engine/issues/02`（混合 Git 引擎）
  - `01-visual-system-darcula-and-widgets/issues/01`（Darcula 14 色主题）
  - `02-vcs-log-dag-graph-algorithm/issues/01`（CSR 永久图 & DFS 轨道算法）
- **Wave 2**：`01/.../02`（Pill 徽标）、`02/.../02`（非交叉行生成）-> `02/.../03`（字符光栅化后端）
- **Wave 3（双线并行）**：`03-vcs-log-three-pane-linkage`（三屏联动日志台）与 `04-staging-workbench-and-partial-hunk`（暂存工作台与行级局部暂存）
- **Wave 4**：`05-branches-popup-context-menus-and-workflows`（分支浮窗、右键菜单与高阶 Git 操作）
- **Wave 5（可选扩展）**：`02-vcs-log-dag-graph-algorithm/issues/04`（`tiny-skia` + Kitty/Sixel 矢量像素轨道后端）

---

## 3. 目标 Rust Workspace 拓扑

```text
jGit/
├── Cargo.toml                  # Workspace root
├── crates/
│   ├── jgit-core/              # 纯领域模型与无 UI 依赖的核心算法库（graph / refs / stage / filter）
│   ├── jgit-engine/            # git2 线程池 + git CLI 执行器 (GIT_OPTIONAL_LOCKS=0) + notify 防抖监听
│   └── jgit-tui/               # ratatui + crossterm 终端界面层（theme / layout / widgets / views）
└── .scratch/                   # 按 Feature 组织的 Specs 与 Issues
```
