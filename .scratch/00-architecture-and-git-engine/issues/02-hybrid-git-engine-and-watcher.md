# 02: 构建混合 Git 引擎 (`git2` 只读/索引层 + `git` CLI 执行器 + 防抖监听 + Console 流)

Status: ready-for-agent
Blocked by: none
Spec: [../spec.md](../spec.md)
Milestone: M1 — 底座、主题与混合引擎

- **参考源码**：
  - `reference/plugins/git4idea/backend/resources/intellij.vcs.git.backend.xml`（`GIT_OPTIONAL_LOCKS`、`--pathspec-from-file`）
  - `reference/plugins/git4idea/backend/src/repo/`（仓库状态检测：Rebasing / Merging / Cherry-picking / Detached HEAD）

---

## 1. 背景与目标

实现 `crates/jgit-engine`，封装多线程 `git2::Repository` 句柄池、异步 `git` CLI 子进程执行器、`.git` 与工作区防抖文件监听器，以及结构化 `GitConsoleEntry` 命令审计流，为上层 TUI 提供携带 `epoch` 版本号的响应式数据快照与写操作执行能力。

---

## 2. 核心实现任务清单

- [ ] **仓库发现与状态探针 (`RepoStateProbe`)**：
  - 使用 `git2::Repository::discover` 定位仓库根目录、`.git` 目录（兼容普通仓库、Git Worktree `.git` 文件及 Submodule）。
  - 实时识别仓库中间态（`RepositoryState`）：`Clean`、`Merge`、`Rebase` / `RebaseInteractive` / `RebaseMerge`（读取 `.git/rebase-merge/msgnum` 与 `end` 获取进度 `2/5`！）、`CherryPick`、`Revert`、`Bisect` 以及是否处于 `Detached HEAD`。
- [ ] **`git2` 后台工作线程池 (`Git2WorkerPool`)**：
  - 基于固定大小工作线程 + `thread_local!` 缓存 `git2::Repository` 实例。
  - 实现请求版本号取消机制（`RequestEpoch`）：新同类只读请求到达时更新最新 `AtomicU64`，工作线程在循环遍历中检查若已过期则提前中止。
  - 提供首期基础读取能力：全量引用快照（Local/Remote/Tag/HEAD + `graph_ahead_behind` 领先落后数）、拓扑提交流读取、工作区/暂存区 `Statuses` 快照。
- [ ] **`git` CLI 异步执行器 (`GitCliExecutor`)**：
  - 自动探测系统 `git` 可执行文件路径与版本号（Windows 下正确处理 `git.exe` 无控制台闪烁标志 `CREATE_NO_WINDOW`）。
  - 统一注入安全环境变量：`GIT_OPTIONAL_LOCKS=0`（只读辅助命令）、`GIT_PAGER=cat`、`GIT_TERMINAL_PROMPT=0`。
  - 支持大量路径参数通过 `--pathspec-from-file=- --pathspec-file-nul` 标准输入安全传递。
  - 流式捕获 `stdout` / `stderr` 行、退出码与执行耗时，实时推送至 `GitConsoleLog`。
- [ ] **文件系统防抖监听器 (`RepoWatcher`)**：
  - 使用 `notify-debouncer-mini` 区分监听 `.git/` 关键元数据文件（100ms 防抖）与工作区目录（250ms 防抖，跳过 `.git/objects` 及大型忽略目录）。
  - 实现写操作期间的刷新抑制锁（`MutationGuard`）：内部 CLI 写操作执行期间暂存外部刷新信号，待写操作结束合并触发一次全量同步。
- [ ] **`[4 Console]` 终端命令审计视图接入**：
  - 在 `jgit-tui` 的 `Console` 标签页渲染命令列表、实时运行 Spinner、绿色成功 / 红色失败退出码徽标及可滚动输出详情。

---

## 3. 关键接口草案

```rust
pub enum TaskRequest {
    LoadRepoSnapshot { epoch: u64 },
    LoadCommitDetails { oids: Vec<git2::Oid>, epoch: u64 },
    LoadStageStatus { include_ignored: bool, epoch: u64 },
    RunGitCli { id: u64, args: Vec<String>, stdin_payload: Option<Vec<u8>>, refresh_after: RefreshScope },
}

pub enum EngineEvent {
    RepoSnapshotLoaded { snapshot: Arc<RepoSnapshot>, epoch: u64 },
    StageStatusLoaded { status: Arc<StageStatusSnapshot>, epoch: u64 },
    ConsoleStarted(GitConsoleEntry),
    ConsoleOutput { id: u64, is_stderr: bool, line: String },
    ConsoleFinished { id: u64, exit_code: i32, duration_ms: u64 },
    ExternalFsChanged(RefreshScope),
}
```

---

## 4. 边界情况与异常处理

- **空仓库（Unborn HEAD / `git init` 后尚无任何提交）**：不得报错崩溃，`RepoSnapshot` 应优雅标记 `is_unborn_head = true`，仍允许读取当前预备分支名并正常使用暂存区创建首次提交。
- **`.git/index.lock` 冲突**：若外部进程正持有锁，返回结构化提示并支持一键重试，严禁盲目自动删除未知 `.lock` 文件。

---

## 5. 验收标准 (Acceptance Criteria)

- [ ] 集成测试覆盖：对新建空仓库、含多个分支与标签的普通仓库、处于 `rebase` 冲突中间态的仓库，均能准确识别 `RepoState` 与 `ahead/behind` 计数。
- [ ] 集成测试覆盖：在外部修改工作区文件或执行 `git branch foo` 后，250ms 内自动收到对应的 `ExternalFsChanged` 事件。
- [ ] 在 `[4 Console]` 标签页能清晰查看每一次 CLI 调用的完整参数、输出与毫秒级耗时。

---

## Comments
