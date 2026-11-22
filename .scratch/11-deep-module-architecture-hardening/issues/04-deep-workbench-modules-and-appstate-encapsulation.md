# 04: 四大深工作台模块封装与 `AppState` 公开字段收敛

Status: resolved
Spec: [../spec.md](../spec.md)
Blocked by: 01, 03
Milestone: M8 — 深模块架构治理与测试面收敛 (P1)

- **关联文件**：
  - `crates/jbgit-tui/src/app/{mod,state,types,action,dispatch,events}.rs`
  - `crates/jbgit-tui/src/app_git.rs`
  - `crates/jbgit-tui/src/app_stage/{mod,events,commit_flow,partial_diff}.rs`
  - `crates/jbgit-tui/src/stage_view/{mod,workbench,commit_editor}.rs`
  - `crates/jbgit-tui/src/log_view/{mod,branches_pane,filter_bar,details_pane,presentation}.rs`
  - `crates/jbgit-tui/src/stash_view.rs`
  - `crates/jbgit-tui/src/views/{mod,log,log_branches,commit,stash,console,header,footer}.rs`

---

## 1. 背景与目标

Commit `16a75c6` 将原 3100 行的 `app.rs` 与 1100 行的 `app_stage.rs` 按行数切分为 11 个文件，但保留了 `AppState` 上 **54 个 `pub` 字段**、`StageWorkbenchState` 上 **41 个 `pub` 字段**以及分布在 8 个不同文件里的 `impl AppState` 块，形成典型的「接口宽度逼近实现体量」的浅模块簇：

1. **同一概念的不变量跨父子结构体撕裂**：
   - `commit_stage_selected_idx` 位于 `AppState`（`app/state.rs:75`），而对应的扁平树列表 `rows` 位于 `StageWorkbenchState`（`stage_view/workbench.rs:97`）。任何修改折叠状态、目录分组或忽略文件开关的操作都必须记得先调 `sync_rows` 再裁剪 `commit_stage_selected_idx`——`app_stage/mod.rs` 为此手动重复调用了 11 次 `sync_rows`，而 `keymap/actions.rs:617, 679`、`input_stage.rs:128` 与 `views/footer.rs:591` 则直接按索引读取可能尚未同步的 `rows`。
2. **导航重置与焦点切换副作用散落重复**：
   - 在 `dispatch_log_action`（`app/dispatch.rs:564-680`）中，每次提交选择变更都手写一遍 `self.details.compare_override = None; self.details.pending_select_path = None;`，共重复 **13 次**；一旦新增跳转入口漏写，右侧变更面板就会停留在旧的跨分支对比态。
   - `compact_refs`、`show_tag_names`（挂在 `AppState`）与 `LogPresentation`（`log_view/presentation.rs`）及 `GraphViewOptions` 把同一套日志展示选项拆在 3 个结构体中，并在 `app/dispatch.rs:240-301` 手动同步。
   - 切换标签页或焦点面板时，清理行内编辑态（`log_filter.editing_text`、`branches.searching`、`stage_workbench.editing_author`）的逻辑散落在 `dispatch.rs` 与 `input_stage.rs` 多处。

本 Issue 将 8 个散落的 `impl AppState` 碎片内聚重组为四个拥有私有状态不变量与窄接口的深模块：**`CommitWorkbench`**、**`LogWorkbench`**、**`StashWorkbench`** 与 **`ConsoleWorkbench`**，将 `AppState` 收缩为薄协调器。

---

## 2. 核心任务清单

- [x] **深化 `CommitWorkbench` 模块，内聚行树同步与草稿/提交流不变量（合并 `app_stage/*` + `stage_view/*`）**：
  - 将 `commit_stage_selected_idx` 迁入 `CommitWorkbench`，并将内部 `rows: Vec<StageTreeRow>`、缓存指纹与局部暂存游标设为模块私有。
  - 在接收新 `StageStatusSnapshot`、`Amend` 预览或切换目录分组/折叠/显示忽略项时，在模块内部统一维护 `rows` 同步与选中行边界夹紧；对外仅提供已保证同步的只读切片/选中条目查询接口（如 `rows(&self) -> &[StageTreeRow]`、`selected_entry(&self) -> Option<StageEntry>`），**彻底删除外部调用方的 11+ 处手动 `sync_rows` 调用**。
  - 对外暴露窄接口：`dispatch(&mut self, action: CommitAction, ctx: &mut SharedContext)`、`on_engine_event(...)`、`flush_due(&mut self, now: Instant, effects: &mut Vec<Effect>)`、`next_deadline(&self) -> Option<Instant>`。
- [x] **深化 `LogWorkbench` 模块，消除 13 处重复重置与展示模型分裂（内聚 `log_view/*` + `log_graph` + 日志动作派发）**：
  - 将 `log_graph`、`branches`、`log_filter`、右侧 `DiffSession`/`ChangesTreeSession` 以及合并后的统一展示配置（将 `compact_refs` 与 `show_tag_names` 并入 `LogPresentation`）封装进 `LogWorkbench`。
  - 提供单一内部提交定位原语（统一处理单选/多选扩展、导航历史栈记录、清空 `compare_override` 与 `pending_select_path`、触发详情与 Diff 防抖协调），**删除 `dispatch.rs` 中重复 13 次的状态重置样板代码**。
- [x] **深化 `StashWorkbench` 与 `ConsoleWorkbench` 模块**：
  - `StashWorkbench`：封装贮藏列表选择、基于共享 `DiffCacheStore` 的 `ChangesTreeSession` / `DiffSession` 请求协调及贮藏写操作入口。
  - `ConsoleWorkbench`：封装 `console_entries`、选中条目索引与自夹紧 `ScrollViewport`（迁移原散落在 `AppState` 上的 `console_entries`、`console_selected_idx`、`console_output_scroll` 及 `console_*_step` 方法）。
- [x] **统一焦点与标签页切换的生命周期钩子，收缩 `AppState` 公开字段（`app/state.rs`, `app/dispatch.rs`）**：
  - 建立单一焦点迁移入口 `set_focus(tab, pane)`：当离开某面板或打开模态框时，自动调用原面板的 `on_blur()` 收敛行内输入态（`editing_text`、`searching`、`editing_author`、可视选区等），消除散落的清理分支。
  - 将 `AppState` 精简为顶层共享协调器（仅协调共享仓库快照、主题/配置/i18n、`DiffCacheStore`、`ModalStack`、全局通知、活动工作台路由及聚合 `next_deadline`），非共享内部状态一律降为模块私有并提供只读视图访问器供 `views/*` 渲染使用。

---

## 3. 验收标准 (Acceptance Criteria)

- [x] `cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
- [x] `AppState` 顶层公开字段数较重构前缩减 60% 以上；`StageWorkbenchState` 的 `rows` 与 `commit_stage_selected_idx` 不再允许外部模块绕过同步不变量直接写入。
- [x] 全仓不再存在散落于 `app/` 之外的顶层 `impl AppState` 碎片文件（`app_stage/*` 与 `app_git.rs` 职责分别归入对应工作台模块与共享工作流协调模块）。
- [x] `dispatch_log_action` 中 13 处重复的 `compare_override = None; pending_select_path = None;` 收敛为单一内部提交变更出口。
- [x] 四大标签页（`[1 Commit]`、`[2 Log]`、`[3 Stash]`、`[4 Console]`）的所有分屏拖拽、焦点循环（含窄屏折叠左侧分支栏时跳过 `LogBranchesTree`）、行内过滤与提交流程行为 100% 保持一致。

---

## Comments

- 2026-11-22 (agent): 基于架构审查 Candidate 04 创建本 P1 工单，将 `16a75c6` 物理拆文件后残留的 95+ 公开字段浅模块簇进一步深化为拥有私有不变量的四大工作台模块。
- 2026-11-22 (agent): 已完成重构并通过全仓测试与 `clippy -D warnings`：
  - 将 `StageWorkbenchState` 的 `status`/`rows`/`selected_idx`/`sync_rows_key` 全部降为私有并在内部 mutators（`apply_status`、`toggle_group_by_dir`、`toggle_show_ignored`、`toggle_row`、`expand_selected`、`expand_all`、`set_amend_flag`、`set_amend_preview`）中自动保持行树同步与边界夹紧，彻底消除外部全部 11+ 处手动 `sync_rows` 调用；
  - 新建 `ConsoleWorkbench`（`crates/jbgit-tui/src/console_workbench.rs`）封装 `console_entries`/`console_selected_idx`/`ScrollViewport`，并将 `compact_refs`/`show_tag_names` 并入 `LogPresentation`；
  - 将 `app_stage/*` 与 `app_git.rs` 归入 `app/stage/*` 与 `app/git_workflows.rs`（消除全部 `app/` 之外的 `impl AppState` 碎片），并将 `dispatch_log_action` 13 处重复的 `compare_override`/`pending_select_path` 重置收敛至单一提交导航出口；
  - 统一焦点与标签页迁移入口 `AppState::set_focus`/`blur_active_pane` 自动收敛各面板行内编辑态与可视选区，并将 `AppState` 顶层 `pub` 字段从 54 个收敛至 16 个（缩减 70.4%）。
