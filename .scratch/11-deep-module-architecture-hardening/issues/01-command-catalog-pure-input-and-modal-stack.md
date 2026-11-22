# 01: 统一 `CommandCatalog`、纯函数输入路由与自包含 `ModalStack` 模块

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M8 — 深模块架构治理与测试面收敛 (P0)

- **关联文件**：
  - `crates/jbgit-tui/src/keymap/{mod,actions,resolve,help}.rs`
  - `crates/jbgit-tui/src/input/{mod,keyboard,mouse,menus}.rs`
  - `crates/jbgit-tui/src/input_menus.rs`
  - `crates/jbgit-tui/src/input_stage.rs`
  - `crates/jbgit-tui/src/menus/{mod,log,branch,stage,stash}.rs`
  - `crates/jbgit-tui/src/views/footer.rs`
  - `crates/jbgit-tui/src/views/overlays/{mod,context_menu,popups}.rs`
  - `crates/jbgit-tui/src/views/dialogs.rs`
  - `crates/jbgit-tui/src/branches_popup.rs`
  - `crates/jbgit-tui/src/dialogs.rs`
  - `crates/jbgit-tui/src/hit_test.rs`
  - `crates/jbgit-tui/src/app/{action,types,state,dispatch}.rs`
  - `crates/jbgit-tui/src/tests/{footer_and_keymap_tests,context_menu_tests,branches_and_dialogs_tests}.rs`

---

## 1. 背景与目标

当前 TUI 中「用户可执行的一条命令」被拆散在 12 个浅模块与 4 套重叠枚举（`KeyAction`、`Action`、`ContextMenuAction` 及 `hit_test.rs` 中 11 个 `*Button` 枚举）里，导致极低的**局部性（Locality）**与严重的**跨接缝泄漏（Seam Leakage）**：

1. **快捷键反向抓取菜单助记符**：`keymap/actions.rs:64-106, 502-509, 640-657`（`find_action_by_mnemonic` / `branch_menu_action_by_mnemonic`）为了执行 `Checkout`、`Uncommit`、`Reword`、`DropCommit`、`RenameBranch`、`DeleteBranch` 等快捷键，先构造完整本地化 `Vec<ContextMenuItem>` 菜单树，再按硬编码英文助记符（`'c'`/`'u'`/`'w'`/`'x'`/`'e'`/`'d'`）扫描提取动作。菜单 UI 助记符变更会直接破坏键盘快捷键。
2. **快捷键提示忽略作用域遮蔽且多处硬编码作用域逻辑**：`menus/*.rs` 使用不感知高优作用域冲突的 `keymap.primary_key_for(...)`（而非 `keymap/resolve.rs::resolve_key_in_scopes`）；`input/keyboard.rs:62-78, 225-371` 在 `KeymapConfig` 之外硬编码了大量结构性回退键（`Enter`/`Esc`/`Ctrl+E`/Console `p`），迫使 `views/footer.rs:451-679` 与 `keymap/help.rs:155-282` 手写镜像的 `PaneId` 分支。
3. **输入路由破坏纯函数契约**：`route_terminal_event`（`input/mod.rs:14`）虽返回 `Vec<Action>`，却在 `route_paste`、`route_modal_key`、`route_shortcuts_help_key`、`route_file_history_key`、`route_branches_popup_key`、`route_stage_left_click`、`context_menu_for_target` 等 ~15 处直接写入 `&mut AppState`；`AppState::flush_pending_at` 反向调 `input_menus::flush_menu_hover`（`input_menus.rs:285`）并在内部执行 `let _ = state.dispatch(...)` 丢弃 `Effect`。
4. **模态栈状态分裂与重复确认框**：`ModalLayer` 同时存在 `CriticalDialog` 与 `Dialog::Choice(ChoiceDialog::confirm)` 两套确认弹窗；11 种浮窗挤在同一个 `FloatingPopup { kind: PopupKind, query, selected }` 中，把真实状态散落在 `AppState` 顶层字段（`file_history`、`conflict_merge`、`hook_error_output`、`stage_workbench.three_way_range_idx`），关闭模态时必须在 `app/dispatch.rs:110-133` 手动逐项清理。

本 Issue 将上述浅层簇深化为两个高杠杆深模块：**`CommandCatalog`** 与 **`ModalStack`**，并将终端输入路由彻底纯函数化。

---

## 2. 核心任务清单

- [ ] **构建单一深模块 `CommandCatalog`，消除助记符抓取与重复枚举（`keymap/*`, `menus/*`, `views/footer.rs`, `hit_test.rs`, `app/types.rs`）**：
  - 参考并泛化 `menus/stash.rs` 的声明式规格模式，建立统一的命令描述结构：集中定义命令 ID（`KeyAction`）、生效作用域、上下文可见性谓词、`check_enabled(&AppState) -> Result<(), DisabledReason>`、i18n 标题与助记符、Footer/Help 展示元数据，以及唯一的动作解析闭包 `resolve(&AppState) -> Vec<Action>`。
  - **删除 `find_action_by_mnemonic` 与 `branch_menu_action_by_mnemonic`**：提交与分支相关的领域动作解析（如 `Checkout`、`Uncommit`、`Reword`、`DropCommit`、`RenameBranch`、`DeleteBranch`）作为独立纯查询函数由 `CommandCatalog` 和 `menus/{log,branch}.rs` 共同调用，键盘派发不再构造或扫描 `ContextMenuItem`。
  - **删除 `ContextMenuAction` 包装枚举**（`app/types.rs:229-251`），让菜单叶子项直接持有命令规格或 `Vec<Action>`；合并 `input/menus.rs` 与 `input_menus.rs` 为单一内聚菜单模块。
  - 右键菜单右侧展示的快捷键提示统一改走 `keymap/resolve.rs::resolve_key_in_scopes(state, action)`，当快捷键在当前上下文被更高优作用域遮蔽或置为 `noop` 时不显示虚假提示。
  - 将结构性默认按键收敛进 `CommandCatalog` / 键位解析层，使 `views/footer.rs` 与 `keymap/help.rs` 直接从目录查询当前上下文生效命令，消除三处镜像 `PaneId` 分支（完整保留 `footer_max_lines` 与 `show_hint` 自定义语义）。
- [ ] **将 `route_terminal_event` 重构为严格只读纯函数（`input/*`, `branches_popup.rs`, `input_stage.rs`, `app/action.rs`, `app/dispatch.rs`）**：
  - 将路由入口签名改为只读借用：`pub fn route_terminal_event(event: &Event, state: &AppState) -> Vec<Action>`（按键序列超时缓冲 `pending_keys` 亦通过显式 `Action` 在 `dispatch` 中演进，不在路由函数内偷改 `state`）。
  - 清零全部 ~15 处路由期直接状态写入：
    - `route_paste`（过滤栏、分支搜索、弹窗输入、对话框编辑、作者输入）、弹窗列表选择/查询编辑（`BranchesPopup`、`ShortcutsHelp`、`GoToRevision`、`FilterChip` 等）、右键菜单唤起前的目标选中对齐、鼠标移动更新 `hover_pos`/`hover_target` 及拖拽状态变更，统一映射为明确 `Action` 由 `AppState::dispatch` 集中应用。
  - 修复 `flush_menu_hover`（`input_menus.rs:285`）：不再在辅助函数中私自调用 `let _ = state.dispatch(...)` 丢弃 `Effect`，统一在 `flush_pending_at` 内走标准 `dispatch` 并收集 `Effect`。
- [ ] **构建自包含深模块 `ModalStack` 并合并重复确认对话框（`app/types.rs`, `dialogs.rs`, `views/dialogs.rs`, `views/overlays/*`, `app/dispatch.rs`）**：
  - **删除 `ModalLayer::CriticalDialog` 与 `render_critical_dialog`**（`views/overlays/mod.rs:121-217`），为 `ChoiceDialog` 增加危险操作视觉令牌（`danger: bool`）与确认快捷行为，统一复用单一确认对话框实现。
  - 将 `FloatingPopup { kind: PopupKind, query, selected }` 按职责升级为强类型模态状态（如 `BranchesPopup(BranchesPopupState)`、`ShortcutsHelp(HelpPopupState)`、`FileHistory(FileHistoryState)`、`ConflictResolver(ConflictMergeState)`、`ThreeWayHunk(ThreeWayState)`、`HookError { output: String }`、`TextPrompt` 等），封装在 `ModalStack` 模块内部。
  - 将原散落在 `AppState` 顶层的模态专属旁路字段迁入对应模态变体，`ModalStack::pop` / `dismiss_context_menus` / `top_mut` 自动维护生命周期清理，删除 `app/dispatch.rs:110-133` 的手动逐字段置 `None` 逻辑。
- [ ] **接口级测试更新与替换（`tests/*`）**：
  - 新增契约测试：遍历全部 `KeyAction` 与语言包（`zh-CN` / `en-US`），验证修改任意菜单项 `mnemonic` 或切换语言后，键盘快捷键解析出的 `Vec<Action>` 100% 不变。
  - 新增纯路由测试：断言 `route_terminal_event` 对任意键盘/鼠标/粘贴事件零可变借用，且通过标准 `dispatch` 应用后与既有弹窗、右键菜单、快捷键帮助、分支浮窗行为一致。
  - 删除针对旧 `CriticalDialog` 或直接篡改旁路弹窗字段的冗余测试，统一通过 `Action` 驱动 `ModalStack` 接口断言。

---

## 3. 验收标准 (Acceptance Criteria)

- [ ] `cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
- [ ] 全仓搜索 `find_action_by_mnemonic`、`branch_menu_action_by_mnemonic`、`ContextMenuAction`、`ModalLayer::CriticalDialog` 结果为零。
- [ ] `route_terminal_event` 接收 `&AppState` 不可变引用，输入路由模块内零 `&mut AppState` 参数残留。
- [ ] 新增一条跨键盘/菜单/底栏提示的命令仅需在 `CommandCatalog`（及对应默认键位/i18n 文案）登记，无需同步手改 `footer.rs`、`help.rs` 与多套中间枚举。
- [ ] 所有模态框（含 `BranchesPopup`、`FileHistory`、`ConflictResolver`、危险操作确认框、级联右键菜单）的鼠标点击内外判定、助记符触发与 `Esc` 逐层关闭行为完全无退化。

---

## Comments

- 2026-11-22 (agent): 基于架构审查 Top Recommendation（Candidate 01）创建本 P0 工单，根治 12 文件联动散弹式修改、助记符反向抓取与输入路由非纯副作用。
- 2026-11-22 (agent): 已完成实现与验证：新增 `keymap::catalog` 统一命令解析与遮蔽感知快捷键查询，彻底删除 `find_action_by_mnemonic`、`branch_menu_action_by_mnemonic` 与 `ContextMenuAction`，合并 `input_menus.rs` 至 `input/menus.rs`；将 `route_terminal_event` 及所有键鼠/弹窗路由函数重构为只读 `&AppState` 纯函数并修复 `flush_menu_hover` 丢弃 `Effect` 问题；构建自包含 `ModalStack` 模块收拢 `FileHistory`/`ConflictResolver`/`ThreeWayInspector`/`HookError` 模态状态并彻底删除 `ModalLayer::CriticalDialog` 与 `render_critical_dialog`；全仓测试与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
