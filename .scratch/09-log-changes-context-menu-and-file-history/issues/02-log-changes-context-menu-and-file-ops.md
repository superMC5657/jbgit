# 02: 变更文件右键/键盘上下文菜单与文件级操作（对齐 IDEA `VcsLogChangesBrowser`）

Status: resolved
Blocked by: 01
Spec: [../spec.md](../spec.md)
Milestone: M6+ — 变更文件菜单与单文件历史

- **参考源码**：
  - `reference/platform/vcs-log/impl/resources/intellij.platform.vcs.log.impl.xml`（`Vcs.Log.ChangesBrowser.Popup`）
  - `reference/platform/vcs-impl/resources/META-INF/VcsActions.xml`（`Vcs.RepositoryChangesBrowserMenu`）
- **关联文件**：
  - `crates/jbgit-tui/src/app.rs`
  - `crates/jbgit-tui/src/app_git.rs`
  - `crates/jbgit-tui/src/log_view.rs`
  - `crates/jbgit-tui/src/input.rs`
  - `crates/jbgit-tui/src/input_menus.rs`
  - `crates/jbgit-tui/src/menus/log.rs`
  - `crates/jbgit-tui/src/menus/stage.rs`
  - `crates/jbgit-tui/src/menus/mod.rs`
  - `crates/jbgit-tui/src/keymap/mod.rs`、`keymap/actions.rs`
  - `crates/jbgit-tui/src/config/defaults/keymap.default.toml`
  - `crates/jbgit-tui/src/i18n/locales/zh-CN.toml`、`en-US.toml`

---

## 1. 背景与目标

目前 `[2 Log]` 右上「变更文件」面板（`PaneId::LogChangesTree`）尚未接入上下文菜单：右键点击文件/目录行或聚焦后按 `.` / `Shift+F10` / `Menu` 均无响应，导致无法像 IDEA 一样在变更树上直接查看文件历史、按路径筛选日志、与本地对比单文件、恢复该提交版本文件或复制补丁/路径。

本 Issue 补齐变更文件上下文菜单与全套文件级操作（并在 `[1 Commit]` 暂存树菜单复用「查看文件历史」入口），同时保持对用户已完成的工具栏 `⊞ 目录` / `⊕` / `⊖` 逻辑零改动。

---

## 2. 核心任务清单

- [x] **在 `input_menus.rs` 接入 `LogChangesTree` 鼠标右键与键盘菜单路由**：
  - `context_menu_for_target` 新增分支：
    - `HitTarget::LogChangeRow { idx }`：若未选中该行则先更新 `state.details.changes_selected = *idx` 并派发 `Action::FocusPane(PaneId::LogChangesTree)`，随后返回 `menus::log_change_menu_items(state)`。
    - `HitTarget::Pane(PaneId::LogChangesTree)`：聚焦 `PaneId::LogChangesTree`，若变更树非空则返回 `menus::log_change_menu_items(state)`。
  - `keyboard_menu_target` 新增 `PaneId::LogChangesTree` 分支：查找 `HitTarget::LogChangeRow { idx }`（`idx == state.details.changes_selected`）锚定弹窗位置；若当前行滚出视口则回退到 `HitTarget::Pane(PaneId::LogChangesTree)`。
- [x] **在 `crates/jbgit-tui/src/menus/log.rs` 实现 `pub fn log_change_menu_items(state: &AppState) -> Vec<ContextMenuItem>`**：
  - 解析当前选中行 `ChangeTreeRow`（区分 `ChangeRowKind::File { change_idx }` 与 `ChangeRowKind::Dir { .. }`）及当前 `DetailsKey`（单提交 / 多提交 / `CompareVersions` / `CompareWithLocal`）：
  - **查看与对比组**：
    - `查看差异`（`d`，文件行启用，触发 `Action::ToggleDiffPreview`）；
    - `全屏查看差异`（`z`，显示 `F4` 快捷键，文件行启用，触发 `Action::ZoomDiff(true)`）；
    - `与本地工作区对比`（`l`，文件行且单提交模式 `DetailsMode::Commits && oids.len() == 1` 启用，触发 `Action::CompareFileWithLocal { target, path }`）。
  - **历史与筛选组**：
    - `查看文件历史…`（`h`，显示动态快捷键 `KeyAction::FileHistory`）：
      - 文件行：触发 `Action::OpenFileHistory { path, start_oid, follow_renames: true }`（`start_oid` 在单提交模式下取该提交 OID，否则 `None`）；
      - 目录行：文案切换为「查看目录历史（按目录筛选日志）」，触发 `Action::FilterLogByPath(dir_key)`（对齐 IDEA `VcsLogDirectoryHistoryProvider`）。
    - `按此路径筛选日志`（`f`，文件行与目录行均启用）：触发 `Action::FilterLogByPath(path_or_dir)`。
  - **恢复与编辑组**：
    - `从此版本恢复文件…`（`g`，仅单提交模式且文件状态非 `FileStatusKind::Deleted` 且无进行中操作时启用）：弹出 `ChoiceDialog::confirm` 二次确认框（展示提交短哈希与文件路径），确认后派发 `GitOp::CheckoutPaths { rev: oid.to_string(), paths: vec![path] }`。
    - `在 $EDITOR 中打开`（`e`，文件行且工作区中该相对路径文件存在时启用，否则置灰提示“工作区中不存在该文件”）：复用 `Action::OpenExternalEditorFile(Some(path))`。
  - **剪贴板组**：
    - `复制相对路径`（`c`，文件/目录行均启用）：`Action::CopyText(path)`。
    - `复制该文件补丁`（`p`，仅单提交模式文件行启用）：`GitOp::PatchToClipboard { args: vec!["show", "--format=", "--patch", oid, "--", path] }`。
- [x] **实现核心 Action 派发与单文件 `CompareWithLocal` 自动定位**：
  - `Action::FilterLogByPath(String)`：将 `self.log_filter.spec.paths = PathFilter::parse(&path)`（注意对含空格路径做好精确保留，必要时扩展 `PathFilter::single(path)` 避免空格被拆成两条路径！），重置 `path_matches`，触发 `Effect::RequestPathMatches` 并弹出轻量 Toast 提示已按路径筛选。
  - `Action::CompareFileWithLocal { target: Oid, path: String }`：
    - 在 `DetailsPaneState` 新增 `pub pending_select_path: Option<String>`；
    - 复用 `CompareWithLocal` 逻辑开启 `diff_preview = true` 并设置 `compare_override = Some(DetailsKey::compare_with_local(target))` 与 `pending_select_path = Some(path)`；
    - 在 `EngineEvent::CommitDetailsLoaded`（以及命中 `details.cache` 同步切换时）检查 `pending_select_path`：自动展开包含该路径的目录并在 `change_rows()` 中将 `changes_selected` 定位至该文件行；若该文件在工作区与该提交完全一致（不在差异列表中），弹出友好 Toast 提示“该文件与本地工作区无差异”并清空 `pending_select_path`。
  - `Action::OpenFileHistory { path: String, start_oid: Option<Oid>, follow_renames: bool }`：在 `app.rs` / `app_git.rs` 声明并由 Issue 03 完整驱动视图打开（本 Issue 先将 Action 与 `Effect::RequestFileHistory` 管线声明就绪，或与 Issue 03 协调无 dead code 告警）。
- [x] **在 `[1 Commit]` 暂存树右键菜单补齐「查看文件历史…」入口（`crates/jbgit-tui/src/menus/stage.rs`）**：
  - 在 `stage_tree_menu_items` 中，对已跟踪文件（`file_path.is_some()` 且 `!untracked` 且非 `Ignored`）追加「查看文件历史…」（助记符 `h`），触发 `Action::OpenFileHistory { path, start_oid: None, follow_renames: true }`。
- [x] **动态键位与中英 i18n 补齐**：
  - `crates/jbgit-tui/src/keymap/mod.rs` & `actions.rs` 新增 `KeyAction::FileHistory`（字符串名 `"file_history"`），在 `keymap.default.toml` 的 `[log]` / `[commit]` 区块绑定默认键位（如 `H`），作用域覆盖 `PaneId::LogChangesTree`（选中文件行打开文件历史，目录行筛选日志）与 `PaneId::CommitChangesTree`（选中已跟踪文件打开文件历史）。
  - 在 `zh-CN.toml` 与 `en-US.toml` 补齐所有新增菜单项、确认弹窗、禁用原因、Toast 与快捷键描述文案（遵循仓库现有 key 命名风格 `menu.log_change.*` / `toast.*` / `keymap.action.file_history`）。
- [x] **补充单测与交互测试**：
  - 验证在 `HitTarget::LogChangeRow { idx }` 右键可弹出完整菜单并自动聚焦/选中对应行；
  - 验证聚焦 `PaneId::LogChangesTree` 后按 `.` 弹出菜单，且文件行与目录行呈现差异化启用/禁用状态；
  - 验证 `PathFilter::single("dir with space/a.rs")` 不因空格错误拆分，且 `Action::FilterLogByPath` 正确触发 `Effect::RequestPathMatches`；
  - 验证 `CompareFileWithLocal` 收到对应 `CommitDetailsLoaded` 后自动把 `changes_selected` 定位到目标文件行，未命中时显示 Toast；
  - 验证 `zh-CN` / `en-US` i18n key 完整性测试通过。

---

## 3. 验收标准 (Acceptance Criteria)

- [x] `cargo test --workspace` 与 `cargo clippy --workspace --all-targets` 全部通过。
- [x] 在 `[2 Log]` 变更文件树上右键或按 `.` / `Shift+F10` 可正常弹出上下文菜单，且不破坏现有工具栏 `⊞ 目录` / `⊕` / `⊖` / `◉ 差异` 行为。
- [x] 「按此路径筛选日志」「与本地工作区对比（自动定位目标文件）」「从此版本恢复文件（带二次确认）」「复制相对路径」「复制该文件补丁」「在 `$EDITOR` 中打开」全部可用且边界状态禁用提示清晰。
- [x] `[1 Commit]` 暂存树右键菜单对已跟踪文件显示「查看文件历史…」，对未跟踪文件正确置灰/隐藏。

---

## Comments

- 2026-11-20 (agent): 创建 Issue 02，依赖 Issue 01 提供的 `GitOp::CheckoutPaths` 与 `FileHistoryKey` 定义。
- 2026-11-20 (agent): 已完成实现并验证：`input_menus.rs`（`LogChangeRow`/`LogChangesTree` 右键及键盘 `.` 菜单）、`menus/log.rs`（`log_change_menu_items` 含查看/对比/文件历史/目录历史/按路径筛选/`Get Version` 二次确认/`$EDITOR`/复制路径与补丁及精细禁用理由）、`app_git.rs` & `log_view.rs`（`FilterLogByPath`、`CompareFileWithLocal` 自动展开与定位）、`menus/stage.rs` & `keymap/`（`KeyAction::FileHistory` 绑定 `H`、`menu.stage.file_history`）、中英 i18n 文案与 `spec09_tests.rs` 单测；`cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
