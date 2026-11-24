# 06: Upstream 管理、已合并分支清理、双分支对比与 Cherry-Pick/Stash 选项

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M10 — reference 深度对齐与正确性加固
Priority: P1

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/branch/{GitUpstreamBranches.kt,GitNewBranchDialog.kt,GitBranchIsNotFullyMergedDialog.java,GitCompareBranchesUi.kt}`
  - `reference/plugins/git4idea/backend/src/ui/branch/cleanup/{CleanupBranchesAction.kt,CleanupBranchesDialog.kt}`
  - `reference/plugins/git4idea/backend/src/ui/branch/GitCheckoutAndRebaseRemoteBranchWorkflow.kt`
  - `reference/plugins/git4idea/backend/src/cherrypick/{GitCherryPicker.java,EmptyCherryPickResolutionStrategy.kt}`、`revert/GitRevertProcess.kt`
  - `reference/plugins/git4idea/backend/src/stash/ui/GitStashDialog.kt`、`ui/GitUnstashDialog.java`
- **关联文件**：
  - `crates/jbgit-core/src/{git_ops/ops.rs,git_ops/refs.rs,branch_tree.rs}`
  - `crates/jbgit-engine/src/ops.rs`、`crates/jbgit-engine/src/details/outgoing.rs`
  - `crates/jbgit-tui/src/{branches_popup.rs,dialogs.rs,menus/branch.rs,menus/log.rs,menus/stage.rs,menus/stash.rs,stash_view.rs,app/git_workflows.rs}`

---

## 1. 背景与目标

日常分支与暂存管理中还有几处高感知的“缺一步”断点：无法直接为本地分支设置/取消 Upstream；合完 PR 后缺乏批量清理已合并本地分支工具；无法任意对比两个非当前分支；远程分支菜单缺「Checkout and Rebase onto Current」；Cherry-Pick / Revert 缺少 `-n`（仅应用改动不提交）与合并提交 `-m` 父选择；Stash 缺少 `--keep-index` 与 `--index`。本工单集中补齐这些高频分支与操作选项。

## 2. 任务清单

- [ ] **Upstream 显式管理与分支操作补全**：
  - 新增 `GitOp::SetUpstream { branch, upstream: String }`（`git branch --set-upstream-to=<upstream> <branch>`）与 `GitOp::UnsetUpstream { branch }`（`git branch --unset-upstream <branch>`），接入本地分支右键菜单；
  - `RenameBranch` 对话框增加「同时取消上游追踪（Unset upstream）」复选框；
  - 远程分支菜单补齐 **Checkout and Rebase onto Current**（对齐 `GitCheckoutAndRebaseRemoteBranchWorkflow`：检出/重置本地分支并变基到当前分支）；
  - `NewBranch` 对话框支持：输入时自动清洗空格→`_`（可选关闭）、勾选「覆盖已有分支（`-B`）」与「设置追踪（`--track`）」；
  - 删除未合并分支确认框不仅显示数字，还列出前 N 条未合并提交的 `short + subject`。
- [ ] **已合并分支批量清理（`Cleanup Branches…`）**：
  - 新增 `ModalLayer::BranchCleanup`：扫描所有非当前、非受保护的已合并本地分支（支持检查相对于 `HEAD` 或目标基准分支如 `main`，标注上游已被远端删除的 `[gone]` 分支），支持多选勾选与一键批量 `GitOp::Sequence` 删除（支持删除后 Undo 恢复最近一批）。
- [ ] **任意双分支对称对比（`Compare Any Two Branches`）**：
  - 在分支菜单新增 `Compare with…`（不仅限于 `Compare with Current`）；
  - 对比视图支持一键切换方向（`A..B` 仅在 B 的提交 / `B..A` 仅在 A 的提交 / `A...B` 合并视图）以及查看两分支树差异（`git diff A..B`）。
- [ ] **Cherry-Pick / Revert 与 Stash 选项**：
  - 扩展 `GitOp::CherryPick { oids, no_commit: bool, record_origin: bool, mainline: Option<u8> }` 与 `GitOp::Revert { oids, no_commit: bool, mainline: Option<u8> }`：
    - Log 菜单提供 **Cherry-Pick** 与 **Cherry-Pick Without Committing (`-n`)**（Revert 同理）；
    - 选中合并提交时弹出父选择（`-m 1` / `-m 2`）；
    - 遇到空 Cherry-Pick 失败时解析为 `GitFailure::EmptyCherryPick`，提供「Skip / Allow Empty / Abort」恢复弹窗。
  - 扩展 `GitOp::StashPush` 支持 `keep_index: bool`（`--keep-index`），`GitOp::StashApply` 支持 `reinstate_index: bool`（`--index`）；
  - Stage 菜单「Stash Changes…」打开轻量弹窗（消息输入、`--include-untracked`、`--keep-index`、仅选中文件/全部变更），Stash Tab 支持对单个 Stash 文件执行 **Diff with Local** 与 **Apply Single File**（`git checkout stash@{n} -- <path>`）。

## 3. 测试

- **单元测试**：`SetUpstream`/`UnsetUpstream`/`CherryPick`/`Revert`/`StashPush`/`StashApply` 编译参数测试；分支名实时清洗测试；`EmptyCherryPick` stderr 分类测试。
- **引擎集成测试**：设置/取消 upstream 生效；合并分支后 `Cleanup` 准确识别已合并与 `[gone]` 分支；`cherry-pick -n` 应用改动但不产生新提交；`stash push --keep-index` 保留暂存区且 `stash apply --index` 还原暂存态。
- **TUI 会话测试**：打开 Branch Cleanup 取消勾选一条分支后批量删除；Stash 弹窗勾选 `--keep-index` 提交执行。
