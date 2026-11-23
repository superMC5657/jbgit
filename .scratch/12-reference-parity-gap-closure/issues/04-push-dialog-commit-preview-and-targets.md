# 04: 完整 Push 对话框（待推提交预览、目标编辑、Tag 模式、被拒后重试）

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M9 — reference 缺口补齐
Priority: P0

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/push/{GitPushSupport,GitPushOperation,GitPushSource,GitPushTarget,GitPushTargetPanel}.java`
  - `reference/plugins/git4idea/shared/src/git4idea/push/GitPushTagMode.kt`
  - `reference/plugins/git4idea/backend/src/actions/addCommit/GitPushSelectedCommitsAction.kt`
- **关联文件**：
  - `crates/jbgit-core/src/git_ops/{ops,outcome}.rs`、`crates/jbgit-engine/src/ops.rs`（`GitOp::Push` 编译，约 `ops.rs:193-211`）
  - `crates/jbgit-tui/src/app/git_workflows.rs`（`prompt_push`，约 `:548`）、`crates/jbgit-tui/src/dialogs.rs`、`crates/jbgit-tui/src/app/modal_stack.rs`、`crates/jbgit-tui/src/views/`（新增 `push_dialog.rs`）

---

## 1. 背景与目标

当前 `prompt_push` 只是一个文本确认框（显示 ahead/behind 数），无法看到将要推送哪些提交、无法修改目标分支、无法推 Tag，被拒后也只有通用错误。IDEA 的 Push 对话框是提交前最后一道检查。

## 2. 任务清单

- [x] **`GitOp::Push` 扩展**：增加 `remote_branch: String`（默认 = upstream 名或同名）、`tags: PushTagMode { None, Follow, All }`（`--follow-tags` / `--tags`）、`force: PushForce { None, WithLease, Force }`（替换 `force_with_lease: bool`），refspec 改为 `<src>:refs/heads/<remote_branch>`。
- [x] **只读查询**：`load_outgoing_sync(loc, branch, remote, remote_branch) -> OutgoingBundle { commits: Vec<CommitSummary>, files_changed: usize, remote_exists: bool }`，`remote/remote_branch..branch`（远端分支不存在时为相对 `remote/HEAD` 或全部可达且未推送的提交，上限 1000）。
- [x] **`ModalLayer::PushDialog(Box<PushDialogState>)`**：
  - 左栏：待推送分支行（`branch → remote : remote_branch`），目标可内联编辑；远端下拉来自 `RepoSnapshot.remotes`（Issue 07 落地前回退到 `default_remote()`），并对远端分支名做速查补全（`GitPushTargetCompletionProvider`）。
  - 中栏：待推提交列表（复用 Log 行渲染：短哈希、作者、相对日期、主题），选中提交时右栏显示其变更文件，可再打开 Diff。
  - 底部选项：`Push tags: None / Current branch (follow) / All`、`Force push (with lease)`、`Set upstream`（无 upstream 时默认勾选）。
  - `Push up to here`（`PromptPush { up_to }`）复用同一对话框并在列表中截断显示。
- [x] **受保护分支**：`jbgit.toml` `[push] protected_branches = ["main", "master"]`（glob），对其强推时按钮禁用并显示原因（对齐 `GitPushSupport` 的 protected branches 行为）。
- [x] **被拒恢复**：`GitFailure::PushRejected` 时弹出"远端有新提交：Merge / Rebase 后重试 / 取消"，选择后串行执行 `fetch` → `merge|rebase <upstream>` → 原 Push（可组成 `GitOp` 序列，冲突时停在冲突横幅流程）。
- [x] 推送成功的通知汇总：推送提交数、新建远端分支、推送的 Tag 数。

## 3. 测试

- 编译测试：各 `PushTagMode` / `PushForce` / 自定义 `remote_branch` 的参数。
- 引擎集成（裸仓远端）：outgoing 列表在远端分支不存在 / 落后 / 已同步三种状态下正确；被拒后 rebase 重试成功。
- TUI：受保护分支强推禁用；`Push up to` 截断；`Esc` 不执行任何操作。
