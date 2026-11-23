# 10: Working Trees（Worktree 管理）

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M9 — reference 缺口补齐
Priority: P1
Blocked by: 09

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/workingTrees/{GitWorkingTreesService.kt,GitCreateWorkingTreeService.kt,GitWorktreeListParser.kt,GitWorktreeSupportStatus.kt}`
  - `reference/plugins/git4idea/backend/src/workingTrees/actions/{GitCreateWorkingTreeAction,GitOpenExistingWorkingTreeForLocalBranchAction,ShowWorkingTreesAction,GitChooseWorkingTreeDialog}.kt`
  - `reference/plugins/git4idea/backend/src/actions/{GitCreateWorkingTreeAction,GitToggleLockWorkingTreeAction}`
- **关联文件**：
  - `crates/jbgit-engine/src/probe.rs`（已支持识别 linked worktree 的 `.git` 文件）
  - `crates/jbgit-core/src/git_ops/ops.rs`、`crates/jbgit-engine/src/ops.rs`
  - `crates/jbgit-tui/src/branches_popup.rs`、`crates/jbgit-tui/src/menus/{branch,log}.rs`、Issue 09 的 `RepoSwitcher`

---

## 1. 背景与目标

Worktree 是 git4idea 近期的重点功能（≈ 4.9k 行）。它让用户不必 stash 就能并行处理多个分支；对终端用户同样高频。

## 2. 任务清单

- [ ] **模型与解析**：`WorktreeInfo { path, head: Option<Oid>, branch: Option<String>, is_main: bool, is_bare: bool, detached: bool, locked: Option<String>, prunable: Option<String> }`；纯函数解析 `git worktree list --porcelain -z`（参照 `GitWorktreeListParser.kt`）。`RepoSnapshot` 新增 `worktrees: Vec<WorktreeInfo>`（只读 CLI，在 refs 变化时刷新）。
- [ ] **支持检测**：git < 2.17 时隐藏功能并在帮助中说明（对齐 `GitWorktreeSupportStatus`）。
- [ ] **`GitOp`**：`AddWorktree { path, branch: WorktreeBranch { Existing(name), New { name, start }, Detached(Oid) } }`、`RemoveWorktree { path, force }`、`LockWorktree { path, reason }`、`UnlockWorktree { path }`、`PruneWorktrees`。
- [ ] **视图 `ModalLayer::Worktrees`**：列表（路径、分支、HEAD 短哈希、锁定 / 可清理标记）；动作：打开（经 Issue 09 `RepoSession::bind`）、新建、锁定 / 解锁、删除（脏工作区时要求 force 二次确认）、清理。
- [ ] **新建对话框**：默认路径 `<repo 父目录>/<repo名>-<分支名>`（`[worktree] default_parent` 可配置），分支三选一；创建成功后询问"立即打开"。
- [ ] **分支联动**：分支浮窗 / 面板中已被其他 worktree 检出的分支显示标记，Checkout 置为 disabled 并给出"在其 Worktree 中打开"动作（对应 `GitOpenExistingWorkingTreeForLocalBranchAction`）；Log 提交菜单新增 "New Worktree from Here…"。
- [ ] `GitFailure::BranchCheckedOutInWorktree`（解析 `is already checked out at` / `is already used by worktree`）。

## 3. 测试

- 解析器：主 worktree、bare、detached、locked（含原因）、prunable。
- 集成：新建 → 列表可见 → 锁定后删除被拒 → 解锁删除 → prune。
- `TuiSession`：在 worktree 中打开后头部仓库名与分支更新；切回主仓库状态恢复。
