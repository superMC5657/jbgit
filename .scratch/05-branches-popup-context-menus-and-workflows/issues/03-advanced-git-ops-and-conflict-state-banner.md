# 03: 实现高阶 Git 写操作闭环（Reset 四模式、Cherry-Pick、Interactive Rebase、Stash 工作台与冲突横幅）

Status: ready-for-agent
Blocked by: 01, 02, 03-vcs-log-three-pane-linkage/issues/03-commit-changes-details-and-diff-linkage.md, 04-staging-workbench-and-partial-hunk/issues/03-commit-editor-amend-and-commit-workflow.md
Spec: [../spec.md](../spec.md)
Milestone: M5 — 分支浮窗、右键菜单与高阶操作

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/reset/GitResetAction.java` & `GitUncommitAction.kt`
  - `reference/plugins/git4idea/backend/src/cherrypick/GitCherryPicker.kt`
  - `reference/plugins/git4idea/backend/src/rebase/` (`GitRewordAction`, `GitSquashLogAction`, `GitDropLogAction`, `GitInteractiveRebaseAction`)
  - `reference/plugins/git4idea/backend/src/stash/ui/`

---

## 1. 背景与目标

打通所有上下文菜单与工具栏背后的高阶 Git 工程工作流执行闭环，包括 IDEA 招牌的 `Keep` 安全重置模式、`Uncommit` 无损撤回、无感环境变量驱动的 `Reword / Squash / Fixup / Drop / Interactive Rebase`、本地冲突修改自动保护（`Smart Checkout / Smart Rebase`）、远程 `Fetch / Pull / Push`、三栏冲突解决辅助，以及独立的 `[3 Stash]` 储藏箱工作台与全局进行中状态横幅。

---

## 2. 核心实现任务清单

- [ ] **Reset 四模式 (`Soft / Mixed / Hard / Keep`) 与 `Uncommit`**：
  - 实现 `GitResetDialog` 对接到 `git reset --soft|--mixed|--hard|--keep <rev>`；
  - 实现 `Uncommit`（仅对 `HEAD` 激活）：执行 `git reset --soft HEAD~1` 并将刚撤回提交的 Commit Message 自动回填进 `[1 Commit]` 编辑框，方便用户微调后重新提交！
- [ ] **Cherry-Pick 与 Revert 工作流**：
  - 支持单选或多选历史提交（自动按拓扑由旧到新排序）依次执行 `git cherry-pick`；以及 `git revert`。遇冲突时自动引导进入冲突解决视图并激活顶部状态横幅。
- [ ] **可视化与快捷历史改写（`Reword` / `Squash` / `Fixup` / `Drop` / `Interactive Rebase`）**：
  - 实现 `jbgit` 内置非交互式 `GIT_SEQUENCE_EDITOR` / `GIT_EDITOR` 回调机制：
    - **Reword**：在任意未推送（或用户确认强推）历史提交弹出编辑框修改说明，后台自动完成变基改写（若是 `HEAD` 则直接走极速 `commit --amend --only -m`）；
    - **Drop / Squash / Fixup**：一键丢弃或压缩选中提交；
    - **Interactive Rebase Modal**：以可视化表格列出 `base..HEAD` 提交序列，支持 `Alt+Up/Down` 调整顺序、单键切换 `pick / reword / squash / fixup / drop`，回车一键执行！
- [ ] **远程网络同步（Fetch / Pull / Push）与 Smart Saver 保护**：
  - 顶栏与浮窗打通异步 `Fetch --prune`、`Pull`（支持 Merge 或 `--rebase`）、`Push`（包含新分支自动 `--set-upstream`、普通推送及带安全护栏的 `--force-with-lease`、以及 `Push All up to Here`）。
  - 实现 **Smart Operation Saver**：当 `Checkout` / `Pull` / `Rebase` / `Merge` 被未提交本地改动阻拦时，弹窗支持一键自动 `Stash -> 执行目标操作 -> Unstash Pop`！
- [ ] **冲突解决视图与 `OngoingRepositoryStateBanner` 横幅**：
  - 当仓库处于 `REBASING (m/n)`、`MERGING`、`CHERRY-PICKING`、`REVERTING` 时，顶栏下方常驻琥珀色操作横幅，一键触发 `Continue`、`Skip`、`Abort`！
  - 对冲突文件支持一键 `Accept Yours (--ours)`、`Accept Theirs (--theirs)`、调用外部合并工具（`git mergetool` / `$EDITOR`）及内置三栏冲突块选择视图（左 Ours / 中 Merged 结果 / 右 Theirs）。
- [ ] **`[3 Stash]` 储藏箱管理标签页**：
  - 左栏展示全量 `stash@{n}` 列表（支持新建带说明 Stash 及包含 Untracked `-u` 选项），右栏复用变更树与语法高亮 Diff 预览 Stash 内容，支持 `Apply`、`Pop`、`Unstash as Branch…`、`Drop`、`Clear`。

---

## 3. 验收标准 (Acceptance Criteria)

- [ ] 集成测试验证：对距 `HEAD` 3 步之前的历史提交执行 `Reword` 与 `Drop`，历史树与提交说明准确改写且后续提交完整保留。
- [ ] 集成测试验证：本地存在未提交文件修改且与目标分支冲突时，触发 `Smart Checkout` 能自动暂存、切换分支并重新弹出本地修改不丢失。
- [ ] 集成测试验证：制造人为 Merge / Rebase 冲突后，顶部状态横幅准确显示当前状态与步数，点击 `[Abort]` 可 100% 干净回退至变基前原始状态。

---

## Comments

- 2026-11-18 (agent): Implemented. `GitOp` catalogue + failure classifier in `crates/jbgit-core/src/git_ops.rs`; conflict parser & 3-pane merge model in `crates/jbgit-core/src/conflict.rs`; executor in `crates/jbgit-engine/src/ops.rs` (console-streamed steps, Smart stash→op→pop, editor-free interactive rebase using a shell-builtin `GIT_SEQUENCE_EDITOR` and `exec git commit --amend -F` rewording); comparison & conflict loaders in `details.rs` / `pool.rs`. Reset 4 modes (Hard double-confirmed), Uncommit, Cherry-pick/Revert, Reword/Squash/Fixup/Drop/Interactive rebase, Fetch/Pull/Push (+ force-with-lease, Push up to here), Accept Yours/Theirs, `git mergetool` / `$EDITOR`, built-in three-pane conflict merge modal (`views/conflict_merge.rs`), ongoing banner (`views/banner.rs`), and `[3 Stash]` tab (`views/stash.rs`, reusing `CommitChangesBrowser` + `DiffPreview`).
