# 03: Log 变更树与分支浮窗快捷键修复、目录菜单过滤及 Diff `Ctrl+F` 补齐

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M11 — Pilot QA 缺陷修复与体验加固
Priority: P2
QA-Refs: `P2-03`, `P2-04`, `P2-08`, `P2-09`, `P2-10`

- **关联文件**：
  - `crates/jbgit-tui/src/config/defaults/keymaps/default.toml`（`[global]:20`、`[log]:60`、`[branch]:99`、`[diff]:165`）
  - `crates/jbgit-tui/src/keymap/mod.rs`（`KeyAction`、冲突检测 `presets_have_zero_unintended_keymap_shadowing_conflicts` `:1499-1617`）
  - `crates/jbgit-tui/src/keymap/catalog.rs`（`is_action_applicable`、动作派发）
  - `crates/jbgit-tui/src/menus/log.rs`（`log_change_menu_items` `:982-1250`）
  - `crates/jbgit-tui/src/branches_popup.rs`（`route_branches_popup_key` `:364-433`）
  - `crates/jbgit-tui/src/input/menus.rs`（`route_context_menu_key` `:96-168`）
  - `crates/jbgit-tui/src/input/keyboard.rs`（`route_file_history_key` `:868-883`、`route_annotate_key` `:1084-1105`、`route_reflog_key` `:1414-1416`）
  - `crates/jbgit-tui/src/i18n/locales/{zh-CN,en-US}.toml`
  - `crates/jbgit-tui/src/tests/file_history_and_changes_tests.rs`（`:166-194`）、`crates/jbgit-tui/src/tests/diff_enhancements_tests.rs`（`:770`）

---

## 1. 根因定位

1. **[`P2-03`] `LogChangesTree` 直按 `c`/`p`/`l` 无效且 `p` 误触发全局 Pull**：
   - `c`（复制相对路径）、`p`（复制文件补丁）、`l`（与本地工作区对比）仅在 `menus/log.rs:1116-1247` 中作为上下文菜单助记符（`Some('c')`/`Some('p')`/`Some('l')`）定义，未注册为 `LogChangesTree` 的直接快捷键。
   - 焦点位于 `PaneId::LogChangesTree` 时，`route_dynamic_key`（`keyboard.rs:114`）早于 `route_pane_fallback_key` 执行，按 `[KeymapScope::Log, KeymapScope::Global]` 查表：`c` 对应的 `[log] checkout` 不适用于 `LogChangesTree` 变为空操作；`l` 未绑定；而 `p` 在 `[log]` 无命中，直接穿透命中 `[global]` 的 `p -> git_pull`（`default.toml:20`），静默拉取了远程提交。
2. **[`P2-04`] 目录节点右键菜单暴露 6 个文件专属项**：
   - `menus/log.rs:1089-1247` 仅对 `show_file_history` / `annotate` 与 `show_dir_history` 做了 `if is_file` 分支，但 `toggle_diff`、`fullscreen_diff`、`compare_local`、`restore_version`、`open_editor`、`copy_patch` 以及它们之间的分隔线在 `is_file == false` 时仍被全部压入列表（仅加了 `.enabled_if(is_file, ...)`），导致目录菜单出现大片置灰死选项和空分隔块。
3. **[`P2-08`] `Git 分支速查` 浮窗列表按 `Space` 不切换收藏**：
   - `branches_popup.rs:422` 排除了空白字符进入速查搜索框（`!c.is_whitespace()`），但 `route_branches_popup_key`（`:381-432`）完全缺少 `KeyCode::Char(' ')` 分支，按键直接落入 `_ => Vec::new()`。
4. **[`P2-09`] 分支子菜单按 `F2` 不触发重命名**：
   - `branch_menu_items` 给「重命名…」附带了 `shortcut: Some("F2")`，但 `route_context_menu_key`（`input/menus.rs:108-167`）只处理了 `KeyCode::Delete`（匹配含 `"Del"` 的 shortcut）和普通字符助记符 `KeyCode::Char`，完全没有 `KeyCode::F(n)` 分支（同等影响到其他菜单中的 `F2`/`F4`）。
5. **[`P2-10`] Diff 面板 `Ctrl+F` 不打开内联搜索**：
   - `default.toml:165` `[diff]` 仅绑定了 `/` → `diff_search`，漏掉了 `<C-f>`（而 `[log]:74` 和 `[branch]:101` 均同时绑定了 `/` 与 `<C-f>`）。`FileHistory`/`Annotate`/`Reflog` 模态内嵌 Diff 视图同样只识别 `/`。

---

## 2. 任务清单

- [ ] **为 `LogChangesTree` 绑定直达文件快捷键并阻断 `p` 穿透到全局 Pull（`P2-03`）**：
  - 在 `keymap/mod.rs` 和 `keymap/catalog.rs` 新增 3 个类型化 `KeyAction`：`CopyFilePath`、`CopyFilePatch`、`CompareFileWithLocal`，`is_action_applicable` 仅在 `active_tab == WorkspaceTab::Log && focused_pane == PaneId::LogChangesTree` 时为 `true`（内部复用 `log_change_menu_items` 相同的单提交与文件行守卫，目录行按 `c` 复制目录相对路径、按 `p`/`l` 安全忽略而非穿透全局 `GitPull`）；
  - 在 `default.toml` `[log]` 段绑定 `c -> copy_file_path`、`p -> copy_file_patch`、`l -> compare_file_with_local`，并在 `menus/log.rs:log_change_menu_items` 通过 `catalog::shortcut_for_action(...)` 动态展示快捷键；
  - 补齐 `zh-CN.toml` / `en-US.toml` 的 `help.action.*` 与 `view.footer.pill.*` 条目，并在 `keymap/mod.rs:1566-1575`（`presets_have_zero_unintended_keymap_shadowing_conflicts`）为 `(PaneId::LogChangesTree, KeyAction::GitPull)` 登记显式有意遮蔽白名单。
- [ ] **目录上下文菜单移除文件专属操作与空分隔线（`P2-04`）**：
  - 在 `crates/jbgit-tui/src/menus/log.rs:1086-1249` 用 `if is_file { ... }` 包裹 `toggle_diff`、`fullscreen_diff`、`compare_local`、`restore_version`、`open_editor`、`copy_patch` 及其专属分隔线；目录行保留 `show_dir_history`、`filter_by_path`、可选 `only_affected`/`swap` 与 `copy_path`。
  - 同步更新 `crates/jbgit-tui/src/tests/file_history_and_changes_tests.rs:166-194` 断言目录行菜单不包含这些文件专属项。
- [ ] **分支速查浮窗支持 `Space` 切换收藏（`P2-08`）**：
  - 在 `crates/jbgit-tui/src/branches_popup.rs:route_branches_popup_key` 增加 `KeyCode::Char(' ') if !ctrl && !alt` 分支：当选中项为 `PopupRow::Branch(b)` 且 `b.kind != RefKind::Tag` 时派发 `Action::ToggleFavoriteRef { full, short }`；
  - 可选优化：在 `Action::ToggleFavoriteRef`（`app/git_workflows.rs:241-254`）切换收藏后，若 `BranchesPopup` 打开则按 `full_ref` 重新定位 `selected` 光标，避免切换后高亮跳位。
- [ ] **上下文菜单支持功能键 `F1`–`F12` 快捷键分发（`P2-09`）**：
  - 在 `crates/jbgit-tui/src/input/menus.rs:route_context_menu_key`（紧邻 `KeyCode::Delete`）添加 `KeyCode::F(n)` 分支（无 Ctrl/Alt/Shift 修饰），匹配当前菜单项 `i.shortcut` 等于或以 `" / "` 分隔包含 `format!("F{n}")` 的项并激活（同时修复分支子菜单 `F2` 重命名、Log 菜单 `F2` Reword 与 `F4` 全屏 Diff）。
- [ ] **为 Diff 面板及模态 Diff 视图补齐 `Ctrl+F` 内联搜索（`P2-10`）**：
  - 在 `crates/jbgit-tui/src/config/defaults/keymaps/default.toml:165` `[diff]` 增加 `{ on = [ "<C-f>" ], run = "diff_search", desc = "在差异视图内搜索文本" }`；
  - 在 `input/keyboard.rs` 的 `route_file_history_key`（`:868`）、`route_annotate_key`（`:1084`）和 `route_reflog_key`（`:1414`）中当 Diff 区域聚焦时同样接受 `Ctrl+f` 触发 `DiffSearchOpen`。

---

## 3. 测试与验收

- `keymap` 预设无冲突测试（`presets_have_zero_unintended_keymap_shadowing_conflicts`）全绿，验证 `default` / `jetbrains` / `vim` 三套预设无意外遮蔽；
- `LogChangesTree` 聚焦按键测试：选中文件按 `c` 写剪贴板相对路径、按 `l` 打开本地对比、按 `p` 复制补丁且**不**发出 `GitOp::Pull`；选中目录右键菜单仅含目录适用项；
- `BranchesPopup` 按 `Space` 切换星标收藏；打开分支子菜单按 `F2` 打开重命名弹窗；
- `[1] CommitDiffPreview` 及 `FileHistory`/`Annotate`/`Reflog` Diff 面板按 `Ctrl+f` 打开内联搜索条（扩展 `diff_enhancements_tests.rs:770`）。
