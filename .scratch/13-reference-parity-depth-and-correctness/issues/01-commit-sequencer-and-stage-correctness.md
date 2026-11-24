# 01: Commit Sequencer 状态、Symlink/模式与 Worktree Hooks 正确性修复

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M10 — reference 深度对齐与正确性加固
Priority: P0

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/checkin/GitRepositoryCommitter.kt`
  - `reference/plugins/git4idea/backend/src/index/GitIndexStatusUtil.kt`、`GitIndexUtil.java`、`vfs/GitIndexVirtualFile.kt`
  - `reference/plugins/git4idea/backend/src/index/ui/GitStageDiffUtil.kt`
- **关联文件**：
  - `crates/jbgit-engine/src/supervisor.rs`（`execute_commit` 路径选择 `:1211-1213`）
  - `crates/jbgit-engine/src/stage.rs`（`execute_commit_git2_sync`、`append_signoff_if_needed`、`probe_commit_precheck_sync`、`apply_stage_buffer_sync`、`stage_targets_sync`、`discard_targets_sync`）
  - `crates/jbgit-engine/src/blob_reader.rs`（`read_worktree_path`）
  - `crates/jbgit-engine/src/pool/stage_status.rs`、`crates/jbgit-core/src/details.rs`、`crates/jbgit-tui/src/views/commit_diff.rs`

---

## 1. 背景与目标

对照 `git4idea` 源码审计发现 4 处会直接造成提交历史或工作区数据损坏的正确性缺陷：
1. 在无 commit hooks 且未开 `commit.gpgsign` 的仓库中，如果当前处于 `Merge` / `CherryPick` / `Revert` / `Rebase*` 状态，通过 Commit 面板提交会落入 `execute_commit_git2_sync`，仅写入单父 `HEAD` 提交且不清理 `MERGE_HEAD`/`CHERRY_PICK_HEAD`，直接破坏合并拓扑；
2. `read_worktree_path` 与 `stage_targets_sync` 跟随符号链接而非使用 `symlink_metadata` / `read_link`，导致悬空符号链接在暂存时被误调用 `index.remove_path` 删除，局部暂存也丢失 `0o120000` 模式；
3. `probe_commit_precheck_sync` 检查默认 hooks 目录时读了 `location.git_dir.join("hooks")`，在 linked worktree 中 hooks 位于 `common_dir.join("hooks")`，导致 worktree 下漏检 hooks 误走 `git2` 跳过钩子；
4. `discard_targets_sync` 仅调用 `fs::remove_file`，对大仓模式下未递归展开的未追踪目录静默无操作；且 Diff 不展示文件权限/类型变更与 `\ No newline at end of file`。

## 2. 任务清单

- [ ] **Commit Sequencer 状态走 CLI 路径**：
  - `supervisor.rs` 在 `repo.state() != RepositoryState::Clean`（`Merge`、`CherryPick`、`Revert`、各类 `Rebase`）或存在 `MERGE_HEAD` / `CHERRY_PICK_HEAD` / `REVERT_HEAD` 时强制走原生 `git commit` CLI（支持多父提交并自动清理 sequencer 状态文件）；
  - `append_signoff_if_needed`（保留给纯 Clean 状态的 `git2` 快路径）对齐 `git commit --signoff` trailer 规则：末尾已有 `Key: value` trailer 块时紧接换行追加，不再额外插入空行。
- [ ] **Linked Worktree Hooks 探测修复**：
  - `probe_commit_precheck_sync` 在未配置 `core.hooksPath` 时改查 `location.common_dir.join("hooks")`（主仓中 `common_dir == git_dir`，linked worktree 中指向公共 hooks 目录）；
  - 当 `precheck.has_commit_hooks == false` 时，Commit 面板的 Hooks 复选框显示为灰色/提示无钩子（对齐 `GitSkipHooksCommitHandlerFactory`），避免误导。
- [ ] **Symlink、文件模式与未追踪目录处理**：
  - `blob_reader.rs::read_worktree_path` 改用 `fs::symlink_metadata`；若为符号链接则读取 `fs::read_link` 目标路径字节并标记 `is_symlink`（不再跟随目标文件或把悬空链接判为 Missing）；
  - `stage_targets_sync` 用 `fs::symlink_metadata(&full).is_ok()` 判定路径是否存在，修复悬空符号链接被误删；
  - `apply_stage_buffer_sync` 遇到符号链接/类型变更（`0o120000`）拒绝行级局部写入并返回明确提示；对普通文本局部暂存时优先经 `git hash-object -w --stdin --path=<rel>` 或应用 `autocrlf`/clean filter（或在保留 `index.add_frombuffer` 时至少检查 `core.autocrlf` / `.gitattributes` filter 并对有 clean filter 的文件回退到 `hash-object --path`）；
  - `discard_targets_sync` 对未追踪目录安全执行 `fs::remove_dir_all`（校验路径在 workdir 内且不跟随外链）；
  - 在 `FileDiff` / `ThreeDocStageDiff` 增加模式与类型变化元数据头（如 `100644 → 100755`、`symlink target`）及 `\ No newline at end of file` 行尾标记展示。
- [ ] **Copy 与双段重命名补齐**：
  - `stage_status.rs` 启用 copy 检测并映射 `FileStatusKind::Copied`；`load_stage_file_diff_sync` 独立解析 `HEAD` / `Staged` / `Local` 三侧路径（对齐 `GitFileStatus.kt`）。

## 3. 测试

- **真实仓库集成测试（修复前必挂）**：
  - 无 hooks 仓库执行 `git merge --no-commit` → 通过引擎 `Commit` 提交 → 断言新 `HEAD` 有 **2 个父提交** 且 `.git/MERGE_HEAD` 已清理；
  - Linked worktree 在主仓 `.git/hooks/pre-commit` 放置失败钩子 → `probe_commit_precheck_sync` 检出 `has_commit_hooks == true` 且提交被钩子拦截；
  - 创建指向不存在目标的悬空符号链接 → `stage_targets_sync` 成功暂存 `0o120000` 条目而非删除；
  - 创建折叠未追踪目录 → `discard_targets_sync` 完整清理目录；
  - 文件末尾无换行符 & `chmod +x` 纯权限变更 → Diff 正确显示提示而非"No textual differences"。
- **纯函数单元测试**：`append_signoff_if_needed` 在普通正文、已有 `Co-authored-by:` trailer、已包含相同 `Signed-off-by:` 时的输出校验。
