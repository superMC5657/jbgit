# 02: `[1] 提交` 初始文件选中对齐与弹窗文本输入框 `Ctrl+a` 全选统一

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M12 — 工作流正确性与 UI 交互加固
Priority: P2

- **关联文件**：
  - `crates/jbgit-tui/src/stage_view/workbench.rs`（`apply_status` `:642-662`、`toggle_group_by_dir` `:673-677`）
  - `crates/jbgit-tui/src/log_view/push_dialog.rs`（`PushDialogState` `:24-51` 及目标分支编辑方法）
  - `crates/jbgit-tui/src/input/keyboard.rs`（`route_push_dialog_key` `:1271-1292`）
  - `crates/jbgit-tui/src/app/action.rs` / `crates/jbgit-tui/src/app/dispatch.rs`（`PushEditTarget*` 动作）
  - `crates/jbgit-tui/src/views/push_dialog.rs`（目标分支全选态高亮渲染）
  - `crates/jbgit-tui/src/dialogs.rs`（`Dialog::{Merge, Pull, Rebase, CreatePatch}` 文本行按键处理 `:2506-2715`）

---

## 1. 根因定位

### 1.1 进入 `[1] 提交` 后默认选中分组头而非文件行，导致 `.` 菜单与右侧 Diff 预览不一致
- 在 `crates/jbgit-tui/src/stage_view/workbench.rs:656-660` 中：
  ```rust
  if !old_rows.is_empty() {
      self.selected_idx = self.smooth_anchor_after_rows_update(&old_rows, old_sel);
  } else {
      self.selected_idx = 0;
  }
  ```
- 首次加载变更状态（以及 `toggle_group_by_dir` `:676`）时，`self.selected_idx` 被固定置为 `0`，而第 0 行始终是分组头 `StageTreeRowKind::Group`（如 ` 已暂存 (1)`）。
- 同时 `desired_diff_key`（`stage_view/workbench.rs:1098`）在选中 `Group` 行时会自动回退预览该分组下的第一个文件（如 `staged_partial.txt (已暂存 ↔ HEAD)`）。右侧已经在预览具体文件，但此时按 `.` 打开的是分组头上下文菜单，文件专属操作（`三方差异对比`、`查看差异`、`显示文件历史…`）全部灰显并提示 `请选择一个文件`，造成左右面板认知割裂。

### 1.2 `PushDialog` 目标分支编辑框及 4 个弹窗内嵌文本行缺失 `Ctrl+a` 全选
- 通用弹窗（`InputDialog`、`IdentityDialog`、`NewBranchDialog`、`NewTagDialog`）已支持 `selected_all` + `Ctrl+a` 全选后覆盖或清空，但以下 5 处独立输入框尚未覆盖：
  - `PushDialog` 目标分支编辑框（`P` → `e`）：`crates/jbgit-tui/src/log_view/push_dialog.rs:24-31` 中 `PushDialogState` 缺少 `target_selected_all: bool`，且 `crates/jbgit-tui/src/input/keyboard.rs:1279-1291` 仅处理了 `Ctrl+u`（`PushEditTargetClear`），`Ctrl+a` 被吞掉导致 `Ctrl+a Backspace` 仅删除末尾 1 个字符；
  - `crates/jbgit-tui/src/dialogs.rs` 中的 4 个内嵌文本输入行（`Dialog::Merge` 的 `Message` `:2526`、`Dialog::Pull` 的 `Branch` `:2589`、`Dialog::Rebase` 的文本字段 `:2642`、`Dialog::CreatePatch` 的路径字段 `:2701`）同样仅处理了 `Ctrl+u`，缺失 `Ctrl+a` 全选语义。

---

## 2. 任务清单

- [x] **优化 `[1] 提交` 首次加载时的初始选中行**：
  - 在 `crates/jbgit-tui/src/stage_view/workbench.rs` 新增辅助方法 `first_file_row_idx(&self) -> usize`（返回 `self.rows` 中首个 `StageTreeRowKind::File | StageTreeRowKind::AmendFile` 的索引，若无则回退 `0`）；
  - 在 `apply_status`（`:659`，`old_rows.is_empty()` 分支）与 `toggle_group_by_dir`（`:676`）中改用 `self.first_file_row_idx()`（检查 `#[cfg(test)] pub fn set_status` 及现有测试的兼容预期）；
  - 确认进入 `[1] 提交` 后，左侧变更树高亮落在首条文件上，与右侧 Diff 预览一致，直接按 `.` 即可使用 `查看差异`、`三方差异对比 (HEAD ↔ 已暂存 ↔ 工作区)`、`显示文件历史…`。
- [x] **为 `PushDialog` 目标分支编辑补齐 `Ctrl+a` 全选**：
  - 在 `crates/jbgit-tui/src/log_view/push_dialog.rs` 的 `PushDialogState` 增加 `pub target_selected_all: bool`（进入/取消/确认编辑时复位为 `false`）；
  - 新增 `Action::PushEditTargetSelectAll` 并在 `input/keyboard.rs:1279-1291` 绑定 `KeyCode::Char('a') if ctrl`；
  - 在 `PushEditTargetBackspace`、`PushEditTargetChar`、`PushEditTargetClear`、`AcceptCompletion` 与 `ModalPaste` 中统一实现全选清空/覆盖语义；
  - 在 `crates/jbgit-tui/src/views/push_dialog.rs` 中，当 `editing_target && target_selected_all` 且 `target_draft` 非空时使用 `bg_selection_focused` / `fg_bright` 高亮视觉反馈。
- [x] **补齐 `Dialog::{Merge, Pull, Rebase, CreatePatch}` 内嵌文本行的 `Ctrl+a` 全选**：
  - 为上述 4 个弹窗的文本编辑态补齐 `selected_all: bool`（切换行/失焦时自动复位，`Ctrl+a` 置真，`Backspace` 清空，输入新字符或粘贴先清空再写入），保持全应用单行弹窗输入行为一致。

---

## 3. 测试与验收

- **单元测试（修复前必挂）**：
  - `stage_view`: 构造含 `Staged` 分组头 + `staged_partial.txt` 的 `StageStatusSnapshot`，首次调用 `apply_status` 后断言 `selected_idx` 落在 `staged_partial.txt` 文件行而非索引 `0`，且 `stage_tree_menu_items(state, state.stage_workbench.selected_idx())` 中 `three_side_diff` / `show_diff` / `file_history` 全部 `is_enabled()`；
  - `push_dialog`: 打开 `PushDialog` → `e` 进入目标编辑（初始 `main`）→ `Ctrl+a` → `Backspace` 断言 `target_draft == ""`；再次 `e` → `Ctrl+a` → 输入 `"release"` 断言 `target_draft == "release"`；
  - `dialogs`: 对 `MergeDialog`（`Message` 行）、`PullDialog`（`Branch` 行）、`RebaseDialog`（文本行）、`CreatePatchDialog`（路径行）验证 `Ctrl+a` + `Backspace` 清空与 `Ctrl+a` + 输入替换。
- **Pilot 定向复验**：
  - `full` 沙盒按 `1` 后直接按 `.`，确认 `三方差异对比 (HEAD ↔ 已暂存 ↔ 工作区)` 可用且可打开三方检视弹窗；
  - 按 `P` → `e` → `Ctrl+a` → `Backspace` 确认目标分支输入框整行清空。

## Comments

- **2026-11-25**: 已修复并验证：
  - `stage_view`: 新增 `first_file_row_idx()`，在 `apply_status` 首次载入及 `toggle_group_by_dir` 中默认选中首个文件行。
  - `push_dialog` / `dialogs`: 为 `PushDialogState`（目标分支编辑）及 `MergeDialog`、`PullDialog`、`RebaseDialog`、`CreatePatchDialog` 内嵌文本行统一补齐 `Ctrl+a` 全选、退格清空、输入/粘贴覆盖及全选高亮渲染。
  - **测试**：`stage_view_initial_apply_status_selects_first_file_row_for_context_menu`、`push_dialog_target_edit_supports_ctrl_a_select_all_and_replace`、`inline_option_dialogs_support_ctrl_a_select_all` 及全量工作区测试均通过。
  - **Pilot 实机复验（`full` 沙盒）**：按 `1` 进入 `[1] 提交` 后 `inspect-row` 确认高亮落在第 5 行 `staged_partial.txt`（`bg=#2E436E`），直接按 `.` 显示可用菜单项并按 `3` 成功打开 `三方变更检视 (工作区 / 暂存区 / HEAD)`；按 `P` → `e` → `Ctrl+a` → `Backspace` 将 `: main▏` 整行清空为 `: ▏`。
