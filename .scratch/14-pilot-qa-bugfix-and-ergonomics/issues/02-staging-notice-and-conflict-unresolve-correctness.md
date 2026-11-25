# 02: 暂存进度提示清理、`UD` 冲突重置与文件历史起始范围对齐

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M11 — Pilot QA 缺陷修复与体验加固
Priority: P2
QA-Refs: `P2-01`, `P2-02`, `P2-05`

- **关联文件**：
  - `crates/jbgit-tui/src/stage_view/workbench.rs`（`apply_engine_event` `:1681-1696`、`StageToggleShowIgnored` `:1956-1961`、`StageActionSelected` / `UnstageActionSelected` / `StageAllTracked` `:1968-2009`）
  - `crates/jbgit-tui/src/app/state.rs`（`has_active_tasks` `:689-706`）
  - `crates/jbgit-tui/src/views/header.rs`（`:217-229`）
  - `crates/jbgit-engine/src/ops.rs`（`GitOp::UnresolveConflict` `:544-553`、`plan_accept_side` `:1006-1064`）
  - `crates/jbgit-engine/src/details/file_history.rs`（`build_file_history_args` `:435-475`）
  - `crates/jbgit-tui/src/views/file_history.rs`（`:119-141`）
  - `crates/jbgit-tui/src/app/dispatch.rs`（`OpenFileHistory` `:1033-1070`）、`crates/jbgit-tui/src/menus/log.rs`（`:1136`）、`crates/jbgit-tui/src/keymap/catalog.rs`（`:979`）

---

## 1. 根因定位

### 1.1 [`P2-01`] 暂存/取消暂存后顶栏第 0 行进度文案与 Braille spinner 永不消失
存在两处漏清理：
1. **瞬态提示未清除**：`StageActionSelected`（`workbench.rs:1978`）、`UnstageActionSelected`（`:1996`）与 `StageAllTracked`（`:2008`）分别设置了瞬态进行中文案 `toast.staging_paths` / `toast.unstaging_paths` / `toast.staging_all_tracked`。引擎完成后经 `RefreshScope::StageStatus` 发回 `EngineEvent::StageStatusLoaded`（`workbench.rs:1690-1696`），但该分支仅调用 `self.apply_status(status)`，**从未清除 `*ctx.status_notice`**。
2. **`loading_snapshot` 标志卡死导致 spinner 永动**：`StageToggleShowIgnored`（`workbench.rs:1956-1961`）设置了 `*ctx.loading_snapshot = true` 却只发出了 `Effect::RefreshScope(RefreshScope::StageStatus)`（不会触发 `RepoSnapshotLoaded`）。而 `loading_snapshot` 仅在 `RepoSnapshotLoaded`（`app/events.rs:293`）和 `RepoLoadFailed`（`:918`）中清零，在 `StageStatusLoaded` 中从未复位，导致切换忽略文件后顶栏 `has_active_tasks()` 永远为 `true`，Braille spinner 一直旋转。

### 1.2 [`P2-02`] 冲突队列对已解决的 `UD` 文件按 `r`（重置已解决）失败
- `GitOp::UnresolveConflict`（`crates/jbgit-engine/src/ops.rs:544-553`）无条件编译为 `git checkout -m -- <paths>`。
- 对于 `UU`（双方修改），`git checkout -m` 可重新生成冲突标记；但对于 `UD`（如 `deleted_by_theirs.txt`，对方删除、我方修改）或 `DU`（我方删除、对方修改），在通过 `AcceptSide`（`git add` 或 `git rm -rf`）标记已解决后，索引中已不存在完整的 3 个 stage 版本，执行 `git checkout -m` 直接退出码 1 报错：
  `error: path 'deleted_by_theirs.txt' does not have all necessary versions`。

### 1.3 [`P2-05`] 从历史旧提交打开文件历史（`H`）时工具栏显示 `范围:HEAD` 却漏掉更新的 `HEAD` 提交
- 从 Log 变更文件树触发 `OpenFileHistory` 时（`menus/log.rs:1136`、`keymap/catalog.rs:979`），传入了 `start_oid: Some(<selected_commit_oid>)`，同时 `FileHistoryKey::new` 默认 `scope = HistoryBranchScope::CurrentHead`。
- 引擎 `build_file_history_args`（`details/file_history.rs:438-449`）在 `CurrentHead` 分支调用 `fallback_rev()`，优先使用 `key.start_oid`（即旧提交 `f08c17b`）而非 `"HEAD"`，因此只列出 `f08c17b` 及更早提交（3 条，不包含 `HEAD` 上的 `2f7ed7c`）。
- 但渲染层 `views/file_history.rs:121-130` 渲染 `HistoryBranchScope::CurrentHead` 时无视 `fh.key.start_oid`，一律硬编码显示 `view.file_history.scope_head`（`范围:HEAD`），造成工具栏标签与实际查询范围自相矛盾。

---

## 2. 任务清单

- [x] **清理暂存瞬态提示与 `loading_snapshot` 状态（`P2-01`）**：
  - 在 `StageWorkbench` 记录是否存在待完成的 transient stage notice（或引入明确标志 `pending_stage_notice: bool`），当 `StageActionSelected` / `UnstageActionSelected` / `StageAllTracked` 写入 `toast.staging_*` / `toast.unstaging_*` 时置 `true`；
  - 在 `EngineEvent::StageStatusLoaded`（`workbench.rs:1690-1696`）接收最新 epoch 后：若 `pending_stage_notice` 为 `true` 且当前 `status_notice` 仍为该瞬态提示，则清空 `*ctx.status_notice = None`（或替换为简短完成态）并复位 `pending_stage_notice = false`；
  - 在 `StageToggleShowIgnored`（`workbench.rs:1958`）不要借用 `loading_snapshot`（改用独立的 `loading_stage_status: bool` 纳入 `has_active_tasks()` 并在 `StageStatusLoaded` 复位，或在 `StageStatusLoaded` 清除对应的 stage loading 状态）。
- [x] **支持 Delete/Modify（`UD`/`DU`/`DD`/`AA`）冲突的无损重置（`P2-02`）**：
  - 将 `GitOp::UnresolveConflict` 的执行从纯 `compile_simple_steps` 升级为 `run_unresolve_conflict`（仿照 `run_accept_side` 的 `plan_accept_side` 模式）：
    1. 读取当前仓库进行中状态（`HEAD` + `MERGE_HEAD` / `CHERRY_PICK_HEAD` / `REVERT_HEAD` / `rebase-merge/stopped-sha` / `rebase-apply/original-commit`），解析语义 `ours_commit` / `theirs_commit`（注意 rebase/cherry-pick/revert 时三方基点分别为 `theirs^` 或 `merge_base(ours, theirs)`）；
    2. 对每个 path 查询 `(base_entry, ours_entry, theirs_entry)`：
       - 若三方均存在（标准 `UU`）且当前在索引中存在 stage 0，可直接走 `git checkout -m -- <path>`；
       - 若任一侧缺失（`UD`/`DU`/`AA` 等）或 `git checkout -m` 不可用，通过 `git update-index --index-info` 显式写入清除 stage 0（`0 0000... <path>`）并重建存在的 stage 1（base）/ stage 2（ours）/ stage 3（theirs）条目，同时将工作区文件恢复为对应存留侧内容（如 `UD` 恢复 `ours` 文件内容，避免 `git rm` 后工作区缺失文件）；
    3. 同步更新 `zh-CN.toml` / `en-US.toml:1417`（`toast.conflict_already_resolved_hint`）文案中过窄的 `git checkout -m` 描述（可选）。
- [x] **统一 FileHistory 工具栏范围标签与起始提交（`P2-05`）**：
  - 方案（推荐，保留「查看截至选中提交的文件历史」能力同时消除误导）：
    - 当 `fh.key.scope == HistoryBranchScope::CurrentHead` 且 `let Some(oid) = fh.key.start_oid`（且该 `oid` 不等于当前 `HEAD`）时，工具栏按钮显示短哈希 `范围:{short_oid}`（如 `范围:f08c17b`）并高亮表示非 `HEAD` 范围；
    - 当用户在范围下拉菜单中显式点击 `Current HEAD`（`FileHistorySetBranchScope(HistoryBranchScope::CurrentHead)`）时，清空 `fh.key.start_oid = None` 并重新拉取，使其真正切换到完整 `HEAD` 历史（4 条含 `2f7ed7c`）。

---

## 3. 测试与验收

- **TUI 会话测试（`P2-01`）**：触发 `StageActionSelected` / `UnstageActionSelected` 后注入 `StageStatusLoaded`，断言 `status_notice` 已清空；触发 `StageToggleShowIgnored` 后注入 `StageStatusLoaded`，断言 `has_active_tasks() == false`。
- **真实仓库集成测试（`P2-02`，修复前必挂）**：构造 `UD` 冲突（`deleted_by_theirs.txt`）→ 分别经 `AcceptSide(ours=true)` 与 `AcceptSide(ours=false)` 解决 → 执行 `GitOp::UnresolveConflict` → 断言退出成功且 `git status --porcelain` 重新恢复为 `UD deleted_by_theirs.txt`，同时 `UU` 冲突重置不回归。
- **TUI + Engine 测试（`P2-05`）**：从旧提交 `f08c17b` 打开 `FileHistory` 断言工具栏显示 `范围:f08c17b`（非 `范围:HEAD`）；切换菜单 `Current HEAD` 后断言 `start_oid == None` 且加载包含 `HEAD` 的全部提交。

## Comments

- **2026-11-25**: Resolved in `0fccbf6`; workspace gates (`fmt`, `clippy`, `test --workspace --locked`) and `jbgit-pilot` ConPTY E2E re-verification (`capabilities` `P2-01`, `conflict` `P2-02`, `full` `P2-05`) passed.
