# 05: Pull 选项对话框、标准 Rebase 对话框与交互式 Rebase 增强

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M10 — reference 深度对齐与正确性加固
Priority: P1

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/pull/{GitPullDialog.kt,GitPullOption.kt}`
  - `reference/plugins/git4idea/backend/src/rebase/{GitRebaseDialog.kt,GitRebaseOption.kt,GitRebaseEntry.kt,GitCommitFixupBySubjectAction.kt,GitCommitSquashBySubjectAction.kt}`
  - `reference/plugins/git4idea/backend/src/rebase/interactive/GitRebaseTodoModel.kt`
  - `reference/plugins/git4idea/backend/src/ui/branch/GitRebaseOntoCommitAction.kt`、`rebase/log/squash/GitSquashOperation.kt`
- **关联文件**：
  - `crates/jbgit-core/src/git_ops/{ops.rs,rebase.rs}`、`crates/jbgit-core/src/repo.rs`（`plan_rewrite_typed`）
  - `crates/jbgit-engine/src/ops.rs`（`Pull`、`Rebase`、`RewriteHistory`、`FixupCommit`）
  - `crates/jbgit-tui/src/{dialogs.rs,menus/branch.rs,menus/log.rs,app/git_workflows.rs,keymap/catalog.rs}`

---

## 1. 背景与目标

Spec 12 补齐了 Merge 对话框，但 **Pull** 与 **标准 Rebase** 仍无选项对话框：`GitPull` 写死 `git pull --no-edit`（无法选远端/分支，也无法传 `--rebase`/`--ff-only`/`--no-ff`/`--squash`/`--no-commit`/`--no-verify`）；`GitOp::Rebase` 仅支持无参数 `git rebase <onto> [<branch>]`（缺少 `--onto`、`-i`、`--rebase-merges`、`--update-refs`、`--autosquash`）。同时交互式 Rebase 缺少核心动作 `edit`（停下修改）以及独立创建 `fixup!`/`squash!` 提交的能力。

## 2. 任务清单

- [ ] **Pull 选项模型与对话框（`PullOptions` + `PullDialog`）**：
  - 在 `jbgit-core::git_ops::ops` 新增 `PullOptions { remote: Option<String>, branch: Option<String>, rebase: bool, ff: FastForward, squash: bool, no_commit: bool, no_verify: bool }` 与纯函数 `validate() -> Result<(), PullOptionConflict>`（对齐 `GitPullOption` 互斥矩阵：`rebase` ⟂ `squash`/`no_ff`/`ff_only`/`no_commit`，`squash` ⟂ `no_ff`/`ff_only`）；
  - 扩展 `GitOp::Pull { options: PullOptions }`（默认保持现有 `git pull --no-edit` 行为不破坏回归）；
  - 提供 `Pull…` 对话框（可选远端、可补全远端分支、刷新/fetch 列表、勾选选项并记忆上次选择至 `<common_dir>/jbgit-pull-options`），保留快捷 `Pull` 动作。
- [ ] **标准 Rebase 选项模型与对话框（`RebaseOptions` + `RebaseDialog`）**：
  - 新增 `RebaseOptions { onto: String, upstream: Option<String>, branch: Option<String>, interactive: bool, rebase_merges: bool, update_refs: bool, keep_empty: bool, autosquash: bool, root: bool }`（带互斥与版本检查，如 `--update-refs` 需 Git ≥ 2.38，通过已有 `GitVersion` 判断）；
  - 扩展 `GitOp::Rebase { options: RebaseOptions }`，在分支菜单提供 `Rebase…` 入口；
  - 在 Log 提交右键菜单新增 **Rebase Current Branch onto Selected Commit**（直接以选中提交为目标发起 Rebase）。
- [ ] **交互式 Rebase（`RebaseTodoDialog`）与 `fixup!`/`squash!` 增强**：
  - `TodoAction` 新增 `Edit`（停下修改，对应 `e` 键；暂停后进入 `OngoingKind::Rebase` 状态并引导用户在 Commit 面板修改后 Continue）及只读保留的 `UpdateRef` 行；
  - `RebaseTodoDialog` 下半区/侧边嵌入当前光标提交的变更文件摘要预览，支持对选定提交按 `d` 查看完整 Diff；
  - `RewordHead` 与 `render_todo` 中的 reword 默认遵守仓库 hooks（不硬编码 `--no-verify`，由可选标志控制）；
  - Log 菜单拆分：**Fixup! Commit**（仅创建 `git commit --fixup=<oid>`）、**Squash! Commit**（`git commit --squash=<oid>`）与 **Fixup & Autosquash**（原立即变基）；
  - **非连续 Squash**：当用户在 Log 选中多个非连续（但同在当前线性历史内的）提交时，允许先自动重排相邻再生成 squash todo，取消 `squash_non_contiguous` 硬拦截（仍保留含合并提交与受保护分支保护）。

## 3. 测试

- **单元测试**：`PullOptions::validate()` 与 `RebaseOptions` 编译参数的全组合测试；`TodoAction::Edit` / `render_todo` 与非连续提交自动重排 squash 规划测试。
- **引擎集成测试**：
  - `PullOptions { rebase: true }` 与 `PullOptions { ff: FfOnly }` 在分叉远端上的行为验证；
  - `RebaseOptions { onto, upstream, update_refs: true }` 在堆叠分支上的变基与 ref 联动更新；
  - 交互式 Rebase 含 `Edit` 动作 → 仓库停在 `RebaseInteractive` → 修改文件并 `amend` → `Ongoing::Continue` 完成改写；
  - `fixup!` 提交后通过 `RebaseOptions { autosquash: true }` 自动合并。
- **TUI 会话测试**：打开 `Pull…` 与 `Rebase…` 对话框，验证互斥选项联动拦截、记忆恢复与命令预览。
