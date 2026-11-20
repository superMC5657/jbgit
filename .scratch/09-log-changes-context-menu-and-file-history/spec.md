# Spec 09: 变更文件上下文菜单与单文件历史（含重命名追踪）规约

- **目录**：`.scratch/09-log-changes-context-menu-and-file-history/`
- **下属 Issues**：
  - [`01-file-history-engine-and-follow-rename.md`](./issues/01-file-history-engine-and-follow-rename.md)
  - [`02-log-changes-context-menu-and-file-ops.md`](./issues/02-log-changes-context-menu-and-file-ops.md)
  - [`03-file-history-view-and-diff-inspector.md`](./issues/03-file-history-view-and-diff-inspector.md)
- **参考源码（IntelliJ IDEA / git4idea）**：
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/frame/VcsLogChangesBrowser.kt`
  - `reference/platform/vcs-log/impl/resources/intellij.platform.vcs.log.impl.xml`（`Vcs.Log.ChangesBrowser.Popup`）
  - `reference/platform/vcs-impl/resources/META-INF/VcsActions.xml`（`Vcs.RepositoryChangesBrowserMenu`：`Vcs.ShowHistoryForRevision` / `Vcs.ShowDiffWithLocal` / `Vcs.GetVersion` / `Vcs.EditSource`）
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/history/VcsLogFileHistoryUiProvider.kt`（单文件历史 vs 目录历史分流）
- **关联源码**：
  - `crates/jbgit-core/src/details.rs`、`git_ops.rs`、`lib.rs`
  - `crates/jbgit-engine/src/details.rs`、`pool.rs`、`supervisor.rs`、`ops.rs`、`lib.rs`
  - `crates/jbgit-tui/src/app.rs`、`app_git.rs`、`log_view.rs`、`input.rs`、`input_menus.rs`
  - `crates/jbgit-tui/src/menus/log.rs`、`menus/stage.rs`、`menus/mod.rs`
  - `crates/jbgit-tui/src/views/overlays.rs`、`views/log_details.rs`、`views/mod.rs`
  - `crates/jbgit-tui/src/keymap/mod.rs`、`keymap/actions.rs`、`config/defaults/keymap.default.toml`
  - `crates/jbgit-tui/src/i18n/locales/zh-CN.toml`、`en-US.toml`

---

## 1. 背景与问题诊断

在 `[2 Log]` 右上「变更文件」面板（`PaneId::LogChangesTree`）中，对照 IDEA 原生 `VcsLogChangesBrowser` 仍存在两块核心能力缺失：

1. **变更文件列表缺少右键/键盘上下文菜单与文件级操作**：
   - 目前 `crates/jbgit-tui/src/input_menus.rs` 的 `context_menu_for_target` 与 `keyboard_menu_target` 未匹配 `HitTarget::LogChangeRow` 与 `PaneId::LogChangesTree`，用户在变更文件树上右键或按 `.` / `Shift+F10` 无任何菜单弹出。
   - 无法直接对单个历史变更文件执行高频操作：查看该文件提交历史、按该路径一键筛选日志、与本地工作区对比该文件、从该提交恢复文件版本（`Get Version`）、在 `$EDITOR` 中打开、复制相对路径或导出单文件补丁。
   - 现有的 `Action::CompareWithLocal { target }` 仅切换整个提交与工作区的差异列表，无法自动定位并展开用户刚刚右键点击的那个目标文件。
2. **缺少可跨重命名追踪的单文件历史检视能力（`Show History for Revision`）**：
   - IDEA 中对单个文件查看历史时，会打开独立的单文件历史视图（`VcsLogSingleFileHistoryProvider`），支持跨 `git mv` 重命名追踪（`--follow`）、在历次提交间快速联动预览该文件 Diff、定位回主日志提交，且关闭后无损保留原日志筛选上下文；对目录查看历史则复用路径筛选（`VcsLogDirectoryHistoryProvider`）。
   - 当前 `jbgit-engine` 仅提供用于日志顶部 `Paths` 筛选器的 `load_path_matches_sync`（基于 `git2::DiffOptions::pathspec`），无法跨文件重命名追踪，也未记录文件在历史各提交时的真实路径（`path_at_commit`），因此在改名前的旧提交上无法直接联动预览单文件 Diff。

---

## 2. 设计目标与职责边界

### 2.1 核心目标
1. **引擎级单文件历史与重命名追踪**：在 `jbgit-core` / `jbgit-engine` 新增异步可取消的单文件历史加载链路，基于 `git log --follow`（NUL 分隔安全解析）追踪跨重命名的提交序列及每次提交对应的 `path_at_commit` / `old_path` / 行统计，同时补充 `GitOp::CheckoutPaths` 支撑从指定提交恢复文件。
2. **补齐变更文件上下文菜单**：为 `PaneId::LogChangesTree` / `HitTarget::LogChangeRow` 接入完整的鼠标右键与键盘菜单（`.` / `Shift+F10` / `Menu`），覆盖单文件与目录两类行节点，并在 `[1 Commit]` 暂存树右键菜单复用「查看文件历史」入口。
3. **无损双分屏单文件历史检视器**：以全屏化/大尺寸模态双分屏浮窗（`PopupKind::FileHistory`）呈现单文件历史列表 + 联动 `syntect` 语法高亮 Diff 预览，支持 `--follow` 切换、单提交/双提交/与本地对比、`Enter`/`g` 一键跳回主日志定位、`Esc` 无损返回原工作态，并接入 Spec 08 的 `ScrollPane` 滚动夹紧机制杜绝 Overscroll。

### 2.2 非目标（Out of Scope）
- **不改动用户已完成并对齐的「变更文件」工具栏展开/折叠逻辑**：保留现有 `views/log_details.rs` 中的 `⊞ 目录`、`⊕`、`⊖`（`ChangesExpandState`）与 `◉ 差异` 布局及高亮行为，不推翻或重构该工具栏。
- **不引入多 Tab 日志管理器**：单文件历史采用模态双分屏视图（按 `Esc` 无损还原主日志状态），避免破坏当前四 Tab（`[1 Commit]` / `[2 Log]` / `[3 Stash]` / `[4 Console]`）全局拓扑；目录历史直接复用主日志顶部 `Paths` 筛选器。
- **不包含逐行 `git blame` / Annotate 视图**：留待后续独立 Spec 规划。

---

## 3. 架构设计

### 3.1 领域模型与 `GitOp` 扩展（`jbgit-core`）

1. **单文件历史模型（`crates/jbgit-core/src/details.rs`）**：

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileHistoryKey {
    /// 发起查询时的相对路径（仓库根相对、POSIX `/` 分隔）。
    pub path: String,
    /// 查询起点提交（`None` 表示从 `HEAD` 开始；来自 Log 变更文件时为选中提交 OID）。
    pub start_oid: Option<Oid>,
    /// 是否启用 `git log --follow` 追踪重命名。
    pub follow_renames: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHistoryEntry {
    pub oid: Oid,
    /// 该文件在此提交中的路径（重命名之前可能与 `key.path` 不同）。
    pub path_at_commit: String,
    /// 若本次提交为重命名/拷贝，记录变更前路径。
    pub old_path: Option<String>,
    pub kind: FileStatusKind,
    pub additions: usize,
    pub deletions: usize,
    pub is_binary: bool,
    pub author_name: String,
    pub author_time: i64,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHistoryBundle {
    pub key: FileHistoryKey,
    pub entries: Vec<FileHistoryEntry>,
    /// 达到单次加载上限时置 true（默认 500 条，避免超大仓阻塞）。
    pub truncated: bool,
}
```

2. **单文件版本恢复操作（`crates/jbgit-core/src/git_ops.rs`）**：
   - 新增 `GitOp::CheckoutPaths { rev: String, paths: Vec<String> }`（对应 IDEA `Vcs.GetVersion`），映射命令 `git checkout <rev> -- <paths...>`，归类为写操作并附带 `RefreshScope::All`（刷新暂存区与工作区状态）；超过 `PATHSPEC_FROM_FILE_THRESHOLD` 时兼容已有 pathspec 策略，执行前由 TUI 弹出二次确认框防止意外覆盖未保存本地修改。

### 3.2 异步单文件历史加载引擎（`jbgit-engine`）

遵循 Spec 00 确立的混合引擎分工（`git2` 线程池负责只读快照与单文件 Blob Diff；只读 `git` CLI 负责复杂拓扑/重命名追踪，强制 `GIT_OPTIONAL_LOCKS=0` 与 `GIT_TERMINAL_PROMPT=0`）：

- **执行命令**：
  ```text
  git -c core.quotepath=false log
      --max-count=501
      [--follow]
      -z
      --name-status
      --format=%x1e%H%x1f%an%x1f%at%x1f%s%x1f
      <start_rev> -- <path>
  ```
  - `-c core.quotepath=false` + `-z` 确保含空格、中文、引号等特殊路径零歧义解析；
  - 解析器为 `jbgit-core` 或 `jbgit-engine` 内纯函数 `parse_file_history_output(raw: &[u8]) -> Vec<FileHistoryEntry>`，可脱离真实 Git 仓库做高覆盖单元测试；
  - 行增删统计可采用同一次调用或轻量二级补算策略（或直接通过 `--numstat` + `--name-status` 双段 / `git2` 对比目标单文件 blob 计算，保证性能与解析可靠性——详见 Issue 01 规约，推荐在 `git log -z --format=... --raw --numstat` 一次调用内同时获取 `status`、`old_path`、`path_at_commit` 与 `additions/deletions/is_binary`，避免二次遍历！）。
- **任务管线与防抖取消（`crates/jbgit-engine/src/supervisor.rs`）**：
  - `TaskRequest::LoadFileHistory { key: FileHistoryKey, epoch: u64 }`
  - `EngineEvent::FileHistoryLoaded { bundle: Arc<FileHistoryBundle>, epoch: u64 }`
  - `EngineHandle::request_file_history(&self, key: FileHistoryKey) -> u64`
  - 在 `supervisor.rs` 中新增独立 `EpochTracker`（`file_history_epoch`），切换文件或切换 `--follow` 时立即丢弃过期结果。
- **历史提交单文件 Diff 复用**：
  - 浏览文件历史选中第 `i` 条提交时，直接使用已有 `FileDiffKey { details: DetailsKey::single(entry.oid), path: entry.path_at_commit, context }` 请求 Diff（或对比本地时使用 `DetailsKey::compare_with_local(entry.oid)`，双版本对比时使用 `DetailsKey::compare_versions(older.oid, newer.oid)`），无需重复造 Diff 轮子！
  - 注：针对双版本对比跨重命名场景（`older.path_at_commit != newer.path_at_commit`），可在 `DetailsKey::CompareVersions` 下按 `newer.path_at_commit` 匹配，并在 Diff 标题提示重命名路径。

### 3.3 变更文件上下文菜单与文件级操作（`jbgit-tui`）

1. **命中目标与键盘菜单**：
   - `input_menus.rs::context_menu_for_target` 新增：
     - `HitTarget::LogChangeRow { idx }`：聚焦 `PaneId::LogChangesTree` 并选中 `idx`，返回 `menus::log_change_menu_items(state)`。
     - `HitTarget::Pane(PaneId::LogChangesTree)`：聚焦 `PaneId::LogChangesTree`，若当前有选中行则返回 `menus::log_change_menu_items(state)`。
   - `input_menus.rs::keyboard_menu_target` 新增 `PaneId::LogChangesTree` 分支，定位当前 `changes_selected` 行的 `HitTarget::LogChangeRow { idx }`。
2. **`menus::log_change_menu_items(state)` 条目设计（对标 `Vcs.RepositoryChangesBrowserMenu`）**：

| 分组 | 菜单项 | 助记符 | 适用行类型 | 动作 / 约束 |
| :--- | :--- | :---: | :--- | :--- |
| 查看与对比 | **查看差异 / 切换差异预览** | `d` | 文件行 | `Action::ToggleDiffPreview`（目录行置灰） |
| 查看与对比 | **全屏查看差异** | `z` | 文件行 | `Action::ZoomDiff(true)`（`F4` / `Ctrl+Enter`） |
| 查看与对比 | **与本地工作区对比** | `l` | 文件行 | 仅单提交模式可用；触发 `Action::CompareFileWithLocal { target, path }`，加载完成后自动定位并选中 `path` 并展开 Diff 预览 |
| 历史与筛选 | **查看文件历史…**（目录行显示「查看目录历史（筛选日志）」） | `h` | 文件 / 目录 | 文件行：`Action::OpenFileHistory { path, start_oid, follow_renames: true }`；目录行：`Action::FilterLogByPath(dir_key)` |
| 历史与筛选 | **按此路径筛选日志** | `f` | 文件 / 目录 | `Action::FilterLogByPath(path_or_dir)`，直接写入顶部 `Paths` 筛选器并触发 `RequestPathMatches` |
| 恢复与编辑 | **从此版本恢复文件…** | `g` | 文件行 | 仅单提交模式且非 `Deleted` 文件可用；弹出确认弹窗后执行 `GitOp::CheckoutPaths`（对标 IDEA `Vcs.GetVersion`） |
| 恢复与编辑 | **在 `$EDITOR` 中打开** | `e` | 文件行 | 复用 `Action::OpenExternalEditorFile(Some(path))`；若工作区不存在该文件则置灰提示 |
| 剪贴板 | **复制相对路径** | `c` | 文件 / 目录 | `Action::CopyText(path)` |
| 剪贴板 | **复制该文件补丁** | `p` | 文件行 | 仅单提交模式可用；`GitOp::PatchToClipboard`（`git show --format= --patch <oid> -- <path>`） |

3. **联动 `[1 Commit]` 暂存树菜单**：
   - 在 `crates/jbgit-tui/src/menus/stage.rs::stage_tree_menu_items` 中，对已跟踪文件（非 `Untracked` / `Ignored`）追加「查看文件历史…」（`Action::OpenFileHistory { path, start_oid: None, follow_renames: true }`），与 IDEA 的 Local Changes 右键 *Show History* 对齐。

### 3.4 单文件历史双分屏视图（`PopupKind::FileHistory`）

- **状态结构（`FileHistoryState`，挂于 `AppState`）**：
  - `key: FileHistoryKey`、`bundle: Option<Arc<FileHistoryBundle>>`、`loading: bool`、`error: Option<String>`
  - `selected: usize`、`anchor: Option<usize>`（支持 `Shift+Up/Down` 选中两个提交进行双版本对比）
  - `compare_with_local: bool`（一键切换对比当前选中历史提交 vs 本地工作区）
  - `diff_cache: LruCache<FileDiffKey, Arc<FileDiff>>`（或复用 `details.diff_cache` / `diff_syntax` 缓存，零重复高亮计算）
  - `diff_side_by_side: bool`、`diff_context: DiffContext`、`diff_scroll: usize`、`diff_zoomed: bool`
  - `focus: FileHistoryFocus`（`List` / `Diff`，`Tab` 或鼠标点击切换）
- **布局与渲染（`crates/jbgit-tui/src/views/overlays.rs` 或拆独立模块 `views/file_history.rs`）**：
  - 占用终端 `92% × 88%`（窄屏自动贴边留 1 格边距），带顶栏标题 `文件历史 · <path> [--follow] (N 条提交)` 与快捷开关按钮（`[跟随重命名]`、`[对比本地]`、`[并排/单栏]`、`[上下文]`、`[在日志中定位]`）。
  - 内部水平分屏（宽屏左右 42% / 58%，窄屏 `< 100` 列自动切上下分屏）：
    - **列表区**：展示提交短哈希、状态色徽标（`A`/`M`/`D`/`R`）、提交说明、作者、相对/绝对日期、以及当 `path_at_commit != key.path` 或存在 `old_path` 时渲染醒目的路径变更徽标（如 `← old/path.rs`）。
    - **Diff 预览区**：复用 `log_details.rs` 的 `syntect` 语法高亮行级 + 行内词级渲染管线，并将 `(viewport_h, max_scroll)` 写入新增的 `ScrollPane::FileHistoryDiff`（Spec 08 双端夹紧）。
- **无损返回与主日志跳转**：
  - `Esc`：关闭浮窗（若处于全屏 Diff 先退出全屏），主日志原有 `LogFilterSpec`、`selected_idx` 与 `focused_pane` 原封不动保留。
  - `Enter` / `g`：关闭浮窗并派发 `Action::JumpToCommit(entry.oid)`，在 `[2 Log]` 中居中选中该提交（若当前主日志激活了筛选器且不包含该提交，给出清晰 Toast 提示或引导清除筛选）。

---

## 4. 任务拆分与依赖顺序

1. **Issue 01**（[`01-file-history-engine-and-follow-rename.md`](./issues/01-file-history-engine-and-follow-rename.md)）：在 `jbgit-core` 与 `jbgit-engine` 完成单文件历史领域模型、`git log --follow -z --raw --numstat` 安全解析器、`TaskRequest::LoadFileHistory` / `EngineEvent::FileHistoryLoaded` 异步可取消管线以及 `GitOp::CheckoutPaths` 写操作支撑，并补齐含 `git mv` 重命名链的集成测试。
2. **Issue 02**（[`02-log-changes-context-menu-and-file-ops.md`](./issues/02-log-changes-context-menu-and-file-ops.md)，Blocked by `01`）：为 `[2 Log]` 变更文件树接入鼠标右键与键盘上下文菜单（`menus::log_change_menu_items`），打通 `FilterLogByPath`、单文件 `CompareWithLocal` 自动定位（`pending_select_path`）、`GetVersion` 文件恢复确认流、`$EDITOR` 打开、复制路径/补丁，并在 `[1 Commit]` 暂存树补齐「查看文件历史」菜单入口及 i18n/键位配置。
3. **Issue 03**（[`03-file-history-view-and-diff-inspector.md`](./issues/03-file-history-view-and-diff-inspector.md)，Blocked by `01, 02`）：实现 `FileHistoryState` 与 `PopupKind::FileHistory` 双分屏交互视图，打通单文件历史列表、重命名路径徽标、30ms 防抖语法高亮 Diff 联动、`--follow` 切换、与本地/双版本对比、`Enter`/`g` 跳回主日志、`ScrollPane::FileHistoryDiff` 夹紧及完整 TUI 渲染/交互测试。
