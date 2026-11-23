# 10: Working Trees（Worktree 管理）

Status: resolved
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

- [x] **模型与解析**：`WorktreeInfo { path, head: Option<Oid>, branch: Option<String>, is_main: bool, is_bare: bool, detached: bool, locked: Option<String>, prunable: Option<String> }`；纯函数解析 `git worktree list --porcelain -z`（参照 `GitWorktreeListParser.kt`）。`RepoSnapshot` 新增 `worktrees: Vec<WorktreeInfo>`（只读 CLI，在 refs 变化时刷新）。
- [x] **支持检测**：git < 2.17 时隐藏功能并在帮助中说明（对齐 `GitWorktreeSupportStatus`）。
- [x] **`GitOp`**：`AddWorktree { path, branch: WorktreeBranch { Existing(name), New { name, start }, Detached(Oid) } }`、`RemoveWorktree { path, force }`、`LockWorktree { path, reason }`、`UnlockWorktree { path }`、`PruneWorktrees`。
- [x] **视图 `ModalLayer::Worktrees`**：列表（路径、分支、HEAD 短哈希、锁定 / 可清理标记）；动作：打开（经 Issue 09 `RepoSession::bind`）、新建、锁定 / 解锁、删除（脏工作区时要求 force 二次确认）、清理。
- [x] **新建对话框**：默认路径 `<repo 父目录>/<repo名>-<分支名>`（`[worktree] default_parent` 可配置），分支三选一；创建成功后询问"立即打开"。
- [x] **分支联动**：分支浮窗 / 面板中已被其他 worktree 检出的分支显示标记，Checkout 置为 disabled 并给出"在其 Worktree 中打开"动作（对应 `GitOpenExistingWorkingTreeForLocalBranchAction`）；Log 提交菜单新增 "New Worktree from Here…"。
- [x] `GitFailure::BranchCheckedOutInWorktree`（解析 `is already checked out at` / `is already used by worktree`）。

## 3. 测试

- 解析器：主 worktree、bare、detached、locked（含原因）、prunable。
- 集成：新建 → 列表可见 → 锁定后删除被拒 → 解锁删除 → prune。
- `TuiSession`：在 worktree 中打开后头部仓库名与分支更新；切回主仓库状态恢复。

## Comments

- 2026-11-24 实现说明：
  - `jbgit-core::worktree`：`WorktreeInfo`、`WorktreeBranch { Existing, New { name, start }, Detached }`、`parse_worktree_list_porcelain`（同时接受换行与 `-z` 的 NUL 分隔；首条为主工作区；无 branch 的非 bare 条目视为 detached；全零 HEAD 视为 unborn）、`GitVersion`（`WORKTREES = 2.17`、`WORKTREE_LIST_Z = 2.36`）、`default_worktree_path`。`RepoSnapshot` 新增 `worktrees` / `submodules` 与 `current_worktree()` / `main_worktree()` / `worktree_of_branch()`（"其他" worktree）。
  - 快照：仅当 `<common>/worktrees` 非空时才同步执行 `git worktree list --porcelain [-z]`（git 路径与版本进程内只探测一次），否则由 probe 合成唯一的主工作区条目，常见仓库零额外子进程；存在的路径做规范化以便与 `workdir` 比较。子模块仅在存在 `.gitmodules` 时经 git2 读取。
  - `GitOp::{AddWorktree, RemoveWorktree { force }, LockWorktree { reason }, UnlockWorktree, PruneWorktrees}` → `git worktree add [-b <n>|--detach] -- <path> [<rev>]` / `remove [--force] --` / `lock [--reason] --` / `unlock --` / `prune --verbose`。`GitFailure::{BranchCheckedOutInWorktree { branch, path }, WorktreeDirty, WorktreeLocked}`（解析 `is already checked out at` / `is already used by worktree at` 及 `branch -d` 的两种写法）。
  - `ModalLayer::Worktrees`（`worktrees_view.rs`）：`●` 当前、`[main]` / `[locked: 原因]` / `[prunable]` 标签；`Enter`/`o`/双击 打开（经 Issue 09 切换）、`n` 新建、`l` 锁定（可填原因）/ 解锁、`d` 删除（主 / 当前 / 已锁定拒绝；确认框说明分支保留；git 报脏时二次确认 `--force`）、`p` 清理、`y` 复制路径。
  - 新建：单选（从 HEAD / 某提交新建分支、已有空闲本地分支、游离 HEAD）→ 位置输入，默认 `<父目录>/<仓库名>-<分支名>`（`[worktree] default_parent` 可配置，支持 `~/`；已存在且非空的路径拒绝）；成功后通知栏提供"打开 '<名称>'"。
  - 分支联动：分支浮窗行右侧 `⎇ <worktree>`、顶部 `⎇ Working Trees…` 行；`branch_checkout` 对被其他 worktree 检出的分支返回禁用原因（键盘 / 菜单 Checkout 一致置灰），菜单改为提供 `Open in Worktree '<name>'`；空闲分支提供 `New Worktree…`；Log 提交菜单 `New Worktree from Here…`；`BranchCheckedOutInWorktree` 失败弹窗可一键打开该 worktree。
  - 版本检测：`EngineHandle::git_info` 解析为 `AppState.git_version`；git < 2.17 时隐藏所有菜单 / 浮窗入口，`<A-w>` 仍列在 `?` 帮助中（文案注明需要 git ≥ 2.17），执行时提示当前版本。
  - 测试：核心解析器（主 / bare / detached / locked 含原因 / prunable / `-z` / CRLF）、版本门槛、默认路径、失败分类；引擎编译参数单测与真实仓库集成（新建三种模式 → 重复检出被拒并分类 → 列表可见且从 linked worktree 加载的快照识别自身 → 锁定后删除被拒 → 解锁 → 脏工作区需 force → 删除目录后 prunable → prune；子模块列表）；`TuiSession` 覆盖模态全部操作、新建流程与路径校验、分支联动与失败弹窗、旧版 git 隐藏。
