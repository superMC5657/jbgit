# 05: Merge 对话框与合并选项

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M9 — reference 缺口补齐
Priority: P1

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/merge/GitMergeDialog.kt`、`GitMergeOption.kt`、`GitMergeDialogCustomizerHelper.kt`
  - `reference/plugins/git4idea/backend/src/branch/GitMergeOperation.java`
- **关联文件**：
  - `crates/jbgit-core/src/git_ops/ops.rs`、`crates/jbgit-engine/src/ops.rs`（`GitOp::Merge` 目前固定编译为 `merge --no-edit <rev>`）
  - `crates/jbgit-tui/src/menus/branch.rs`（`merge_into`）、`crates/jbgit-tui/src/dialogs.rs`、i18n locales

---

## 1. 背景与目标

现在 "Merge 'x' into 'cur'" 直接执行快进或默认合并，无法选择 `--no-ff`、`--squash` 等常用选项，也无法编辑合并消息。

## 2. 任务清单

- [ ] `GitOp::Merge { rev, options: MergeOptions }`，`MergeOptions { ff: FastForward { Default, NoFf, FfOnly }, squash: bool, no_commit: bool, message: Option<String>, strategy: Option<String>, strategy_option: Option<String>, allow_unrelated: bool }`；`Default` 时保持现有命令行完全不变（回归保护）。
- [ ] 互斥规则以纯函数 `MergeOptions::validate() -> Result<(), MergeOptionConflict>` 表达，对齐 `GitMergeOption` 的冲突表：`squash` ⟂ `NoFf`、`squash` ⟂ `message`、`FfOnly` ⟂ `NoFf`、`FfOnly` ⟂ `no_commit`。
- [ ] 对话框：分支菜单 "Merge…"（带省略号的新入口）打开选项对话框；原无省略号的快速 Merge 保留默认行为。显示即将合并的提交数（`cur..rev`）。
- [ ] `--squash` / `--no-commit` 完成后：切到 `[1 Commit]` 标签并预填 `.git/SQUASH_MSG` / `MERGE_MSG` 到提交编辑器，通知说明"变更已暂存，等待提交"。
- [ ] `FfOnly` 失败映射为 `GitFailure::NotFastForward`，提供"改为普通 Merge / Rebase"选项。
- [ ] 记住上次选择（每仓库，存入 `<common_dir>/jbgit-merge-options`，与 `jbgit-branches-panel` 同目录约定）。

## 3. 测试

- `validate()` 的全部互斥组合单元测试；编译参数测试（含 `-s ours`、`-X theirs`、`-m` 多行消息走 `-F -` stdin）。
- 集成：`--squash` 后工作区暂存且 `SQUASH_MSG` 被预填；`--ff-only` 在分叉时失败并给出恢复选项。
