# 03: 跨工作台共享 `DiffSession`、`ChangesTreeSession` 与自夹紧滚动视口

Status: resolved
Spec: [../spec.md](../spec.md)
Blocked by: 01, 02
Milestone: M8 — 深模块架构治理与测试面收敛 (P1)

- **关联文件**：
  - `crates/jbgit-tui/src/log_view/{details_pane,file_history}.rs`
  - `crates/jbgit-tui/src/stash_view.rs`
  - `crates/jbgit-tui/src/stage_view/{workbench,commit_editor}.rs`
  - `crates/jbgit-tui/src/views/log_details/{mod,changes_browser,diff_view}.rs`
  - `crates/jbgit-tui/src/views/{commit,commit_diff,file_history,stash,conflict_merge,console}.rs`
  - `crates/jbgit-tui/src/app/{state,types,action,dispatch,effects,events}.rs`
  - `crates/jbgit-tui/src/app_git.rs`
  - `crates/jbgit-tui/src/app_stage/{mod,events,partial_diff}.rs`
  - `crates/jbgit-tui/src/tests/file_history_and_changes_tests.rs`

---

## 1. 背景与目标

`VCS Log`、`Stash`、`File History` 与 `Staging Workbench` 四个工作台围绕「变更文件树 + Diff 检视 + 有界视口滚动」复制了大量浅层状态与渲染代码，且暴露出两处真实滚动缺陷：

1. **三套重复缓存与四套 Diff 会话状态机**：
   - `StashTabState`（`stash_view.rs:15`）直接内嵌克隆了整份 `DetailsPaneState`（包含独立的 3 个 LRU 缓存），迫使 `app/events.rs:36-54, 125-182` 与 `app/effects.rs:10-26` 对每条 Diff / Details / Syntax 事件做扇出双写（其中 `app/events.rs:179` 在处理 Stash Diff 时甚至误查了 `self.details.diff_syntax`）。
   - `FileHistoryState`（`log_view/file_history.rs:22-90`）跨模块借用 `state.details.diff_cache` / `diff_syntax`，却重新手写了一套 `pending_diff`、`requested_diff`、`diff_side_by_side`、`diff_context`、`diff_scroll`、`diff_zoomed` 与 `FileDiffRowsCache`（且漏掉了 Hunk 跳转能力）。
   - `StageDiffRow`（`stage_view/workbench.rs:36-40`）重复定义了 `DiffRow`（`details_pane.rs:60-64`），`views/commit_diff.rs` 重复实现了 `views/log_details/diff_view.rs` 中约 180 行 Unified / Split 渲染与词级差异配对逻辑。
   - `Action` 枚举中充斥着按宿主乘以 4 的同构变体（`*ToggleSideBySide`、`*CycleContext`、`*ScrollDiff`、`*JumpHunk`、`*ToggleGroupByDir`、`*ExpandAll`）。
2. **跨帧回写式滚动夹紧遗漏了冲突合并视图与提交编辑器**：
   - `ViewportScrollMetrics`（`app/types.rs:38-92`、`app/state.rs:293-360`）把滚动不变量拆散在 `AppState` 的 6 个 `match` 分支与渲染末尾回写步骤中：
     - `ConflictScroll`（`app_git.rs:285`）被漏接，`cm.scroll = (cm.scroll as isize + delta).max(0) as usize` 无上界夹紧，`PageDown` 越过文件底部后按 `Up` 会陷入无效死区；
     - `CommitMessageEditor::scroll_row`（`stage_view/commit_editor.rs:24`）在 `views/commit.rs:683-689` 被用于裁剪可见行，但全仓没有任何代码更新 `scroll_row`，多行提交信息超过编辑框高度时光标会移出可视区。

本 Issue 提取共享缓存库 **`DiffCacheStore`** 以及三个深模块：**`DiffSession`**、**`ChangesTreeSession`** 与自夹紧 **`ScrollViewport`**，让一套深实现服务全部工作台。

---

## 2. 核心任务清单

- [ ] **提取全局共享缓存模块 `DiffCacheStore`（`jbgit-tui`）**：
  - 将 `CommitChangesBundle`、`FileDiff` 与 `HighlightedDiff` 的 LRU 缓存统一收归单一 `DiffCacheStore` 实例（挂在顶层共享只读/受控协调层），彻底删除 `StashTabState` 中重复持有的第二份 LRU 缓存与 `app/events.rs` / `app/effects.rs` 的多处扇出查表/双写代码，修复 `Stash` 语法高亮缓存查询错位问题。
- [ ] **深化共享 `DiffSession` 与统一/分栏渲染内核（`log_view/*`, `stash_view.rs`, `stage_view/*`, `views/log_details/diff_view.rs`, `views/commit_diff.rs`, `views/file_history.rs`）**：
  - 构建深模块 `DiffSession`（结合 Issue 02 的 `RequestSlot`）：内聚管理 `side_by_side`、`diff_context`、30ms 防抖请求、Unified / Split 投影行缓存、按需 `LazyWordDiffCache`、前/后 Hunk 跳转（自动使 `File History` 同步获得 Hunk 跳转能力）与有界滚动偏移。
  - 统一 `DiffRow` 与 `StageDiffRow` 行投影模型，抽取共享的 Unified / Split 行渲染核心，并通过窄装饰器/适配器接口支持 `Staging Workbench` 特有的 1 列行状态指示槽、交互光标/选区高亮与 Hunk 暂存/回滚胶囊按钮，删除 `views/commit_diff.rs` 与 `views/log_details/diff_view.rs` 之间的重复布局和词级配对实现。
- [ ] **深化共享 `ChangesTreeSession` 并合并同构 `Action`（`log_view/details_pane.rs`, `stash_view.rs`, `app/action.rs`）**：
  - 将 `VCS Log` 与 `Stash` 中完全一致的变更文件目录树构建、折叠代数缓存、展开/折叠（含单目录递归与全部展开/折叠）、上下移动与选中文件定位逻辑收敛进单一 `ChangesTreeSession` 模块，删除 `app/dispatch.rs:693-742` 与 `app_git.rs:366-417` 的重复实现。
  - 用带目标作用域（如 `DiffHost` / `ChangesHost` 或当前激活焦点目标）的统一动作替代按宿主复制 4 遍的 ~20 个 `Action` 变体。
- [ ] **用自夹紧 `ScrollViewport` 替换 `ViewportScrollMetrics` 并修复两处滚动缺陷（`app/types.rs`, `app/state.rs`, `app_git.rs`, `stage_view/commit_editor.rs`, `views/commit.rs`, `views/conflict_merge.rs`）**：
  - 设计内聚的 `ScrollViewport` 值类型（封装偏移量、最近已知内容总行数与视口高度，支持 `scroll_by`、`scroll_to_edge`、`page_step`、`half_page_step`、`ensure_cursor_visible` 与 `bind_geometry(total_lines, viewport_h)` 双向即时夹紧），嵌入各会话内部而非在 `AppState` 维护 6 分支全局表。
  - **修复 `3-Way Conflict Resolver` 滚动越界（`app_git.rs:285`、`views/conflict_merge.rs`）**：将冲突解决器的滚动状态迁移至 `ScrollViewport`，确保触底后任意滚轮/`PageDown`/`G` 均严格夹紧在 `total_lines.saturating_sub(viewport_h)`，按一次 `Up` 立即向上滚动一行。
  - **修复 `CommitMessageEditor` 多行光标滚动跟随（`stage_view/commit_editor.rs`、`views/commit.rs`）**：在编辑器光标移动、换行、粘贴、退格、撤销/重做及渲染视口高度变化时自动更新并夹紧 `scroll_row`，确保光标所在行始终处于编辑框可视窗口内。
- [ ] **测试收敛与新增回归覆盖**：
  - 对 `DiffSession`、`ChangesTreeSession` 与 `ScrollViewport` 的公开接口编写一套集中行为测试（覆盖 Unified/Split 切换、Hunk 跳转、词级懒缓存、`0` 行/短内容/超长内容边界夹紧）。
  - 新增 `3-Way Conflict Resolver` 触底防死区回归测试与 `CommitMessageEditor` 超过视口高度（如 5 行视口输入 15 行文本并上下移动光标）的可视窗口跟随测试。

---

## 3. 验收标准 (Acceptance Criteria)

- [ ] `cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
- [ ] `StashTabState` 不再持有独立的 `DetailsPaneState` 副本，全应用仅保留一份共享 `DiffCacheStore`（在 `VCS Log` 已加载过的提交或文件 Diff，在 `Stash` 或 `File History` 命中同一键时直接零延迟复用）。
- [ ] `StageDiffRow` 与重复的 Unified/Split 渲染循环被消除，且 `Staging Workbench` 的逐行/Hunk 局部暂存、可视块选择、鼠标命中与语法高亮视觉表现 100% 保持不变。
- [ ] `3-Way Conflict Resolver` 连续执行 `PageDown` 触底后按单次 `Up`/`k` 立即向上移动 1 行；`CommitMessageEditor` 输入超过编辑区高度的文本时光标永不跑出可见区域。
- [ ] Spec 08 确立的既有 6 个视口（`ConsoleOutput`、`LogDiff`、`LogDetails`、`StashDiff`、`StageDiff`、`FileHistoryDiff`）滚动夹紧测试全部保持通过。

---

## Comments

- 2026-11-22 (agent): 基于架构审查 Candidate 02 创建本 P1 工单，消除 4 套重复 Diff 会话状态机与双份 LRU 缓存，同时一并修复 `ConflictScroll` 越界死区与 `CommitMessageEditor::scroll_row` 未接线缺陷。
- 2026-11-22 (agent): 已完成实现与验证：提取全局唯一 `DiffCacheStore`、共享 `DiffSession`、`ChangesTreeSession` 与统一 `DiffRow`（彻底删除 `StashTabState` 内嵌的重复 `DetailsPaneState`/LRU 缓存、双写扇出与 `StageDiffRow`，并复用统一 `diff_line_style` 渲染内核）；构建自夹紧 `ScrollViewport` 统一覆盖各工作台并彻底修复 `3-Way Conflict Resolver` 触底滚动死区与 `CommitMessageEditor::scroll_row` 多行光标跟随缺陷；补充完整单测与回归测试，全仓测试与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
