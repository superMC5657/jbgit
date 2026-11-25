# 01: Smart Merge 被阻挡文件名解析（2 空格缩进）与 Stash 恢复失败分类修复

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M12 — 工作流正确性与 UI 交互加固
Priority: P2

- **关联文件**：
  - `crates/jbgit-core/src/git_ops/outcome.rs`（`classify_failure` 中的 `collect_files` `:118-154`）
  - `crates/jbgit-engine/src/ops.rs`（`Runner::run_smart` `:2654-2697`）
  - `crates/jbgit-tui/src/app/git_workflows.rs`（`apply_git_outcome` `:2756-2770`, `:2839-2871`）
  - `crates/jbgit-tui/src/i18n/locales/zh-CN.toml` / `en-US.toml`

---

## 1. 根因定位

### 1.1 Smart Merge 弹窗在 `git merge` ort 因已修改/暂存文件失败时渲染空白列表行
- 在 `crates/jbgit-core/src/git_ops/outcome.rs:118-138` 中，`collect_files` 仅识别以 `\t` 或 **4 个空格 (`"    "`)** 开头的文件名行：
  ```rust
  if line.starts_with('\t') || line.starts_with("    ") {
      files.push(line.trim().to_string());
  }
  ```
- 现代 Git `ort` 合并策略在 `git merge` 失败输出中，对被阻挡的已修改/暂存文件使用 **2 个空格缩进**：
  ```text
  error: Your local changes to the following files would be overwritten by merge:
    staged_partial.txt
  Merge with strategy ort failed.
  ```
- 这导致 `collect_files("would be overwritten by")` 返回空 `Vec`；随后 `crates/jbgit-tui/src/app/git_workflows.rs:2799-2810` 用 `files.len().max(1)` 显示标题 `合并 'feature/linear-chain' 将覆盖 1 个文件中的本地修改：`，紧接着输出 0 个文件名并追加一个空行，用户看不到具体被阻挡的文件。

### 1.2 `Smart` 操作后 `git stash pop --index` 因未跟踪文件碰撞失败时误判为合并冲突
- 在 `crates/jbgit-engine/src/ops.rs:2684-2695` 中，`Runner::run_smart` 执行完核心操作（如 `git merge --ff-only` 成功）后调用 `self.stash_restore().await`（即 `git stash pop --index`）：
  ```rust
  if !pop.ok() {
      res.outcome.stash_kept = true;
      if res.outcome.success {
          res.outcome.success = false;
          res.outcome.failure = Some(GitFailure::Conflicts);
      }
  }
  ```
- 当 `git stash pop` 失败原因是**未跟踪文件已存在**（例如 `linear_chain.txt already exists, no checkout` / `error: could not restore untracked files from stash`）时，工作区与索引中不存在任何 `UU`/`UD` 未合并冲突条目，也没有进行中的 merge 状态。
- 上述代码无条件写入 `GitFailure::Conflicts`，导致 `git_workflows.rs:2839-2870` 弹出错误引导 `请先解决冲突，然后点击横幅上的继续。本地修改已保存在 stash@{0}。 [解决冲突]`——既无进行中横幅，点击 `[解决冲突]` 也无冲突可解。

---

## 2. 任务清单

- [x] **支持 2 空格缩进的 Git 覆盖提示文件名解析**：
  - 修改 `crates/jbgit-core/src/git_ops/outcome.rs:126-135` 中的 `collect_files`：
    - 将缩进判定放宽为 `line.starts_with('\t') || line.starts_with("  ")`，同时过滤掉已知非路径结束提示行（如 `"Please commit your changes"` / `"Aborting"` / `"Merge with strategy"` / `"error:"` / `"fatal:"` / `"hint:"` 等）；
    - 确认 `UntrackedOverwritten` 与 `LocalChangesOverwritten` 两条路径均能正确提取 `\t`、2 空格、4 空格缩进的多行文件名。
  - 在 `crates/jbgit-tui/src/app/git_workflows.rs:2799-2809`：当 `files.is_empty()` 时避免输出孤立空行或给出明确兜底提示。
- [x] **区分 `stash_restore` 的真实合并冲突与非冲突恢复失败**：
  - 在 `crates/jbgit-engine/src/ops.rs:2684-2696`：检查 `pop.all_lines()` 是否包含真实冲突信号（如 `classify_failure(&pop.all_lines())` 为 `GitFailure::Conflicts` 或存在 `CONFLICT` / unmerged 提示）：
    - 确有冲突时，保留 `GitFailure::Conflicts` + `stash_kept = true`；
    - 非冲突失败（如 `already exists, no checkout` / `could not restore untracked files from stash`）时，保持主操作成功或设置明确的 `GitFailure::Other(...)` 并保留 `stash_kept = true`，不再伪造 `GitFailure::Conflicts`。
  - 核对 `crates/jbgit-tui/src/app/git_workflows.rs:2756-2770`：主操作已成功但 `stash_kept == true` 且非冲突时，展示带 `[恢复贮藏]`（`toast.unstash_btn`）或清晰说明“操作已完成，但本地修改未能自动恢复，仍保存在 `stash@{0}`”的通知，不再展示 `[解决冲突]`。

---

## 3. 测试与验收

- **单元测试（`jbgit-core`，修复前必挂）**：
  - 在 `crates/jbgit-core/src/git_ops/outcome.rs` 补充 `classify_failure` 测试：输入真实 `git merge` ort 2 空格缩进错误（含单文件与多文件、`Merge with strategy ort failed.` 尾行），断言返回 `GitFailure::LocalChangesOverwritten { files: vec!["staged_partial.txt"] }` 且不含尾行噪声；同时覆盖 `\t` 与 4 空格缩进回归用例。
- **引擎 / TUI 测试（修复前必挂）**：
  - 模拟 `run_smart` 主操作（`git merge --ff-only`）成功但 `stash pop` 输出 `linear_chain.txt already exists, no checkout` / `error: could not restore untracked files from stash` 的场景，断言 `outcome.stash_kept == true` 且 `outcome.failure` **不是** `Some(GitFailure::Conflicts)`；
  - TUI 测试断言 `apply_git_outcome` 不弹出带 `Action::OngoingResolve`（`[解决冲突]`）的通知。
- **Pilot 定向复验**：
  - 在 `full` 沙盒复现脏 `staged_partial.txt` 触发 `智能合并` 弹窗，确认弹窗明确列出 `  • staged_partial.txt`；
  - 复现未跟踪 `linear_chain.txt` 触发 `Smart Merge --ff-only`，确认快进成功且底部通知准确提示 `stash@{0}` 未自动恢复，而非提示“请先解决冲突”。

## Comments

- **2026-11-25**: 已修复并验证：
  - `jbgit-core`: `collect_files` 支持 `\t`、2 空格、4 空格缩进并过滤 Git 结束提示行；`git_workflows.rs` 在 `files.is_empty()` 时使用通用兜底文案 `dialog.smart.overwrite_prompt_generic` 且不输出孤立空行。
  - `jbgit-engine`: `run_smart` 与 `run_update_project` 通过 `stash_restore_has_conflicts` 仅在真实合并冲突时标为 `GitFailure::Conflicts`，未跟踪文件碰撞保留主操作成功状态与 `stash_kept = true`。
  - **测试**：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace --locked` 全量通过（含 `classify_local_changes_conflicts_and_unmerged`、`smart_merge_untracked_stash_restore_failure_is_not_classified_as_conflicts`、`smart_merge_real_stash_restore_conflict_still_classifies_as_conflicts`、`smart_merge_success_with_stash_kept_offers_unstash_not_resolve_conflicts`）。
  - **Pilot 实机复验（`full` 沙盒）**：脏 `staged_partial.txt` 合并 `feature/linear-chain` 时 `智能合并` 弹窗明确列出 `  • staged_partial.txt`；在 `test/ff-base`（`feature/linear-chain~5`）放置未跟踪 `linear_chain.txt` 后执行 `git merge --ff-only` → `智能合并`，`HEAD` 快进至 `a6c8a54`（工作区干净，无未合并冲突），底部通知栏显示 `✕ 本地修改未能自动干净还原，已保存在 stash@{0}  [还原贮藏]`，未误报 `[解决冲突]`。
