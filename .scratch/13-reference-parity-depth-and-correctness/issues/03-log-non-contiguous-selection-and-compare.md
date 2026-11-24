# 03: Log 非连续多选解锁任意两提交对比与「仅显示受影响变更」

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M10 — reference 深度对齐与正确性加固
Priority: P0

- **参考源码**：
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/table/selection/CommitSelectionImpl.kt`、`VcsLogGraphTable.java`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/actions/CompareRevisionsFromLogAction.kt`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/actions/ShowOnlyAffectedChangesAction.java`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/frame/VcsLogAsyncChangesTreeModel.kt`（`:296-313`、`ManyChanges` 保护）
- **关联文件**：
  - `crates/jbgit-tui/src/graph/state.rs`（`LogGraphState` 选区模型 `:514-516, :763-768`）
  - `crates/jbgit-tui/src/log_view/{workbench.rs,details_pane.rs,presentation.rs}`
  - `crates/jbgit-tui/src/input/{keyboard.rs,mouse.rs}`、`crates/jbgit-tui/src/menus/log.rs`（`:106-120` `Compare Versions`）
  - `crates/jbgit-tui/src/views/{log.rs,log_details/changes_browser.rs}`

---

## 1. 背景与目标

当前 `LogGraphState` 仅通过单个 `anchor: Option<usize>` 记录连续区间 `anchor..=selected_idx`，而 `menus/log.rs` 的 `Compare Versions` 要求恰好选中 2 条提交——结果用户在 Log 中**只能对比相邻两行**，无法对比任意两个不相邻的提交。此外，当启用 Path 筛选时，右侧 Changes 树依然列出选中提交的全部文件，噪音极大。本工单一次性解决 Log 选区模型与路径筛选联动短板。

## 2. 任务清单

- [ ] **非连续多选集合模型（`LogGraphState`）**：
  - 引入基于 `Oid`（或稳定 `visible_row` 集合，快照重载后按 `Oid` 恢复）的多选集合 `selected_rows: BTreeSet<usize>` + `cursor_idx: usize` + `range_anchor: Option<usize>`；
  - 交互约定（与 IDEA 及终端习惯一致）：
    - 普通上下移动 / 左键单击：单选光标行并清空其他选中；
    - `Shift+Up/Down` / `Shift+Click`：以 `range_anchor` 到目标行做连续区间选择；
    - `Ctrl+Click` / 键盘 `Space`（或在 Log 表格作用域绑定 `ToggleLogRowSelection`）：切换当前行的选中态而不清除其他已选行；
    - `Esc`（无弹窗且多选数量 > 1 时）：收起多选回到单选当前光标行；
  - 表格渲染：所有选中行高亮（光标行与非光标选中行使用可区分的焦点/次级高亮或左侧 `◆` 标记），状态栏/标题显示 `N selected`。
- [ ] **任意两提交对比与大选区护栏**：
  - 当选中数恰好为 2（无论是否相邻）时，`Compare Versions` 菜单项与快捷键生效，按拓扑/提交时间顺序将较旧者作为 `base`、较新者作为 `target` 发起双提交 Diff（并支持交换对比方向）；
  - 多选超过阈值（默认 `MAX_AGGREGATED_COMMITS = 100`，对齐 `VcsLogUtil.getShownChangesLimit()`）时，右侧 Changes 面板显示「已选 N 个提交，按 Enter 确认加载聚合变更」占位提示，防止误选数千提交卡死聚合。
- [ ] **「Show Only Affected Changes」开关**：
  - 在 `LogPresentation` 新增 `show_only_affected_changes: bool`（持久化到 `jbgit.toml` `[log]`，默认 `true` 或可由按钮/快捷键切换）；
  - 当 `PathFilter` 激活且开启该开关时，右侧 `LogChangesTree` 自动过滤仅显示命中当前路径筛选的变更文件，并在 Changes 面板标题栏显示 `[Filtered: M/N]` 徽标及一键点击切换全部/受影响文件。

## 3. 测试

- **单元测试**：`LogGraphState` 单选、Shift 连续选、Ctrl+Click / Space 非连续勾选、取消勾选、`Esc` 收起、以及快照重载后按 `Oid` 保活选区的完整状态机测试。
- **TUI 会话测试**：
  - 在 5 提交仓库中选中第 0 行与第 3 行（跳过第 1、2 行）→ 触发 `Compare Versions` → 验证 Diff 展示第 3 行到第 0 行的净差异；
  - 激活 `PathFilter("src/a.rs")` 并选中修改了 `src/a.rs` 与 `src/b.rs` 的提交 → 验证 Changes 树在开关开启时仅显示 `src/a.rs`，关闭时显示全部两文件。
