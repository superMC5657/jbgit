# jbgit Feature Specs & Issue Tracker 目录索引

> 本目录为 `jbgit` 项目的本地需求工单与技术规格索引（遵循 [`docs/agents/issue-tracker.md`](../docs/agents/issue-tracker.md) 与 [`AGENTS.md`](../AGENTS.md)），按执行时序汇总 **9 个特性与治理专题（共 24 个原子工单）** 及其架构演进取舍。

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
└── .scratch/                   # 按执行时序组织的 Feature Specs (#00 ~ #08) 与原子 Issues
```

---

> 协作规则、Issue 追踪约定、领域术语表与探索式 TUI 测试指引分别以 [`AGENTS.md`](../AGENTS.md)、[`issue-tracker.md`](../docs/agents/issue-tracker.md) 与 [`ai-exploratory-testing.md`](../docs/agents/ai-exploratory-testing.md) 为准，此处不再重复抄录。
