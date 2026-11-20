# 01: 单文件历史引擎（`--follow` 重命名追踪）与单文件版本恢复操作

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M6+ — 变更文件菜单与单文件历史

- **参考源码**：
  - `reference/platform/vcs-log/api/src/com/intellij/vcs/log/VcsLogFileHistoryHandler.kt`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/history/FileHistoryFilterer.kt`
- **关联文件**：
  - `crates/jbgit-core/src/details.rs`
  - `crates/jbgit-core/src/git_ops.rs`
  - `crates/jbgit-core/src/lib.rs`
  - `crates/jbgit-engine/src/details.rs`
  - `crates/jbgit-engine/src/pool.rs`
  - `crates/jbgit-engine/src/supervisor.rs`
  - `crates/jbgit-engine/src/ops.rs`
  - `crates/jbgit-engine/src/lib.rs`

---

## 1. 背景与目标

当前 `jbgit-engine` 仅提供基于 `git2::DiffOptions::pathspec` 的 `load_path_matches_sync`，该实现面向主日志的 `Paths` 集合过滤，无法跨 `git mv` 重命名追踪单文件完整演变历史，也不记录每个历史提交里该文件当时的路径（导致对改名前的旧提交无法正确拉取 Diff）。同时 `GitOp` 尚缺少从指定提交检出单个文件到工作区（对应 IDEA `Vcs.GetVersion`）的操作类型。

本 Issue 在 `jbgit-core` 与 `jbgit-engine` 层建立完整、安全、可取消的单文件历史加载管线与单文件版本恢复操作，为上层菜单与单文件历史视图提供零阻塞后端底座。

---

## 2. 核心任务清单

- [x] **在 `jbgit-core/src/details.rs` 定义单文件历史领域模型并从 `jbgit-core/src/lib.rs` 导出**：
  - `FileHistoryKey { path: String, start_oid: Option<Oid>, follow_renames: bool }`（实现 `Debug, Clone, PartialEq, Eq, Hash`）
  - `FileHistoryEntry { oid: Oid, path_at_commit: String, old_path: Option<String>, kind: FileStatusKind, additions: usize, deletions: usize, is_binary: bool, author_name: String, author_time: i64, summary: String }`
  - `FileHistoryBundle { key: FileHistoryKey, entries: Vec<FileHistoryEntry>, truncated: bool }`
- [x] **在 `jbgit-core/src/git_ops.rs` 新增 `GitOp::CheckoutPaths`**：
  - 变体：`GitOp::CheckoutPaths { rev: String, paths: Vec<String> }`
  - 命令规划：`git checkout <rev> -- <paths...>`（复用或遵循现有 `GitStep` 与 `RefreshScope::All` 约定，在 `describe()` / `is_write_op()` / `plan_steps()` 及 `crates/jbgit-engine/src/ops.rs` 完备接入）。
  - 补充 `jbgit-core` 单元测试验证 `GitOp::CheckoutPaths` 生成的参数顺序（确保 `--` 位于路径前，防止文件名与分支名冲突）。
- [x] **实现 NUL 分隔的 `git log --follow` 历史解析器与加载函数（`crates/jbgit-engine/src/details.rs`）**：
  - 实现纯函数 `pub fn parse_file_history_z(raw: &[u8], max_entries: usize) -> (Vec<FileHistoryEntry>, bool)`，解析如下只读 `git` 命令输出：
    ```text
    git -c core.quotepath=false log
        --max-count=<max_entries + 1>
        [--follow]
        -z
        --raw
        --numstat
        --format=%x1e%H%x1f%an%x1f%at%x1f%s%x1f
        <start_rev> -- <path>
    ```
  - 解析器必须正确处理以下边界情况：
    - 含空格、非 ASCII 中文字符、引号或括号的路径（依赖 `-c core.quotepath=false` 与 `-z`）；
    - 普通变更（`A` / `M` / `D` / `T`）与重命名/拷贝（`R100` / `R085` / `C100`：`old_path` 为源路径，`path_at_commit` 为目标路径）；
    - 二进制文件 `--numstat` 行（`-\t-\t<path>` -> `is_binary = true, additions = 0, deletions = 0`）；
    - 合并提交或仅变更文件权限（`0\t0`）的条目；
    - 超过 `max_entries`（默认 `500`）时截断并置 `truncated = true`。
  - 实现异步/同步加载入口 `load_file_history_sync(location: &RepoLocation, key: &FileHistoryKey, token: &EpochToken) -> Result<FileHistoryBundle, EngineError>`：
    - 强制设置 `GIT_OPTIONAL_LOCKS=0` 与 `GIT_TERMINAL_PROMPT=0`；
    - 起始版本为 `key.start_oid` 时传入其完整 40 位十六进制字符串，为 `None` 时传入 `"HEAD"`；
    - 执行前与解析阶段检查 `token.is_stale()`，过期返回 `Err(EngineError::Cancelled)`；
    - 空仓库（unborn `HEAD`）或路径不存在于历史中时优雅返回空 `entries` 而非报错崩溃。
- [x] **接入 `Git2WorkerPool` 与 `EngineSupervisor` 任务管线**：
  - `crates/jbgit-engine/src/pool.rs` 新增 `load_file_history(...)`。
  - `crates/jbgit-engine/src/supervisor.rs`：
    - `TaskRequest::LoadFileHistory { key: FileHistoryKey, epoch: u64 }`
    - `EngineEvent::FileHistoryLoaded { bundle: Arc<FileHistoryBundle>, epoch: u64 }`
    - `EngineHandle::request_file_history(&self, key: FileHistoryKey) -> u64`
    - 独立 `EpochTracker`（`file_history_epoch`），高并发连续请求时自动淘汰旧 epoch 任务，且不向 `[4 Console]` 刷屏记录只读内部查询。
- [x] **核查历史重命名文件 Diff 兼容（`crates/jbgit-engine/src/details.rs`）**：
  - 确认现有 `load_file_diff_sync` 在传入 `FileDiffKey { details: DetailsKey::single(entry.oid), path: entry.path_at_commit, context }` 时，对 `R` 重命名提交（`entry.path_at_commit` 为新路径）与改名前旧提交（`entry.path_at_commit` 为旧路径）均能准确加载 Diff 内容。
- [x] **编写单元与真实临时仓库集成测试**：
  - 纯单元测试覆盖 `parse_file_history_z`：普通增删改、带空格/中文路径、`R092` 重命名、二进制文件、超限 `truncated = true`。
  - 临时 Git 仓库集成测试：
    1. 创建文件 `a.txt` → 修改 → `git mv a.txt sub/b name.txt` → 再修改；
    2. `follow_renames = true` 时返回全部 4 个提交，且改名前提交的 `path_at_commit == "a.txt"`、重命名提交 `kind == FileStatusKind::Renamed` 且 `old_path == Some("a.txt")`；
    3. `follow_renames = false` 时仅返回重命名及之后的提交；
    4. 对每个返回的 `FileHistoryEntry` 调用 `load_file_diff_sync`，验证均能成功得到对应修改的 Hunk；
    5. 验证 `GitOp::CheckoutPaths` 能将旧提交版本的文件准确恢复到工作区。

---

## 3. 验收标准 (Acceptance Criteria)

- [x] `cargo test --workspace` 与 `cargo clippy --workspace --all-targets` 全部通过。
- [x] 真实仓库重命名链测试中，`follow_renames = true` 能完整追踪跨重命名的所有提交且 `path_at_commit` 逐提交准确。
- [x] 文件名包含空格与中文字符时，`parse_file_history_z` 与 `load_file_history_sync` 解析零错误。
- [x] 连续发起多次 `request_file_history` 时，过期请求被 `EpochToken` 正确取消，不阻塞 UI 线程也不污染控制台命令流。

---

## Comments

- 2026-11-20 (agent): 创建 Issue 01，作为 Spec 09 的引擎与领域模型底座。
- 2026-11-20 (agent): 已完成实现并验证：`jbgit-core`（`FileHistoryKey`/`FileHistoryEntry`/`FileHistoryBundle`、`EpochTracker::file_history`、`PathFilter::single`、`GitOp::CheckoutPaths`）与 `jbgit-engine`（`parse_file_history_z`、`load_file_history_sync`、跨重命名/本地比较/单提交 `load_file_diff_sync`、`Git2WorkerPool`、`EngineSupervisor`、`prepare_pathspec_args`、`ops.rs`）。`cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
