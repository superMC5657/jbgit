# 03: 单文件历史双分屏交互视图与联动 Diff 检视器

Status: resolved
Blocked by: 01, 02
Spec: [../spec.md](../spec.md)
Milestone: M6+ — 变更文件菜单与单文件历史

- **参考源码**：
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/history/VcsLogFileHistoryUiProvider.kt`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/history/FileHistoryFilterUi.kt`
- **关联文件**：
  - `crates/jbgit-tui/src/app.rs`
  - `crates/jbgit-tui/src/app_git.rs`
  - `crates/jbgit-tui/src/hit_test.rs`
  - `crates/jbgit-tui/src/input.rs`
  - `crates/jbgit-tui/src/input_menus.rs`
  - `crates/jbgit-tui/src/views/file_history.rs`（新建）
  - `crates/jbgit-tui/src/views/log_details.rs`（复用语法高亮 Diff 渲染辅助函数）
  - `crates/jbgit-tui/src/views/overlays.rs`
  - `crates/jbgit-tui/src/views/mod.rs`
  - `crates/jbgit-tui/src/i18n/locales/zh-CN.toml`、`en-US.toml`

---

## 1. 背景与目标

在完成 Issue 01（`--follow` 单文件历史引擎）与 Issue 02（变更文件右键菜单与入口动作）后，本 Issue 实现对标 IDEA `VcsLogFileHistoryUi` 的单文件历史交互视图：用户从 `[2 Log]` 变更文件树或 `[1 Commit]` 暂存树触发「查看文件历史」后，可在一个高信息密度的双分屏视图中流畅浏览该文件的历次提交（含重命名链路）、实时联动查看该文件在对应提交的语法高亮 Diff、与本地工作区或另一历史版本对比，并支持一键定位回主日志或按 `Esc` 无损返回原界面。

---

## 2. 核心任务清单

- [x] **设计并接入 `FileHistoryState` 与 `PopupKind::FileHistory`（`crates/jbgit-tui/src/app.rs` / `app_git.rs`）**：
  - 在 `PopupKind` 新增 `FileHistory` 变体；
  - 在 `AppState` 新增 `pub file_history: Option<FileHistoryState>`，包含：
    - `key: FileHistoryKey`、`bundle: Option<Arc<FileHistoryBundle>>`、`loading: bool`、`error: Option<String>`
    - `selected: usize`、`anchor: Option<usize>`（用于 `Shift+Up/Down` 或标定双提交对比）
    - `compare_with_local: bool`、`diff_side_by_side: bool`、`diff_context: DiffContext`、`diff_scroll: usize`、`diff_zoomed: bool`
    - `focus: FileHistoryFocus`（`Commits` / `Diff`）
    - `pending_diff_key: Option<FileDiffKey>` 与 `diff_debounce_deadline: Option<Instant>`（30ms 防抖，复用 `details.diff_cache` 与 `details.diff_syntax` 缓存，避免重复解析高亮）。
  - 在 `ScrollPane` 与 `ViewportScrollMetrics`（Spec 08）新增 `ScrollPane::FileHistoryDiff` 字段与 `scroll_pane` / `clamp_scroll_metrics` 分支，确保文件历史 Diff 视图同样具备零 Overscroll 双端夹紧能力。
- [x] **打通异步加载与 30ms 防抖 Diff 联动**：
  - `Action::OpenFileHistory { path, start_oid, follow_renames }`：初始化 `FileHistoryState`、推入 `ModalLayer::FloatingPopup { kind: PopupKind::FileHistory, .. }` 并派发 `Effect::RequestFileHistory(key)`。
  - 收到 `EngineEvent::FileHistoryLoaded { bundle, epoch }`：校验 `latest_file_history_epoch`，填充 `bundle`；若存在 `start_oid` 则默认选中匹配该 OID 的行（否则选中第 0 行），立即调度该行对应单文件 Diff 加载。
  - 光标在历史提交列表上下移动时，计算目标 `FileDiffKey`：
    - 默认模式：`DetailsKey::single(entry.oid)` + `path: entry.path_at_commit`；
    - 开启 `compare_with_local`：`DetailsKey::compare_with_local(entry.oid)` + `path: entry.path_at_commit`；
    - 存在 `anchor`（选中 2 个不同提交）：按时间序构造 `DetailsKey::compare_versions(older.oid, newer.oid)` + `path: newer.path_at_commit`；
    - 命中 `details.diff_cache` 时 0ms 瞬时渲染，未命中时 30ms 防抖派发 `Effect::RequestFileDiff`。
- [x] **实现 `crates/jbgit-tui/src/views/file_history.rs` 双分屏渲染与命中注册**：
  - 浮窗占满终端约 `92% × 88%`（窄屏自动收缩并保留安全边距），背景遮罩清晰区分底层工作台。
  - **顶栏标题与可点击工具条**：
    - 展示 `文件历史 · <path>`、提交数统计（若 `truncated` 显示 `500+` 提示）；
    - 复用统一 `views::render_toolbar` 渲染可点击按钮（带悬停/激活态）：
      - `[跟随重命名 (f)]`（切换 `key.follow_renames` 并重新请求历史）
      - `[对比本地 (l)]`（切换 `compare_with_local`）
      - `[并排/单栏 (S)]`、`[上下文 (w)]`、`[全屏 (z)]`
      - `[在日志中定位 (g)]`（关闭浮窗并跳转主日志该提交）
  - **自适应双分屏内容区**：
    - 宽屏（`width >= 100`）采用左右双栏（左 42% 提交列表 / 右 58% Diff 预览）；窄屏采用上下双栏；`diff_zoomed == true` 时 Diff 占满浮窗内容区。
    - **提交列表区**：展示短哈希、文件状态色徽标（`A`/`M`/`D`/`R`）、提交说明、右对齐 `+N -M`（或 `bin`）、作者与日期；若 `entry.old_path.is_some()` 或 `entry.path_at_commit != key.path`，渲染重命名/路径变更标记（如 `← old_path` 或 `@ path_at_commit`）。
    - **Diff 预览区**：提取并复用 `views/log_details.rs` 的 `syntect` 语法高亮与 Unified / Side-by-Side 渲染逻辑（无代码复制），写入 `ctx.record_scroll_bound(ScrollPane::FileHistoryDiff, visible_h, max_scroll)`；无文本差异/二进制文件/加载中状态展示友好提示。
  - 在 `hit_test.rs` 注册 `FileHistoryRow { idx }`、`FileHistoryToolbar(FileHistoryToolbarButton)`、`FileHistoryDiffArea` 命中目标，支持鼠标点击选行、滚轮滚动列表/Diff、点击工具栏按钮与双击行定位主日志。
- [x] **完善键盘交互与无损返回（`input.rs`）**：
  - 列表导航：`j`/`k`/`Up`/`Down`/`PageUp`/`PageDown`/`Home`/`End`/`gg`/`G`（列表焦点时移动选中提交，Diff 焦点时通过 `ScrollPane::FileHistoryDiff` 滚动 Diff）。
  - `Shift+Up`/`Shift+Down`：框选两个提交进行双版本对比；单按方向键清除 `anchor`。
  - `Tab` / `BackTab`：在提交列表与 Diff 预览之间切换焦点。
  - `f`：切换 `--follow` 重命名追踪并刷新历史列表。
  - `l`：切换对比本地工作区模式。
  - `S` / `w` / `z`（或 `F4`）：切换并排 Diff / 上下文行数 / 全屏 Diff。
  - `Enter` / `g`：关闭文件历史浮窗，切换至 `[2 Log]` 并派发 `Action::JumpToCommit(entry.oid)`（若主日志当前筛选器隐藏了该提交，给出 Toast 提示）。
  - `Esc`：若处于 `diff_zoomed` 先退出全屏；否则关闭浮窗、清空临时 `file_history` 状态，无损还原进入前的 Tab、焦点面板与筛选器。
- [x] **补充中英 i18n 文案与回归测试**：
  - 在 `zh-CN.toml` / `en-US.toml` 补齐 `view.file_history.*` 文案（标题、空状态、加载状态、截断提示、工具栏按钮与底栏提示）。
  - 编写 TUI 单元测试与 `TestBackend` 渲染测试：
    1. 打开 `PopupKind::FileHistory` → 接收 `FileHistoryLoaded`（含重命名条目）→ 验证默认选中 `start_oid` 行、右侧请求对应 `path_at_commit` 的 Diff、渲染输出包含重命名标记；
    2. 切换 `f`（`follow_renames`）重新派发 `Effect::RequestFileHistory`；
    3. 按 `l` 切换 `compare_with_local` 派发 `DetailsMode::CompareWithLocal` Diff 请求；
    4. 按 `Enter` / `g` 关闭浮窗并触发主日志定位该提交；按 `Esc` 无损保留进入前的 `LogFilterSpec` 与 `focused_pane`；
    5. 验证 `ScrollPane::FileHistoryDiff` 在触底后继续下滚无 Overscroll 死区，且窄屏（`80×24`）渲染不发生 panic。

---

## 3. 验收标准 (Acceptance Criteria)

- [x] `cargo test --workspace` 与 `cargo clippy --workspace --all-targets` 全部通过。
- [x] 从 `[2 Log]` 变更文件树或 `[1 Commit]` 暂存树触发「查看文件历史…」，可流畅查看该文件跨重命名的完整历史并实时联动高亮 Diff。
- [x] 支持 `--follow` 切换、与本地工作区对比、两历史版本对比、`Enter`/`g` 跳回主日志定位，以及 `Esc` 无损返回。
- [x] Diff 滚动接入 `ScrollPane::FileHistoryDiff` 双端夹紧，宽屏与窄屏（`80×24`）渲染均正常无错位或 panic。

---

## Comments

- 2026-11-20 (agent): 创建 Issue 03，依赖 Issue 01（单文件历史引擎）与 Issue 02（变更文件菜单及触发入口）。
- 2026-11-20 (agent): 已完成实现并验证：`FileHistoryState`/`FileHistoryFocus`、`PopupKind::FileHistory`、`ScrollPane::FileHistoryDiff`、`views/file_history.rs`（自适应宽/窄双分屏、`render_file_diff_body` 复用、重命名徽标 `←`/`@`、可点击工具栏与选中范围高亮）、`input.rs`/`input_menus.rs` 键盘与鼠标交互（`f`/`l`/`Shift+↑↓`/`Tab`/`S`/`w`/`z`/`F4`/`Enter`/`g`/`Esc`）、30ms 防抖与 LRU 复用及 `spec09_tests.rs` `TestBackend` 端到端测试；`cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
