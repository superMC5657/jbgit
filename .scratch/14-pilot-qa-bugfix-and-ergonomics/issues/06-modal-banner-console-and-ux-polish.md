# 06: 弹窗校验时机、Welcome/Clone 状态去重、横幅/控制台排版与输入/菜单/回滚文案打磨

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M11 — Pilot QA 缺陷修复与体验加固
Priority: P3
QA-Refs: `P3-01`, `P3-02`, `P3-03`, `P3-04`

- **关联文件**：
  - `crates/jbgit-tui/src/dialogs.rs`（`InputDialog` `:188-217, 2314-2357`、`IdentityDialog` `:1146-1199, 2163-2216`）
  - `crates/jbgit-tui/src/views/dialogs.rs`（`render_identity` `:420-565`、`render_input` `:1367-1402`）
  - `crates/jbgit-tui/src/app/git_workflows.rs`（`Action::DialogConfirm` `:174-218`）
  - `crates/jbgit-tui/src/app/dispatch.rs`（粘贴与点击处理 `:269-318, 517-522`）
  - `crates/jbgit-tui/src/views/header.rs`（`:154-166`）、`crates/jbgit-tui/src/views/log.rs`（`:694-703`）
  - `crates/jbgit-tui/src/app/events.rs`（`UiAsyncEvent::CloneFinished` `:76-109`）、`crates/jbgit-tui/src/repo_lifecycle.rs`（`:650, 827-869`）
  - `crates/jbgit-tui/src/views/banner.rs`（`:128-135, 175-176`）、`crates/jbgit-tui/src/views/console.rs`（`:170-205`）
  - `crates/jbgit-tui/src/menus/branch.rs`（`:73, 122-237`）、`crates/jbgit-tui/src/tests/context_menu_tests.rs`（`:648-667`）
  - `crates/jbgit-tui/src/stage_view/workbench.rs`（`:1226-1287, 2294-2303, 2392-2401, 2449-2514`）
  - `crates/jbgit-tui/src/i18n/locales/{zh-CN,en-US}.toml`（`:1437`）

---

## 1. 根因定位

1. **[`P3-01`] Git 身份弹窗打开即报红 `✕ 用户名不能为空`**：
   - `IdentityDialog`（`dialogs.rs:1147-1185`）缺少 `validation_shown` 状态位，`render_identity`（`views/dialogs.rs:431, 509-518`）每次渲染直接调用 `d.error(i)` 并在空字符串时立即显示红色错误行。
2. **[`P3-02`] Welcome 背景残留加载文案 & Clone 报错重复输出两行**：
   - 非 Git 目录启动时 `init_engine_and_session`（`session.rs:910-915`）令 `repo_snapshot: None` 且 `repo_error: None`，导致 `views/header.rs:154-166` 掉入 `else` 渲染 `加载中…`，`views/log.rs:694-703` 无条件渲染 `正在加载提交图谱…`；
   - `clone_repository_async` 先把 `fatal: ...` 通过 `CloneProgress` 推进 `cd.progress_lines`，退出非零后再通过 `CloneFinished(Err)` 写入 `cd.error`；`render_clone_repo_modal`（`repo_lifecycle.rs:827-869`）先以 1 空格缩进渲染 `cd.error`，紧接着又以 2 空格缩进渲染 `cd.progress_lines`，导致同一报错打印两遍。
3. **[`P3-03`] Bisect 按钮缺尾空格 & Console 失败行粘连与 `ms` 被截断**：
   - `views/banner.rs:129` 把含首尾空格的 `lbl`（如 `" Good "`）拼成 `format!("{lbl} {k}")`，得到 `"[ Good  Alt+Shift+G]"`（右括号前缺空格）；
   - `views/console.rs:176` 对非零退出码写成 `format!(" ✗{code}")` 漏了尾部空格，导致 `✗2git ...` 粘连；同时 `:184` 计算预留宽度硬编码 `10`，而 `:189` 渲染的 `" [HH:MM:SS]"` 实际占 `11` 列（差 1 列），导致长命令截断时把行尾 `ms` 的 `'s'` 裁掉（显示 `53m` / `59m`）。
4. **[`P3-04`] 三项 Commit/Branch 交互细节**：
   - **(a) 单行弹窗输入框不支持 `Ctrl+a` 全选**：`InputDialog`（`dialogs.rs:188, 2314`）及同类弹窗直接操作 `String`，未处理 `Ctrl+a`；
   - **(b) 当前分支子菜单列出指向自身的对比/变基/合并**：`menus/branch.rs:122-237` 对 `is_current` 仅调用 `.enabled_if(!is_current, ...)` 而未省略，导致菜单中出现 `与 '<self>' 对比`、`将 '<self>' 变基到 '<self>'`、`将 '<self>' 合并到 '<self>'`；
   - **(c) 回滚整块 Hunk 提示文案仍写「选中行」**：`StageDiffRollbackSelection`（`workbench.rs:2294`）在非 visual 模式下回退到整块 hunk，`ApplyStageDiffHunk(_, Rollback)`（`:2392`）亦然，但 `apply_stage_diff_selection`（`:2508`）统一输出 `toast.rolled_back_lines`（`已回滚 {path} 中的选中行`）。

---

## 2. 任务清单

- [ ] **延迟 `IdentityDialog` 校验报错展示（`P3-01`）**：
  - 在 `IdentityDialog` 增加 `pub validation_shown: bool`（默认 `false`）；
  - 在 `Action::DialogConfirm`（`app/git_workflows.rs:178-217`）校验不通过时将 `validation_shown = true`（亦可在切换字段失焦或编辑输入后置 `true`）；
  - `views/dialogs.rs:509-518` 仅在 `d.validation_shown && err.is_some()` 时显示红色 `✕ {e}`，此前留空或显示柔和引导文案。
- [ ] **清理 Welcome 非 Git 背景占位文案 & 去重 Clone 失败日志（`P3-02`）**：
  - 在 `AppState` 新增 `is_unattached_workspace(&self) -> bool`（`self.repo_error.is_some() || self.modal_stack.iter().any(|l| matches!(l, ModalLayer::Welcome(_)))`），供 `views/header.rs:154`（显示 `无仓库`）和 `views/log.rs:694-703`（抑制 `正在加载提交图谱…`）使用；
  - 在 `app/events.rs:97-108` 收到 `UiAsyncEvent::CloneFinished(Err(err))` 时，从 `cd.progress_lines` 剔除与 `err` 各非空行重复的条目，避免报错双份渲染。
- [ ] **修正 Bisect 按钮间距与 Console 失败行宽度计算（`P3-03`）**：
  - `views/banner.rs:129`：`Some(k) => format!(" {} {k} ", lbl.trim())`（或 `format!("{lbl} {k} ")`），保证 `[` 后与 `]` 前空格对称；
  - `views/console.rs:176`：改为 `format!(" ✗{code} ")`；
  - `views/console.rs:184-191`：先构造 `let ts = format!(" [{}]", entry.timestamp_label());`，用 `ts.width()` 替代魔数 `10` 计算 `reserved`。
- [ ] **单行输入框 `Ctrl+a` 全选、隐藏自身分支自指向菜单项与区分 Hunk 回滚提示（`P3-04`）**：
  - 为 `InputDialog`（并按需复用到 `NewBranch`/`Identity`/`Credential`/`StashPush`/`CloneDialogState`）增加 `selected_all: bool`：`Ctrl+a` 选中全文并在渲染时用 `bg_selection_focused` 高亮；全选态下按 `Backspace`/`Delete` 清空，输入或粘贴替换全文并取消全选；
  - 在 `menus/branch.rs:122-237` 将 `checkout_and_rebase`、`compare_with`、`rebase_onto`、`merge_into`、`merge_options_item` 改为 `if !is_current { ... }` 省略（注意不留多余分隔线），并同步更新 `crates/jbgit-tui/src/tests/context_menu_tests.rs:648-667`；
  - 在 `zh-CN.toml` / `en-US.toml:1437` 新增 `toast.rolled_back_hunk`（`已回滚 {path} 中的选中代码块 (Ctrl+Z 撤销)` / `Rolled back hunk in {path} (Ctrl+Z to undo)`），在 `workbench.rs:2300, 2398, 2506` 区分整块回滚（非 visual 模式 `d` 或 hunk 按钮）与选区行回滚，注意在清除 `visual_anchor_row` 之前判定 `whole_hunk`。

---

## 3. 测试与验收

- `IdentityDialog` 渲染测试：初开无 `✕` 报错，按 `Enter` 提交空表单后显示 `✕`；
- `Welcome` 渲染测试：非 Git 目录下背景不出现 `加载中…` / `正在加载提交图谱…`；`CloneFinished(Err)` 渲染测试：报错行仅出现一次；
- `Bisect` banner 与 `Console` 失败行 `TestBackend` 测试：断言 `[ Good Alt+Shift+G ]` / `[ 跳过 Alt+Shift+S ]` 尾空格对称、`✗1 git ...` 含分隔空格且行尾完整保留 `ms`；
- `InputDialog` 按 `Ctrl+a` + `Backspace` 清空全部内容 / `Ctrl+a` + 输入替换全文；当前分支子菜单不含自指向操作；Diff 非 visual 模式按 `d` 回滚提示 `选中代码块`，visual 模式选行按 `d` 回滚提示 `选中行`。
