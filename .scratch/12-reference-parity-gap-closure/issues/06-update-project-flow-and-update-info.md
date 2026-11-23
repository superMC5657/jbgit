# 06: Update Project 流程与更新摘要（Update Info）

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M9 — reference 缺口补齐
Priority: P1

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/update/{GitUpdateProcess.java,GitUpdateSession.kt,GitUpdateExecutionProcess.kt,GitUpdateOptionsDialog.kt,GitUpdateInfoAsLog.kt}`
  - `reference/platform/vcs-impl/src/com/intellij/openapi/vcs/update/UpdateInfoTree.java`
- **关联文件**：
  - `crates/jbgit-core/src/git_ops/ops.rs`（现有 `Fetch` / `Pull { rebase }` / `UpdateBranch` / `Smart`）
  - `crates/jbgit-engine/src/ops.rs`（`run_smart` 保存 / 恢复本地改动）
  - `crates/jbgit-tui/src/app/git_workflows.rs`、`crates/jbgit-tui/src/keymap/catalog.rs`、`crates/jbgit-tui/src/log_view/filter_bar.rs`

---

## 1. 背景与目标

IDEA 的 Update Project（`Ctrl+T`）一次性完成：保存本地改动 → fetch → 当前分支按 merge/rebase 同步 upstream → 其他有 upstream 的本地分支快进 → 恢复改动 → 展示"本次更新进来了哪些提交"。`jbgit` 目前只有分散的 Fetch / Pull / 单分支 Update。

## 2. 任务清单

- [x] `GitOp::UpdateProject { mode: UpdateMode { Merge, Rebase }, save_changes: bool, update_other_branches: bool }`；`mode` 默认读取 `pull.rebase`，可被 `jbgit.toml` `[update] mode = "merge" | "rebase" | "git-config"` 覆盖。
- [x] 执行器（`jbgit-engine/src/ops.rs` 新增 `run_update_project`）按顺序编排已有步骤，每步写入控制台：
  1. 记录更新前快照：`HEAD` 及所有带 upstream 的本地分支 tip。
  2. 若有本地改动且 `save_changes` → 复用 `run_smart` 的 stash 保存逻辑。
  3. `fetch --all --prune`。
  4. 当前分支：`merge --no-edit @{u}` 或 `rebase @{u}`；冲突时**停止**并保留 stash（通知说明改动在哪个 stash，冲突解决后可由横幅继续并自动 pop）。
  5. 其他分支（`update_other_branches`）：仅当能快进时 `fetch . <upstream>:<branch>` 形式更新，不能快进的列入摘要"已跳过"。
  6. 恢复 stash；pop 冲突时转入现有冲突流程。
- [x] `UpdateInfo { ranges: Vec<(branch, before: Oid, after: Oid)>, skipped: Vec<(branch, reason)>, new_commits: usize, files_changed: usize }` 作为 `GitOpOutcome` 的附加载荷返回。
- [x] 完成通知："更新 N 个提交，M 个文件 — 查看"；"查看"打开 Log 并应用范围筛选 `before..after`（筛选栏新增只读 Range 芯片，可一键清除），对齐 `GitUpdateInfoAsLog`。
- [x] 入口：全局键位 `<A-u>`（三套预设一致）、分支浮窗顶部 "Update Project…"、头部状态栏 behind 计数可点击。**jetbrains 预设的 `<C-t>` 保持绑定 `GitPull` 不变**（`config/keymap_config.rs` 约 `:422`，2026-11-23 维护者拍板：不改变已有用户习惯）；用户可在 `keymap.toml` 中自行改绑到 `update_project`，`docs/manual.md` 给出示例。

## 3. 测试

- 集成：远端前进 2 个提交 + 本地有未提交改动 → 更新后改动保留、`UpdateInfo.new_commits == 2`。
- 当前分支冲突时停止且 stash 未丢失；其他分支不可快进时进入 `skipped`。
- `TuiSession`：通知"查看"后 Log 只显示更新范围内的提交。

## Comments

- 2026-11-24 实现说明：
  - 偏离：`mode` 字段为 `Option<UpdateMode>`，`None` 表示遵循 Git 配置（引擎依次读取 `branch.<name>.rebase`、`pull.rebase`），对应 `jbgit.toml` `[update] mode = "git-config"`（默认）。另增 `[update] save_changes` / `update_other_branches`（默认均为 `true`）。
  - 当前分支使用其 upstream 短名（如 `origin/main`）执行 `merge --no-edit` / `rebase`，而非 `@{u}`，使合并说明可读；无 upstream / 游离 HEAD / upstream 已被删除时在 stash 之前直接失败。
  - `UpdateInfo` 额外携带 `commits: Vec<Oid>`（各区间 `rev-list after ^before` 的去重并集，上限 10 000），`new_commits` 即其长度；Log 的只读 Range 芯片据此按提交集合过滤，因此变基后旧 tip 不在图中也能正确显示。
  - 冲突后保留的 `jbgit update project` 贮藏：TUI 记录 `pending_update_stash`，在 `Continue` / `Abort` 成功且无进行中操作时自动 `stash pop`。stash pop 自身冲突时不自动处理（贮藏保留，提示用户）。
  - 入口：`<A-u>`（写入默认 keymap 的 `[global]`，三套预设继承）、分支浮窗首行 `Update Project…`（模式单选对话框）、头部 `↓N` 计数可点击（打开同一对话框）。jetbrains 预设 `<C-t>` 仍为 `GitPull`。
