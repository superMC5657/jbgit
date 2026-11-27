# jbgit Feature Specs & Issue Tracker 目录索引

> 本目录为 `jbgit` 项目的本地需求工单与技术规格索引（遵循 [`docs/agents/issue-tracker.md`](../docs/agents/issue-tracker.md) 与 [`AGENTS.md`](../AGENTS.md)），按执行时序汇总 **17 个特性与治理专题（共 83 个原子工单）** 及其架构演进取舍。

---

## 目录规范约定

- **单个特性对应单一目录**：`.scratch/<NN>-<feature-slug>/`
- **核心规格文档**：`.scratch/<NN>-<feature-slug>/spec.md`
- **原子工单拆解**：`.scratch/<NN>-<feature-slug>/issues/<NN>-<slug>.md`（从 `01` 递增编号）
- **工单状态流转**：工单头部通过 `Status:` 标注状态（`needs-triage` / `needs-info` / `ready-for-agent` / `ready-for-human` / `wontfix` → `claimed` → `resolved`），遵循 [`docs/agents/triage-labels.md`](../docs/agents/triage-labels.md)（注：`#06` 工单文件内已显式更新为 `resolved`；其余已完成专题的代码已合入主干，工单文件内历史 `Status:` 字段保留初始状态）。
- **现状真相优先**：现行系统架构边界、快捷键预设与 TOML 配置规范以 [`README.md`](../README.md) 及 [`docs/manual.md`](../docs/manual.md) 为准；各专题的演进、扩展与功能取舍见下文「架构演进与取舍清单」。

---

## 执行时序全景速查表 (Master Execution Timeline)

| 顺序标识 | 研发阶段 (Phase) | Spec 目录 (Slug) | 专题 / 特性名称 | 落地时间 | 核心 Commit | 里程碑 / 涉及 Crate | 状态 |
| :---: | :--- | :--- | :--- | :---: | :---: | :---: | :---: |
| **#00** | Phase 1: 核心底座与视觉图谱 | [`00-architecture-and-git-engine`](./00-architecture-and-git-engine/spec.md) | 系统总体架构、异步 TUI 事件循环与混合 Git 引擎 | 2026-11-18 | `763cb60` | M1 (`core` / `engine` / `tui`) | 2 / 2 Resolved |
| **#01** | Phase 1: 核心底座与视觉图谱 | [`01-visual-system-darcula-and-widgets`](./01-visual-system-darcula-and-widgets/spec.md) | 视觉美学规范 — Darcula 14 色轨道、双重分屏与 Pill 徽标（#06 扩展多主题） | 2026-11-18 | `9cf9600` | M1 / M2 (`core` / `tui`) | 2 / 2 Resolved |
| **#02** | Phase 1: 核心底座与视觉图谱 | [`02-vcs-log-dag-graph-algorithm`](./02-vcs-log-dag-graph-algorithm/spec.md) | 提交图排版算法 — DFS 连续轨道、非交叉 DAG 与 Cell/Vector 双渲染后端 | 2026-11-18 | `892cf6b`, `28beca1` | M2 / M6 (`core` / `tui`) | 4 / 4 Resolved |
| **#03** | Phase 2: 核心工作台与高阶工作流 | [`03-vcs-log-three-pane-linkage`](./03-vcs-log-three-pane-linkage/spec.md) | Git Log 三屏联动工作台、多维复合筛选器与语法高亮 Diff 联动 | 2026-11-18 | `7b0a53e` | M3 (`core` / `engine` / `tui`) | 3 / 3 Resolved |
| **#04** | Phase 2: 核心工作台与高阶工作流 | [`04-staging-workbench-and-partial-hunk`](./04-staging-workbench-and-partial-hunk/spec.md) | 变更暂存工作台、三文档行追踪器与 Hunk/行级局部暂存 | 2026-11-18 | `1081ee4` | M4 (`core` / `engine` / `tui`) | 3 / 3 Resolved |
| **#05** | Phase 2: 核心工作台与高阶工作流 | [`05-branches-popup-context-menus-and-workflows`](./05-branches-popup-context-menus-and-workflows/spec.md) | 全局分支浮窗、右键上下文菜单、高阶 Git 写操作与三方冲突合并工作台 | 2026-11-18 | `37b4602` | M5 (`core` / `engine` / `tui`) | 3 / 3 Resolved |
| **#06** | Phase 3: 配置体系与国际化扩展 | [`06-config-system-and-i18n`](./06-config-system-and-i18n/spec.md) | Yazi 风格四层配置体系、中英 i18n 国际化、主题 Flavors 与预设/动态键位系统 | 2026-11-19 | `b03c75a`, `334d605`, `76be594` | M6 (`tui`) | 3 / 3 Resolved |
| **#07** | Phase 4: 控制台治理与视口体验加固 | [`07-console-output-stream-and-severity`](./07-console-output-stream-and-severity/spec.md) | Git 控制台保序输出流重构与 `git4idea` 风格行级语义分级渲染 | 2026-11-19 | `aa00fcf` | M6+ (`core` / `engine` / `tui`) | 2 / 2 Resolved |
| **#08** | Phase 4: 控制台治理与视口体验加固 | [`08-viewport-scroll-clamping-and-console-navigation`](./08-viewport-scroll-clamping-and-console-navigation/spec.md) | 全视图滚动上界双端夹紧（防 Overscroll 卡死）与控制台键盘导航/位置指示 | 2026-11-19 | `4248b7d` | M6+ (`tui`) | 2 / 2 Resolved |
| **#09** | Phase 4: 控制台治理与视口体验加固 | [`09-log-changes-context-menu-and-file-history`](./09-log-changes-context-menu-and-file-history/spec.md) | 变更文件上下文菜单、单文件历史（`--follow` 重命名追踪）与联动 Diff 检视 | 2026-11-20 | — | M6+ (`core` / `engine` / `tui`) | 3 / 3 Resolved |
| **#10** | Phase 5: 性能架构治理与交互零卡顿优化 | [`10-diff-switching-and-render-performance`](./10-diff-switching-and-render-performance/spec.md) | Diff 切换延迟治理、异步语法高亮、每帧渲染缓存与大仓库性能优化 | 2026-11-21 | `8c0ae20`, `01f2791` | M7 (`core` / `engine` / `tui`) | 5 / 5 Resolved |
| **#11** | Phase 6: 深模块架构治理与测试面收敛 | [`11-deep-module-architecture-hardening`](./11-deep-module-architecture-hardening/spec.md) | 统一命令目录与纯路由、消费者级单飞槽、共享 Diff 会话、深工作台封装与确定性 `TuiSession` | 2026-11-22 | — | M8 (`core` / `engine` / `tui` / `pilot`) | 6 / 6 Resolved |
| **#12** | Phase 7: 对齐 reference 基线的功能缺口补齐 | [`12-reference-parity-gap-closure`](./12-reference-parity-gap-closure/spec.md) | Annotate、选区历史、Tag 管理、完整 Push / Merge 对话框、Update Project、远端与凭据、仓库切换、Worktree、多仓库决策、图谱折叠、应用 Patch / 按版本浏览、仓库能力感知 | 2026-11-24 | `77a4a68`..`2b834cd` | M9 (`core` / `engine` / `tui` / `pilot`) | 13 / 14 Resolved（1 wontfix） |
| **#13** | Phase 8: 对齐 reference 基线的深度补齐与正确性加固 | [`13-reference-parity-depth-and-correctness`](./13-reference-parity-depth-and-correctness/spec.md) | Commit/Stage/冲突/选区 P0 正确性修复、Diff 空白/横滚/搜索、Pull/Rebase 选项弹窗、Upstream/分支清理/选项补齐、Commit 模板与 Push 加固、FileHistory/Annotate 深化、真正 3-Way 合并队列、Patch/Tree 增强、高亮器/LinearBek/Reflog、增量刷新分页、Clone/Init/CRLF、GPG/Bisect/细节三件套 | 规划中（2026-11-24） | — | M10 (`core` / `engine` / `tui` / `pilot`) | 0 / 14 Open（全部 `ready-for-agent`） |
| **#14** | Phase 9: 探索性 TUI QA（Round 1）缺陷修复 | [`14-pilot-qa-bugfix-and-ergonomics`](./14-pilot-qa-bugfix-and-ergonomics/spec.md) | Windows `\\?\` Clone 路径、`--unshallow` 图谱刷新、暂存 spinner / `UD` 冲突重置 / FileHistory 范围、变更树与分支快捷键、ConPTY 按键编码、筛选栏与弹窗打磨（19 项） | 2026-11-25 | `15d8358`..`2760aea` | M11 (`core` / `engine` / `tui` / `pilot`) | 6 / 6 Resolved |
| **#15** | Phase 10: 工作流正确性与 UI 交互加固 | [`15-ui-and-workflow-hardening`](./15-ui-and-workflow-hardening/spec.md) | Smart Merge 2 空格文件名解析与 Stash 恢复失败分类、Commit 初始文件选中与弹窗 `Ctrl+a` 全选统一、窄屏弹窗/对比栏/边框对齐与菜单宽度、Hover 反馈一致性 | 规划中（2026-11-25） | — | M12 (`core` / `engine` / `tui`) | 0 / 4 Open（全部 `ready-for-agent`） |
| **#16** | Phase 11: 大仓库与渲染/并发性能二期治理 | [`16-large-repo-and-ui-performance-hardening`](./16-large-repo-and-ui-performance-hardening/spec.md) | 消除 `with_thread_repo` 强刷索引、`SnapshotCommitCache` `Arc` 共享与全仓 Copy 扫描；DAG 边去重/线性宽度扫描/零分配 Filter；三方冲突 `O(L²)` 与软换行/搜索缓存；Details/Footer/Console 与矢量图滚动优化；Tokio 解阻与 Windows/Watcher/基准补齐 | 2026-11-28 | `1f8c349`..`9e76d21` | M13 (`core` / `engine` / `tui`) | 5 / 7 Resolved（4 项收窄范围；1 wontfix；1 needs-triage） |

---

## 阶段演进摘要

### Phase 1: 核心底座、视觉系统与 DAG 提交图谱（2026-11-18）

奠定 `jbgit` 三层 Cargo Workspace（`jbgit-core` / `jbgit-engine` / `jbgit-tui`）与 60 FPS 无阻塞终端运行时骨架，确立 IDEA 风格视觉语法与非交叉拓扑提交图谱算法。

#### #00 · `00-architecture-and-git-engine`
- **定位**：系统总体架构、异步 TUI 事件循环与混合 Git 引擎（2 / 2 Resolved）。
- **原子工单**：
  - [`01-workspace-tui-runtime-and-hit-test.md`](./00-architecture-and-git-engine/issues/01-workspace-tui-runtime-and-hit-test.md) — Cargo Workspace、Elm/TEA 单向数据流主循环、`OneCellSplitter` 与 `HitTestRegistry` 空间命中系统。
  - [`02-hybrid-git-engine-and-watcher.md`](./00-architecture-and-git-engine/issues/02-hybrid-git-engine-and-watcher.md) — `git2` 线程池（含 `RequestEpoch` 取消机制）+ 原生 `git` CLI 异步执行器 + `notify-debouncer-mini` 文件监听 + `GitConsoleEntry` 审计流。
- **演进备注**：早期硬编码品牌名 `jGit` 已由 Commit `c05f4fc` 统一重命名为 `jbgit`；`GitConsoleEntry` 的 `stdout`/`stderr` 分离存储由 `#07` 重构为单序列保序流。

#### #01 · `01-visual-system-darcula-and-widgets`
- **定位**：Darcula 语义色板、14 色轨道调色板与 Pill 引用徽标系统（2 / 2 Resolved）。
- **原子工单**：
  - [`01-darcula-theme-14-color-palette-and-icons.md`](./01-visual-system-darcula-and-widgets/issues/01-darcula-theme-14-color-palette-and-icons.md) — TrueColor RGB / ANSI 256 兼容色板、`ThemePalette` 令牌体系与 `GlyphMode::Unicode` / `NerdFont` 双模字形。
  - [`02-ref-pill-badges-and-grouping.md`](./01-visual-system-darcula-and-widgets/issues/02-ref-pill-badges-and-grouping.md) — 跟踪分支合并（`origin & main`）、引用优先级排序与紧凑胶囊折叠渲染。
- **演进备注**：硬编码主题色后续由 `#06` 扩展为支持 `darcula`、`catppuccin-mocha`、`gruvbox-dark` Flavors 与 `theme.toml` 自定义调色板覆盖；宽字符测量由 `#06` Issue 02 强化 CJK 对齐。

#### #02 · `02-vcs-log-dag-graph-algorithm`
- **定位**：IntelliJ `vcs-log` 级别两阶段解耦 DAG 提交图布局与双渲染后端（4 / 4 Resolved）。
- **原子工单**：
  - [`01-permanent-graph-and-dfs-track-layout.md`](./02-vcs-log-dag-graph-algorithm/issues/01-permanent-graph-and-dfs-track-layout.md) — 紧凑 CSR `PermanentLinearGraph`、`GitBranchLayoutComparator` 与迭代 DFS 连续 `layout_index` 分配。
  - [`02-non-crossing-print-elements-and-bek-sort.md`](./02-vcs-log-dag-graph-algorithm/issues/02-non-crossing-print-elements-and-bek-sort.md) — `EdgesInRowGenerator`、`GraphElementComparatorByLayoutIndex` 非交叉行投影与 Bek 拓扑排序。
  - [`03-tui-graph-cell-rasterizer-and-hit-testing.md`](./02-vcs-log-dag-graph-algorithm/issues/03-tui-graph-cell-rasterizer-and-hit-testing.md) — 双列字符矩阵 DAG 轨道光栅化器与轨道节点/箭头鼠标命中交互。
  - [`04-kitty-sixel-vector-graph-backend.md`](./02-vcs-log-dag-graph-algorithm/issues/04-kitty-sixel-vector-graph-backend.md) — 基于 `tiny-skia` 与 Kitty / Sixel / iTerm2 协议的像素级抗锯齿矢量轨道后端（`graphics-protocol` 特性，Commit `28beca1` 落地）。

---

### Phase 2: 核心工作台与高阶 Git 工作流（2026-11-18）

围绕 `[2 Log]`、`[1 Commit]`、`[3 Stash]` 与全局分支/右键菜单打通完整日常与高阶 Git 工作流。

#### #03 · `03-vcs-log-three-pane-linkage`
- **定位**：Git Log 三屏联动工作台与多维复合筛选器（3 / 3 Resolved，Commit `7b0a53e`）。
- **原子工单**：
  - [`01-branches-dashboard-left-pane.md`](./03-vcs-log-three-pane-linkage/issues/01-branches-dashboard-left-pane.md) — 左侧可折叠 `Branches Dashboard` 目录树、`↑↓` 领先/落后计数、星标收藏与筛选联动。
  - [`02-vcs-log-table-and-filter-bar.md`](./03-vcs-log-three-pane-linkage/issues/02-vcs-log-table-and-filter-bar.md) — 中部虚拟化提交图表格与多维筛选栏（文本/正则/大小写、Branch/User/Date/Paths 芯片与高亮器）。
  - [`03-commit-changes-details-and-diff-linkage.md`](./03-vcs-log-three-pane-linkage/issues/03-commit-changes-details-and-diff-linkage.md) — 右侧双重分屏（提交变更文件树 + 提交详情元数据 + `syntect` 语法高亮联动 Diff 预览）。

#### #04 · `04-staging-workbench-and-partial-hunk`
- **定位**：变更暂存工作台、三文档行追踪器与 Hunk/逐行局部暂存（3 / 3 Resolved，Commit `1081ee4`）。
- **原子工单**：
  - [`01-staging-dual-split-and-changes-tree.md`](./04-staging-workbench-and-partial-hunk/issues/01-staging-dual-split-and-changes-tree.md) — 暂存工作台双重分屏与五态 `GitStageTree`（Conflicted / Staged / Unstaged / Untracked / Ignored）。
  - [`02-three-doc-line-tracker-and-partial-staging.md`](./04-staging-workbench-and-partial-hunk/issues/02-three-doc-line-tracker-and-partial-staging.md) — 三文档（`HEAD` / `Index` / `Worktree`）行状态追踪器、`imara-diff` 差分与 `git2::Index::add_frombuffer` 内存级局部暂存。
  - [`03-commit-editor-amend-and-commit-workflow.md`](./04-staging-workbench-and-partial-hunk/issues/03-commit-editor-amend-and-commit-workflow.md) — 多行提交编辑器、`Amend` 修正模式、Sign-off/Hooks 选项、提交历史回填与防空提交流闭环。

#### #05 · `05-branches-popup-context-menus-and-workflows`
- **定位**：全局分支速查浮窗、通用上下文菜单与高阶 Git 写操作闭环（3 / 3 Resolved，Commit `37b4602`）。
- **原子工单**：
  - [`01-branches-popup-and-speed-search.md`](./05-branches-popup-context-menus-and-workflows/issues/01-branches-popup-and-speed-search.md) — 全局 `GitBranchesPopup`（`nucleo-matcher` 模糊速查、级联操作子菜单与顶栏分支胶囊联动）。
  - [`02-context-menu-framework-and-mouse-ux.md`](./05-branches-popup-context-menus-and-workflows/issues/02-context-menu-framework-and-mouse-ux.md) — 通用右键/键盘上下文菜单引擎、视口边缘自动翻转与助记符系统。
  - [`03-advanced-git-ops-and-conflict-state-banner.md`](./05-branches-popup-context-menus-and-workflows/issues/03-advanced-git-ops-and-conflict-state-banner.md) — Reset（Soft/Mixed/Hard/Keep）、Cherry-Pick、Revert、Reword/Drop/Squash、Stash 工作台与交互式三方冲突解决器。

---

### Phase 3: 配置体系与国际化扩展（2026-11-19）

解耦早期硬编码界面文案、主题配色、面板分屏比例与静态快捷键，引入 Yazi 风格的模块化多文件配置体系。

#### #06 · `06-config-system-and-i18n`
- **定位**：Yazi 风格四层 TOML 配置体系、中英全量国际化、主题 Flavors 与预设/动态键位系统（3 / 3 Resolved，Commits `b03c75a` / `334d605` / `76be594`）。
- **原子工单**：
  - [`01-config-discovery-and-yazi-multi-file-system.md`](./06-config-system-and-i18n/issues/01-config-discovery-and-yazi-multi-file-system.md) — 四级覆盖发现链（`JBGIT_CONFIG_DIR` > `.jbgit/` > 用户配置目录 > `include_str!` 默认值）、`jbgit.toml` / `[layout]` 全向分屏比例校验，以及 `--init-config` / `--print-default-config` CLI 命令。
  - [`02-i18n-localization-engine-and-cjk-support.md`](./06-config-system-and-i18n/issues/02-i18n-localization-engine-and-cjk-support.md) — 内置 `zh-CN` / `en-US` 语言包、`I18nManager`、CJK 双宽字符对齐与输入法支持（Commit `1b9030b` 补全简体中文局部润色）。
  - [`03-theme-flavors-and-dynamic-keymaps.md`](./06-config-system-and-i18n/issues/03-theme-flavors-and-dynamic-keymaps.md) — `darcula` / `catppuccin-mocha` / `gruvbox-dark` 主题预设、`keymap.toml`（`default` / `jetbrains` / `vim` 预设、`prepend_keymap` / `append_keymap` / `noop`、`g g` 多键序列与 Which-Key 提示），以及 `?` 可搜索命令帮助面板与 [`docs/manual.md`](../docs/manual.md)。

---

### Phase 4: 控制台治理与视口体验加固（2026-11-19 ~ 2026-11-20）

针对实际 Git 工作流与探索式测试暴露的交互细节进行专项收敛与稳定性加固。

#### #07 · `07-console-output-stream-and-severity`
- **定位**：Git 控制台时序输出流与 `git4idea` 风格语义分级渲染（2 / 2 Resolved，Commit `aa00fcf`）。
- **原子工单**：
  - [`01-ordered-console-stream-and-severity-classifier.md`](./07-console-output-stream-and-severity/issues/01-ordered-console-stream-and-severity-classifier.md) — 将 `GitConsoleEntry` 重构为保序 `Vec<ConsoleLine>`，在 `jbgit-core` 实现对齐 `GitImplBase.ERROR_INDICATORS` 的纯函数分类器 `classify_console_line`（`Normal` / `Info` / `Warning` / `Error`）。
  - [`02-console-view-semantic-rendering-and-stream-ux.md`](./07-console-output-stream-and-severity/issues/02-console-view-semantic-rendering-and-stream-ux.md) — 移除对正常 `stderr` 进度/摘要的 `[stderr] ` 硬编码前缀与错误标红，按语义分级映射 `ThemePalette` 配色。

#### #08 · `08-viewport-scroll-clamping-and-console-navigation`
- **定位**：全视图滚动上界双端夹紧（防 Overscroll 卡死）与控制台详情导航增强（2 / 2 Resolved，Commit `4248b7d`）。
- **原子工单**：
  - [`01-bounded-viewport-scroll-and-overscroll-fix.md`](./08-viewport-scroll-clamping-and-console-navigation/issues/01-bounded-viewport-scroll-and-overscroll-fix.md) — 引入 `apply_scroll_delta` 饱和算术与 `ViewportScrollMetrics` 每帧几何回写机制，在事件派发与渲染收尾阶段双端夹紧全部 5 个文本/Diff 视口（`console_output`、`log_diff`、`log_details`、`stash_diff`、`stage_diff`）。
  - [`02-console-navigation-keybindings-and-scroll-indicator.md`](./08-viewport-scroll-clamping-and-console-navigation/issues/02-console-navigation-keybindings-and-scroll-indicator.md) — 为 `PaneId::ConsoleOutputDetail` 补齐 `gg` / `G` / `Home` / `End` / 半页滚动与按真实视口高度的 `PageUp` / `PageDown`，并增加标题栏行范围/百分比与右边缘细滚动条指示。

#### #09 · `09-log-changes-context-menu-and-file-history`
- **定位**：对齐 IDEA `VcsLogChangesBrowser` 的变更文件上下文菜单、文件级操作与跨重命名追踪单文件历史检视器（3 / 3 Resolved）。
- **原子工单**：
  - [`01-file-history-engine-and-follow-rename.md`](./09-log-changes-context-menu-and-file-history/issues/01-file-history-engine-and-follow-rename.md) — 在 `jbgit-core` / `jbgit-engine` 新增 `FileHistoryKey`/`FileHistoryBundle`、`-z --raw --numstat` `git log --follow` 异步可取消加载管线与 `GitOp::CheckoutPaths` 单文件版本恢复。
  - [`02-log-changes-context-menu-and-file-ops.md`](./09-log-changes-context-menu-and-file-history/issues/02-log-changes-context-menu-and-file-ops.md) — 为 `PaneId::LogChangesTree` / `HitTarget::LogChangeRow` 接入右键与键盘上下文菜单（单文件历史、路径筛选、自动定位的本地对比、`Get Version`、`$EDITOR`、复制路径/补丁），并在 `[1 Commit]` 暂存树复用文件历史入口。
  - [`03-file-history-view-and-diff-inspector.md`](./09-log-changes-context-menu-and-file-history/issues/03-file-history-view-and-diff-inspector.md) — 实现 `PopupKind::FileHistory` 双分屏覆盖视图（重命名路径徽标、30ms 防抖语法高亮 Diff 联动、`--follow`/对比本地/双版本对比、`Enter`/`g` 跳回主日志、`Esc` 无损返回与 `ScrollPane::FileHistoryDiff` 滚动夹紧）。

---

### Phase 5: 性能架构治理与交互零卡顿优化（2026-11-21）

围绕 2026-11-21 基准测试与全仓热路径审计发现的 19 项底层瓶颈，系统治理切换代码修改延迟、每帧渲染抖动与大仓库扩展性问题。

#### #10 · `10-diff-switching-and-render-performance`
- **定位**：Diff 切换延迟治理、异步语法高亮、每帧渲染缓存与大仓库性能优化（5 / 5 Resolved，Commits `8c0ae20` / `a2f8630` / `01f2791`）。
- **原子工单**：
  - [`01-async-syntax-highlighting-and-safe-diff-limits.md`](./10-diff-switching-and-render-performance/issues/01-async-syntax-highlighting-and-safe-diff-limits.md) — P0：`Cargo.toml` Dev 重计算依赖优化、`syntect` 后台预热与异步可取消高亮、单流消除 `Context` 双重高亮、4 MiB 大文件 / 2048 B 长行安全护栏、`SlicedDocument` 零逐行拷贝与 `StageLineTracker` / `word_diff` / span 线性化。
  - [`02-fast-diff-loading-commit-cache-and-event-coalescing.md`](./10-diff-switching-and-render-performance/issues/02-fast-diff-loading-commit-cache-and-event-coalescing.md) — P0（Blocked by `01`）：`load_file_diff_sync` 目标树直读快路径（绕开全树 `resolve_changes` + `find_similar`）、异步化签名验证、`StageWorkbenchState` LRU 缓存与 30ms 防抖、定点冲突查询与主循环输入事件合并。
  - [`03-render-hot-path-caching-and-vector-graph-perf.md`](./10-diff-switching-and-render-performance/issues/03-render-hot-path-caching-and-vector-graph-perf.md) — P1：持久化 `EdgesInRowGenerator` 缓存、预解析键位与单帧 Footer 规划、缓存 `change_rows` / `sync_rows` / `diff_rows` / Refs 索引 / 过滤正则，并实现矢量图按行局部重栅格化与 Sixel `[bool; 256]` 编码提速。
  - [`04-engine-worker-priority-and-watcher-isolation.md`](./10-diff-switching-and-render-performance/issues/04-engine-worker-priority-and-watcher-isolation.md) — P1：`Git2WorkerPool` 高优交互 / 后台扫描双优先级调度、将 `RepoWatcher::is_path_ignored_sync` 隔离出共享 worker 池、以及提交草稿 300ms 防抖异步落盘。
  - [`05-large-repo-filtering-graph-and-status-scaling.md`](./10-diff-switching-and-render-performance/issues/05-large-repo-filtering-graph-and-status-scaling.md) — P2（Blocked by `03, 04`）：Log 文本过滤 120ms 防抖与单次 `VisibleGraph` 构建、`containing_branches` 异步化与缓存、`load_path_matches_sync` 接入 `git log --simplify-merges`（Bloom filter），以及 Windows 大工作区状态/多分支 `ahead_behind`/快照体积优化。

---

### Phase 6: 深模块架构治理与确定性测试面收敛（2026-11-22）

针对 `16a75c6` 物理拆文件后残留的浅模块簇、跨接缝状态泄漏及调用方编排裂隙，按深模块设计原则系统提升局部性（Locality）、接口杠杆（Leverage）与端到端确定性可测性。

#### #11 · `11-deep-module-architecture-hardening`
- **定位**：深模块架构治理、接缝收敛与确定性测试面重构（6 / 6 Resolved）。
- **原子工单**：
  - [`01-command-catalog-pure-input-and-modal-stack.md`](./11-deep-module-architecture-hardening/issues/01-command-catalog-pure-input-and-modal-stack.md) — P0：单一 `CommandCatalog` 统一键盘/菜单/底栏/帮助命令规格（消除 `find_action_by_mnemonic` 助记符反向抓取与 `ContextMenuAction`）、`route_terminal_event` 纯函数化、以及自包含 `ModalStack`（合并 `CriticalDialog` 与清理旁路弹窗状态）。
  - [`02-consumer-scoped-request-slots-and-engine-seam.md`](./11-deep-module-architecture-hardening/issues/02-consumer-scoped-request-slots-and-engine-seam.md) — P0：按消费者槽位隔离的 `RequestSlot` 单飞取消（修复跨视图共享 `EpochTracker` 导致 `"Loading diff…"` 卡死缺陷），隐藏 `Git2WorkerPool` 透传与底层 `*_sync` 细节并提供本地测试替身接缝。
  - [`03-shared-diff-changes-and-bounded-scroll-sessions.md`](./11-deep-module-architecture-hardening/issues/03-shared-diff-changes-and-bounded-scroll-sessions.md) — P1（Blocked by `01, 02`）：全局唯一 `DiffCacheStore`（消除 `StashTabState` 重复 LRU）、跨四工作台共享 `DiffSession` / `ChangesTreeSession`，以及自夹紧 `ScrollViewport`（修复 `ConflictScroll` 越界死区与 `CommitMessageEditor::scroll_row` 光标跟随）。
  - [`04-deep-workbench-modules-and-appstate-encapsulation.md`](./11-deep-module-architecture-hardening/issues/04-deep-workbench-modules-and-appstate-encapsulation.md) — P1（Blocked by `01, 03`）：将 8 文件 `impl AppState` 碎片与 95+ 公开字段收敛为 `CommitWorkbench`、`LogWorkbench`、`StashWorkbench`、`ConsoleWorkbench` 四大深模块，内聚 `rows` 自动同步与提交导航重置不变量。
  - [`05-unified-git-op-compiler-subprocess-and-hunk-builder.md`](./11-deep-module-architecture-hardening/issues/05-unified-git-op-compiler-subprocess-and-hunk-builder.md) — P1（Blocked by `02`）：纯声明式 `GitOp`、统一 `jbgit-engine` CLI 编译/`prepare_pathspec_args`/`index.lock` 子进程双适配器接缝，以及合并双文档/三文档 Hunk 构建并保留原始 `'\t'` 修复 `[diff] tab_size` 失效。
  - [`06-deterministic-tui-session-driver-and-pilot-codec.md`](./11-deep-module-architecture-hardening/issues/06-deterministic-tui-session-driver-and-pilot-codec.md) — P2（Blocked by `04, 05`）：提取库级 `TuiSession` 确定性回合驱动、按 Replace-Don't-Layer 替换 6 套手写残缺测试循环，并将 `tools/jbgit-pilot` 纳入 Workspace 共享键位编解码。

---

### Phase 7: 对齐 reference 基线的功能缺口补齐（2026-11-23 ~ 2026-11-24）

以 `reference/`（`git4idea` / `vcs-log` / `vcs-impl`）为基线做功能面差距评估并补齐缺口，将「终端可适用功能面」覆盖度由 ≈ 55%–60% 提升至 ≈ 75%–82%。

#### #12 · `12-reference-parity-gap-closure`
- **定位**：reference 功能缺口补齐路线图（13 / 14 Resolved，Issue 11 wontfix，Commits `77a4a68`..`2b834cd`、`8491a11`）。
- **原子工单**：
  - [`01-blame-annotate-engine-and-view.md`](./12-reference-parity-gap-closure/issues/01-blame-annotate-engine-and-view.md) — P0：`git blame --porcelain` 可取消加载管线与全文件注解视图（年龄着色、Annotate Previous Revision、跳转 Log）。
  - [`02-show-history-for-selection.md`](./12-reference-parity-gap-closure/issues/02-show-history-for-selection.md) — P1（Blocked by `01`）：`git log -L` 行区间历史，复用 File History 视图。
  - [`03-tag-management.md`](./12-reference-parity-gap-closure/issues/03-tag-management.md) — P0：删除 / 恢复本地 Tag、删除远端 Tag、推送单个 Tag 与 Tag 菜单。
  - [`04-push-dialog-commit-preview-and-targets.md`](./12-reference-parity-gap-closure/issues/04-push-dialog-commit-preview-and-targets.md) — P0：待推提交预览、目标编辑、Tag 推送模式、受保护分支与被拒后更新重试。
  - [`05-merge-dialog-options.md`](./12-reference-parity-gap-closure/issues/05-merge-dialog-options.md) — P1：`--no-ff` / `--ff-only` / `--squash` / `--no-commit` / 消息 / 策略及互斥校验。
  - [`06-update-project-flow-and-update-info.md`](./12-reference-parity-gap-closure/issues/06-update-project-flow-and-update-info.md) — P1：保存改动 → fetch → 同步当前分支 → 快进其他分支 → 恢复，及按范围筛选的更新摘要。
  - [`07-remote-management.md`](./12-reference-parity-gap-closure/issues/07-remote-management.md) — P1：`RepoSnapshot.remotes` 与远端增删改。
  - [`08-askpass-credential-bridge.md`](./12-reference-parity-gap-closure/issues/08-askpass-credential-bridge.md) — P1（Blocked by `07`）：`GIT_ASKPASS` / `SSH_ASKPASS` 凭据回调桥（仅当前用户可访问的命名管道 / Unix socket + 会话令牌，不存储凭据）。
  - [`09-repository-session-rebind-and-switcher.md`](./12-reference-parity-gap-closure/issues/09-repository-session-rebind-and-switcher.md) — P1：进程内仓库会话重绑定与最近仓库切换器。
  - [`10-working-trees.md`](./12-reference-parity-gap-closure/issues/10-working-trees.md) — P1（Blocked by `09`）：Worktree 列表 / 创建 / 打开 / 锁定 / 删除 / 清理。
  - [`11-multi-root-model-decision.md`](./12-reference-parity-gap-closure/issues/11-multi-root-model-decision.md) — wontfix：多仓库模型已决策为"单活动仓库 + 快速切换"，由 `09` 的切换器承载。
  - [`12-graph-collapse-linear-branches.md`](./12-reference-parity-gap-closure/issues/12-graph-collapse-linear-branches.md) — P2：`CollapsedGraph` 线性分支折叠 / 展开投影层。
  - [`13-apply-patch-and-browse-at-revision.md`](./12-reference-parity-gap-closure/issues/13-apply-patch-and-browse-at-revision.md) — P2：应用 Patch（预检 / 3-way）与按版本浏览仓库树。
  - [`14-repo-capabilities-shallow-submodule-lfs-signature.md`](./12-reference-parity-gap-closure/issues/14-repo-capabilities-shallow-submodule-lfs-signature.md) — P3：签名状态细分、Submodule 只读标注、LFS 指针占位与 Unshallow（四项全做）。
- **明确不做**：Shelf / Changelists、In-memory Rebase、Clone / Init、多仓库同步模型、IDE 专属能力（理由见 Spec §2.1）。

---

### Phase 8: 对齐 reference 基线的深度补齐与正确性加固（规划中，2026-11-24）

在 Spec 12 落地后对照 `reference/` 做二次全量源码审计（日常单仓库工作流 ≈ 90%–95%，终端可适用功能面 ≈ 75%–82%）。本阶段先消除 4 类 P0 正确性边界缺陷，再按高频程度补齐选项深度、真正 3-way 合并、图谱保真度与 Reflog，目标将终端可适用覆盖度推至 ≈ 92%+。

#### #13 · `13-reference-parity-depth-and-correctness`
- **定位**：reference 深度对齐与正确性加固路线图（0 / 14 Open，全部 `ready-for-agent`；SQLite 持久化索引与 In-Memory Rebase 已拍板 `wontfix`）。
- **原子工单**：
  - [`01-commit-sequencer-and-stage-correctness.md`](./13-reference-parity-depth-and-correctness/issues/01-commit-sequencer-and-stage-correctness.md) — P0：修复 Merge/Sequencer 状态走 `git2` 丢父提交、linked worktree hooks 路径、Symlink/权限/未追踪目录与 Copy 处理。
  - [`02-conflict-semantics-crlf-and-delete-modify.md`](./13-reference-parity-depth-and-correctness/issues/02-conflict-semantics-crlf-and-delete-modify.md) — P0：修复 Rebase 冲突 Ours/Theirs 反转、Delete/Modify `AcceptSide` 走 `git rm`、CRLF 保留、二进制冲突护栏与选块滚屏。
  - [`03-log-non-contiguous-selection-and-compare.md`](./13-reference-parity-depth-and-correctness/issues/03-log-non-contiguous-selection-and-compare.md) — P0：Log 非连续多选解锁任意两提交对比、`ManyChanges` 护栏与「仅显示受影响变更」。
  - [`04-diff-whitespace-horizontal-scroll-and-search.md`](./13-reference-parity-depth-and-correctness/issues/04-diff-whitespace-horizontal-scroll-and-search.md) — P1：Diff 忽略空白（`-b`/`-w`）、水平滚动/软换行、Tab-Stop 对齐、`/` 搜索、二进制元数据与 Log 复制增强。
  - [`05-pull-and-rebase-options-dialogs.md`](./13-reference-parity-depth-and-correctness/issues/05-pull-and-rebase-options-dialogs.md) — P1：Pull 选项对话框、标准 Rebase 对话框（`--onto`/`--rebase-merges`/`--update-refs`）、交互式 Rebase `edit`、独立 `fixup!`/`squash!` 与非连续 Squash。
  - [`06-branch-upstream-cleanup-and-cherry-stash-options.md`](./13-reference-parity-depth-and-correctness/issues/06-branch-upstream-cleanup-and-cherry-stash-options.md) — P1：Set/Unset Upstream、已合并分支批量清理、任意双分支对称对比、Cherry-Pick/Revert `-n`/`-m` 与 Stash `--keep-index`/`--index`。
  - [`07-commit-workflow-templates-merge-msg-and-push-polish.md`](./13-reference-parity-depth-and-correctness/issues/07-commit-workflow-templates-merge-msg-and-push-polish.md) — P1（Blocked by `01`）：`commit.template`、自动加载 `MERGE_MSG`/`SQUASH_MSG`、72 列折行、Commit & Push 直通 Push 对话框与 Push Rejected Smart 重试。
  - [`08-file-history-modal-actions-and-annotate-depth.md`](./13-reference-parity-depth-and-correctness/issues/08-file-history-modal-actions-and-annotate-depth.md) — P1：FileHistory 模态内右键菜单/分支筛选/分页/搜索与 Annotate 拆分 `-M`/`-C`、列定制及 LRU 缓存。
  - [`09-true-three-way-merge-and-conflict-queue.md`](./13-reference-parity-depth-and-correctness/issues/09-true-three-way-merge-and-conflict-queue.md) — P2（Blocked by `02`）：基于 `:1/:2/:3` 的真正 3-way 自动合并、Result 内联编辑与多文件冲突队列视图。
  - [`10-patch-options-and-tree-browser-ergonomics.md`](./13-reference-parity-depth-and-correctness/issues/10-patch-options-and-tree-browser-ergonomics.md) — P2：Apply Patch `-R`/`-p<n>`/部分勾选/Diff 预览、Create Patch 路径选择与 Tree Browser `/` 模糊搜索。
  - [`11-vcs-log-highlighters-graph-fidelity-and-reflog.md`](./13-reference-parity-depth-and-correctness/issues/11-vcs-log-highlighters-graph-fidelity-and-reflog.md) — P2（Blocked by `03`）：`My Commits` / `Already Cherry-Picked` 高亮器、`LinearBek`、`No-Merges`、表内速查与 Reflog 视图。
  - [`12-incremental-log-refresh-and-pagination.md`](./13-reference-parity-depth-and-correctness/issues/12-incremental-log-refresh-and-pagination.md) — P2：轻量增量头部刷新（`VcsLogJoiner` 风格）与突破 50k 渐进分页（不引入 SQLite）。
  - [`13-clone-init-git-executable-and-crlf.md`](./13-reference-parity-depth-and-correctness/issues/13-clone-init-git-executable-and-crlf.md) — P2：Clone / Init 仓库生命周期、`[git] executable` 配置与 Windows CRLF 提交前检测。
  - [`14-gpg-bisect-and-repo-ergonomics.md`](./13-reference-parity-depth-and-correctness/issues/14-gpg-bisect-and-repo-ergonomics.md) — P2：GPG 签名与 SSH Host-Key 指纹确认、Bisect 控制、Submodule gitlink 暂存、`.worktreeinclude` 与灵活 `.gitignore`。
- **明确不做（2026-11-24 拍板 `wontfix`）**：SQLite 持久化索引、In-Memory Rebase，及沿用 Spec 12 的 Shelf/Changelists、多仓库同步模型与 IDE 专属能力。

---

### Phase 9 ~ 10: 探索性 TUI QA 修复与体验加固（2026-11-25）

基于 `jbgit-pilot` 在 Windows ConPTY + `vt100` 下执行端到端探索性测试，按“根因修复 → 回归锁定”持续收敛工作流正确性与 UI 细节。

#### #14 · `14-pilot-qa-bugfix-and-ergonomics`
- **定位**：第一轮探索性 QA 缺陷修复与体验加固（6 / 6 Resolved，Commits `15d8358`..`2760aea`）。
- **原子工单**：
  - [`01-windows-clone-path-and-unshallow-refresh.md`](./14-pilot-qa-bugfix-and-ergonomics/issues/01-windows-clone-path-and-unshallow-refresh.md) — P1（Resolved）：剥离 Windows `\\?\` 克隆路径前缀与 `--unshallow` 线程仓库/快照缓存失效。
  - [`02-staging-notice-and-conflict-unresolve-correctness.md`](./14-pilot-qa-bugfix-and-ergonomics/issues/02-staging-notice-and-conflict-unresolve-correctness.md) — P2（Resolved）：清除暂存进度 spinner、修复 `UD/DU/AA/DD` 冲突重置与 FileHistory 范围标签对齐。
  - [`03-log-changes-tree-and-branches-shortcuts.md`](./14-pilot-qa-bugfix-and-ergonomics/issues/03-log-changes-tree-and-branches-shortcuts.md) — P2（Resolved）：Log 变更树 `c`/`p`/`l` 快捷键、目录右键菜单过滤、分支 `Space` 收藏 / `F2` 重命名与 Diff `Ctrl+F`。
  - [`04-conpty-pilot-keys-and-modal-escape.md`](./14-pilot-qa-bugfix-and-ergonomics/issues/04-conpty-pilot-keys-and-modal-escape.md) — P2（Resolved）：ConPTY 下 `Ctrl+~` / `Ctrl+Enter` / `Esc` 编码与弹窗关闭修复。
  - [`05-filter-bar-and-dialog-layout-overflow.md`](./14-pilot-qa-bugfix-and-ergonomics/issues/05-filter-bar-and-dialog-layout-overflow.md) — P2（Resolved）：140 列筛选栏动态预算、作者弹窗 `Ctrl+U` 与新建 Worktree 对话框提示防截断。
  - [`06-modal-banner-console-and-ux-polish.md`](./14-pilot-qa-bugfix-and-ergonomics/issues/06-modal-banner-console-and-ux-polish.md) — P3（Resolved）：身份弹窗延迟校验、Welcome 占位符与重复报错去重、Bisect/Console 排版与通用弹窗 `Ctrl+a` / 自身分支菜单 / Hunk 回滚文案。

#### #15 · `15-ui-and-workflow-hardening`
- **定位**：工作流正确性与 UI 交互加固（0 / 4 Open，全部 `ready-for-agent`）。
- **原子工单**：
  - [`01-smart-merge-blocked-files-and-stash-restore-failure.md`](./15-ui-and-workflow-hardening/issues/01-smart-merge-blocked-files-and-stash-restore-failure.md) — P2：修复 `git merge` ort 2 空格缩进文件名解析遗漏及 `stash pop` 未跟踪文件恢复失败误报为 `GitFailure::Conflicts`。
  - [`02-commit-initial-selection-and-unified-ctrl-a.md`](./15-ui-and-workflow-hardening/issues/02-commit-initial-selection-and-unified-ctrl-a.md) — P2：`[1] 提交` 首次载入默认选中首条文件行对齐右侧 Diff 预览，补齐 `PushDialog` 与剩余弹窗内嵌文本行 `Ctrl+a` 全选。
  - [`03-modal-narrow-layout-overflow-and-borders.md`](./15-ui-and-workflow-hardening/issues/03-modal-narrow-layout-overflow-and-borders.md) — P2：修复冲突解决弹窗窄屏裁切/穿透、两分支对比栏 `树差异 ✓` 挤掉 `✕`、`Alt+T` 边缘缝隙、Annotate 叠排行历史双底边框、FileHistory 80 列工具栏与上下文菜单宽度预算。
  - [`04-interactive-hover-feedback-consistency.md`](./15-ui-and-workflow-hardening/issues/04-interactive-hover-feedback-consistency.md) — P3：统一冲突解决器底部按钮、Log 筛选芯片/`⚙` 与顶栏分支胶囊的鼠标 Hover 视觉反馈。

---

### Phase 11: 大仓库与渲染/并发性能二期治理（2026-11-28，范围修订后完成）

在 Spec 10 落地及 Spec 12–15 补齐增量刷新、真正 3-Way 合并、Diff 软换行/搜索、Annotate、LinearBek 与 Update Project 后，开展全仓库三层 Crate 二期源码性能审计，系统消除大仓库（5 万–20 万 commits / 1 万–5 万文件，含 Windows）、高帧率渲染、矢量图滚动与长会话内存保留中的 P0/P1 瓶颈。

#### #16 · `16-large-repo-and-ui-performance-hardening`
- **定位**：大仓库、渲染热路径与引擎并发性能二期治理（5 / 7 Resolved，分支 `spec16-perf`）。
- **范围修订**：实测 10 万提交基准后（`recommended_track_width` ≈ 190 ms，构图 ≈ 35 ms，每帧视口 ≈ 0.36 ms），只实施真实卡顿 / bug 且改动小的子集，其余暂缓（汇总到 `07`）或不做，详见 [`spec.md` §5](./16-large-repo-and-ui-performance-hardening/spec.md#5-范围修订2026-11-28)。
- **原子工单**：
  - [`01-engine-index-reload-snapshot-arc-and-copy-scan.md`](./16-large-repo-and-ui-performance-hardening/issues/01-engine-index-reload-snapshot-arc-and-copy-scan.md) — P0（resolved，`0d94bee`）：消除 `with_thread_repo` 强制 `index.read(true)`、`SnapshotCommitCache`/`joiner.rs` 多副本深拷贝阻塞 Tokio 与点击 Untracked 文件触发全仓 `copies_from_unmodified` 扫描。
  - [`02-core-dag-edges-filter-and-visible-graph-perf.md`](./16-large-repo-and-ui-performance-hardening/issues/02-core-dag-edges-filter-and-visible-graph-perf.md) — P0（resolved，收窄为 `recommended_track_width` 单趟扫描 190 ms → 0.35 ms，`a4b0cc6`）：移除冗余 `edges_from_below`、重写 `recommended_track_width` 为单趟 `O(V+E)` 扫描、削减 `VisibleGraph`/`LinearBek` 小 `Vec` 分配与实现零分配 `Oid`/Filter/`natural_cmp` 匹配。
  - [`03-conflict-merge-and-diff-soft-wrap-search-perf.md`](./16-large-repo-and-ui-performance-hardening/issues/03-conflict-merge-and-diff-soft-wrap-search-perf.md) — P0（resolved，收窄为冲突 `O(L²)` / 视口投影、惰性滚动、二分搜索与单次粘贴重算，`013495f`）：修复三方冲突 `Custom` `O(L²)` 克隆并改为视口窗口投影、缓存软换行高度前缀和与惰性滚动 fallback、`DiffSearchState::ranges_for` 二分查找及减少 `-b`/`-w` 重复拷贝。
  - [`04-tui-frame-caching-ui-thread-offload-and-memory-bounds.md`](./16-large-repo-and-ui-performance-hardening/issues/04-tui-frame-caching-ui-thread-offload-and-memory-bounds.md) — P1（wontfix，候选项移入 `07`）：Commit Details / Footer / Filter 弹窗建议缓存、异步大图折叠、Console 虚拟化渲染（保持单命令审计行不硬截断）与 `ConsoleWorkbench::entries`/`rollback_undo_stack` 内存有界化。
  - [`05-vector-graph-scroll-blit-and-resize-coalescing.md`](./16-large-repo-and-ui-performance-hardening/issues/05-vector-graph-scroll-blit-and-resize-coalescing.md) — P1（resolved，收窄为 `set_capabilities` 去无条件作废 + `Resize` 合并，`042aa3b`）：矢量图垂直滚动 `copy_within` 行级搬移、命中区按 `content_key` 缓存与水平合并、Sixel 量化加速及 `Resize` 事件合并。
  - [`06-engine-tokio-unblock-windows-scaling-and-benchmarks.md`](./16-large-repo-and-ui-performance-hardening/issues/06-engine-tokio-unblock-windows-scaling-and-benchmarks.md) — P1（resolved，收窄为 Watcher 过滤 + Added/Deleted 计行快速路径，`9e76d21`）：移除 Tokio 异步线程同步阻塞、优化 `load_commit_details_sync`/批验签/取消链路、治理 Windows 大工作区 `unstage`/串行子进程与 Watcher，并补齐 `graph_layout.rs` Criterion 回归基准。
  - [`07-deferred-perf-candidates.md`](./16-large-repo-and-ui-performance-hardening/issues/07-deferred-perf-candidates.md) — P2（needs-triage）：范围修订后暂缓的特定场景候选项（软换行行高缓存、Commit Details hash 解析、Filter 弹窗缓存、Console 虚拟化、批量 unstage、UpdateProject 串行 spawn）与 `recurse_untracked_dirs` 默认值产品决策。

---

## 架构演进与取舍清单 (Superseded, Pruned & Trade-offs)

在迭代过程中，部分早期规划、数据结构或交互策略做了明确的演进替换、边界收敛或工程取舍，在此集中备注：

### 1. 已被后续架构演进或扩展的实现
- **硬编码界面文案、单一主题与固定分割比例（`#00` / `#01` / `#03` / `#04`）→ `#06 06-config-system-and-i18n`**：演进为 Yazi 风格四层 TOML 配置（`jbgit.toml` / `theme.toml` / `keymap.toml`）、`zh-CN` / `en-US` 双语引擎与 CJK 等宽对齐、3 套内置主题 Flavors（`darcula` / `catppuccin-mocha` / `gruvbox-dark`）、3 套键位预设（`default` / `jetbrains` / `vim`）以及支持水平与垂直子栏位的 `[layout]` 比例配置（Commits `b03c75a`、`334d605`、`76be594`）。
- **控制台 `stdout` / `stderr` 分离数组与 `[stderr]` 全标红（`#00` Issue 02）→ `#07 07-console-output-stream-and-severity`**：`GitConsoleEntry` 由两个独立的 `stdout_lines` / `stderr_lines` 演进为保序 `output_lines: Vec<ConsoleLine>` 与四级行级语义着色（`Normal` / `Info` / `Warning` / `Error`），同时保留底层 `jbgit-engine` `GitCliOutput` 分流字段以兼容冲突与锁竞争解析（Commit `aa00fcf`）。
- **5 个文本/Diff 视口无上界滚动（`#03` / `#04` / `#05`）→ `#08 08-viewport-scroll-clamping-and-console-navigation`**：用 `apply_scroll_delta` 与 `ViewportScrollMetrics` 每帧回写替换原先无上界累加与仅局部变量截断的逻辑，彻底消除触底后继续滚轮或按 `G` 跳转导致的反向滚动死区（Commit `4248b7d`）。
- **早期项目命名 `jGit` → `jbgit`（Commit `c05f4fc`）**：统一项目名、二进制入口、Crate 命名（`jbgit-core` / `jbgit-engine` / `jbgit-tui`）、环境变量 `JBGIT_CONFIG_DIR` 与仓库级局部目录 `.jbgit/`。

### 2. 明确裁撤、收敛或不做的功能取舍
- **混合 Git 引擎读写边界（`#00` Issue 02）**：放弃使用纯 `libgit2` 承担全部 Git 写操作与联网交互，确立「`git2` 多线程只读查询 & 内存 Index 局部暂存 + 原生 `git` CLI 异步子进程执行写操作与远端同步（强制 `GIT_OPTIONAL_LOCKS=0`、`GIT_TERMINAL_PROMPT=0` 与 `index.lock` 退避重试）」分工，保证 GPG 签名、SSH Agent、Hooks 与交互式变基的 100% 兼容性。
- **像素矢量图谱作为可选后端并自动降级（`#02` Issue 04）**：基于 `tiny-skia` 的 Kitty / Sixel / iTerm2 像素级矢量渲染通过 Cargo `graphics-protocol` 特性及 `graph.backend`（`auto` / `vector` / `cell`）接入，未探测到图形协议的终端默认回退至零依赖的双列字符矩阵光栅化器（`#02` Issue 03）。
- **控制台非 PTY 化与无强制行数截断（`#07` / `#08`）**：`[4 Console]` 聚焦 Git 命令审计回放与语义着色，不引入交互式伪终端（PTY）仿真，也不对 `GitConsoleEntry::output_lines` 设强制行数截断上限。
- **底栏快捷键提示收敛（`#06` 后续精简，Commits `159bacf` / `66b36ef`）**：底栏默认展示高频核心操作提示并锚定 `?` 帮助入口，避免窄屏信息过载；完整快捷键通过 `?` 可搜索命令面板查询，并支持在 `keymap.toml` 通过 `footer_max_lines`（`0` / `1` / `2` / `3`）及单条绑定 `show_hint` 按需展开。

### 3. 工程治理、探索式 QA 与仓库拓扑
- **交互式 ConPTY + `vt100` 探索式测试驱动（`tools/jbgit-pilot`，Commits `f43a1db` / `2a9de13`）**：构建 AI Agent 可交互式操纵真实 `jbgit` 二进制的 PTY 探索测试框架（详见 [`docs/agents/ai-exploratory-testing.md`](../docs/agents/ai-exploratory-testing.md)），内置 `full`、`conflict`、`--custom-config` 三类隔离沙箱场景（运行数据隔离于 `.pilot/`，不污染 `.scratch/`），并已通过探索式 QA 驱动修复弹窗内部点击误关闭、分支弹窗动态键位路由与 `GoToRevision` 窄视口 `clamp` panic 等边缘缺陷（Commits `f9cca34`、`d8d74ef`）。
- **现行 Workspace 与工程拓扑**：

```text
jbgit/
├── Cargo.toml                  # Workspace 根配置 (Rust 2024 Edition)
├── crates/
│   ├── jbgit-core/             # 纯领域模型与核心算法（graph / stage_tracker / console / refs / filter / conflict）
│   ├── jbgit-engine/           # git2 线程池 + git CLI 异步执行器 (GIT_OPTIONAL_LOCKS=0) + notify 防抖监听
│   └── jbgit-tui/              # ratatui + crossterm 终端界面（config / i18n / theme / keymap / hit_test / views）
├── tools/
│   └── jbgit-pilot/            # ConPTY + vt100 交互式终端探索测试驱动
├── docs/
│   ├── manual.md               # 用户操作与 TOML 配置手册
│   └── agents/                 # Agent 协作指引（issue-tracker / triage-labels / domain / ai-exploratory-testing）
└── .scratch/                   # 按执行时序组织的 Feature Specs (#00 ~ #16) 与原子 Issues
```

---

> 协作规则、Issue 追踪约定、领域术语表与探索式 TUI 测试指引分别以 [`AGENTS.md`](../AGENTS.md)、[`issue-tracker.md`](../docs/agents/issue-tracker.md) 与 [`ai-exploratory-testing.md`](../docs/agents/ai-exploratory-testing.md) 为准，此处不再重复抄录。
