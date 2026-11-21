# 04: 引擎双优先级通道、Watcher 线程池隔离与草稿异步写盘

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M7 — 性能架构治理与交互零卡顿优化 (P1)

- **关联文件**：
  - `crates/jbgit-engine/src/pool.rs`
  - `crates/jbgit-engine/src/supervisor.rs`
  - `crates/jbgit-engine/src/watcher.rs`
  - `crates/jbgit-tui/src/stage_view.rs`
  - `crates/jbgit-tui/src/app_stage.rs`
  - `crates/jbgit-tui/src/app.rs`
  - `crates/jbgit-tui/src/main.rs`

---

## 1. 背景与目标

当前后台执行链路与磁盘写入存在两处严重干扰交互响应的问题：
1. **单 FIFO 通道饿死与 Watcher 抢占**：`Git2WorkerPool::new(4)`（`crates/jbgit-engine/src/pool.rs:58-90`）仅使用一条无界 FIFO `crossbeam_channel`，导致 `LoadRepoSnapshot`（5 万提交）、`LoadStageStatus`（全工作区扫描）与 `LoadPathMatches`（逐提交 `diff_tree_to_tree`）和用户正在等待的交互式 `LoadFileDiff` / `LoadStageFileDiff` 互相堵在同一条队里；同时 `watcher.rs:239` 在收到非硬编码目录的工作区文件系统事件时，还会直接在回调里**同步调用 `pool.is_path_ignored_sync(...)` 抢占这 4 个 worker 槽位**。当外部 `cargo build` / `npm` / IDE 产生文件事件时，切换 Diff 预览会明显被排长队。
2. **提交编辑器逐字符同步写盘**：`app_stage.rs:482-509` 每次处理 `CommitEditorChar`、`Backspace`、`Newline`、`Paste` 等事件都会立刻调用 `stage_view.rs:495-509` 的 `save_draft()` 在 UI 主线程同步 `std::fs::write` / `remove_file` `.git/jbgit-commit-draft`；在 Windows Defender 等安全软件实时扫描 `.git/` 的环境下会导致打字丢帧。

本 Issue 建立引擎高优交互/后台扫描双优先级调度、将 Watcher 忽略判断彻底隔离出主 worker 池，并将草稿保存改为防抖异步落盘。

---

## 2. 核心任务清单

- [ ] **将 `Git2WorkerPool` 升级为双优先级（Interactive / Background）调度（`crates/jbgit-engine/src/pool.rs`, `supervisor.rs`）**：
  - 将单一 `crossbeam_channel::unbounded` 拆分为 `high_tx / high_rx`（交互优先队列）与 `bg_tx / bg_rx`（后台扫描队列）。
  - Worker 线程主循环采用 `crossbeam_channel::select_biased!` 优先消费 `high_rx`（并至少保留一定并发隔离或专用交互保证，例如确保即使所有后台队列积压满 4 个重任务，交互请求也能在最短时间获得线程调度，或为交互池保留保底线程）。
  - 在 `supervisor.rs` 明确分级：
    - **High（Interactive）**：`LoadFileDiff`、`LoadStageFileDiff`、`LoadCommitDetails`、`LoadFileHistory`、`LoadConflictDocument`、`ApplyStageBuffer`；
    - **Background**：`LoadRepoSnapshot`、`LoadStageStatus`、`LoadPathMatches`、`LoadAmendPreview`、`ProbeCommitPrecheck`。
  - 确保已过期 epoch 任务在出队第一时间及执行节点尽早跳过，零浪费后台槽位。
- [ ] **将 `RepoWatcher` 从共享 `Git2WorkerPool` 彻底隔离（`crates/jbgit-engine/src/watcher.rs`, `supervisor.rs`）**：
  - 移除 `RepoWatcher::start` 对 `Arc<Git2WorkerPool>` 的持有以及 `is_relevant_worktree_path` 对 `pool.is_path_ignored_sync(...)` 的调用。
  - 为 worktree 事件过滤配置专属轻量化检查器（在 watcher 回调内部或独立专用单工作线程缓存一个可复用的 `git2::Repository` 句柄 + 短 TTL/按父目录缓存的 `is_path_ignored` 查询结果），对同一批海量 FS 事件先按静态黑名单与目录前缀去重再查 `.gitignore`。
  - 完整保留既有 `MutationGuard` 抑制自身写操作触发风暴与防抖语义（`100 ms` `.git/` 元数据 / `250 ms` 工作区文件）。
- [ ] **提交说明草稿改为 300 ms 防抖异步持久化（`stage_view.rs`, `app_stage.rs`, `app.rs`, `main.rs`）**：
  - 在 `StageWorkbenchState` 将直接写盘替换为脏标志与截止时间（`draft_dirty: bool`、`draft_save_deadline: Option<Instant>`，以及 `DRAFT_DEBOUNCE = Duration::from_millis(300)`）。
  - 每次文本修改仅标记 `mark_draft_dirty(now)`，并把 `draft_save_deadline` 并入 `AppState::next_deadline()`；到期触发时捕获当前草稿快照通过后台异步任务（或 Tokio blocking 队列带序号防乱序）写盘/删文件，不在 UI 主线程同步执行磁盘 IO。
  - **数据安全兜底**：在执行 `DispatchCommit`（提交发出）、切换 `Amend`、外部编辑器交互或退出主循环（`main.rs` 跳出 `while !state.should_quit` 后）之前，统一调用 `flush_draft_sync()` 确保最新编辑内容无损落盘或按预期清理。
- [ ] **编写并发优先级与防抖写盘测试**：
  - `jbgit-engine` 优先级调度测试：模拟向 `Background` 队列投递多个长耗时任务占位/排队时，随后投递的 `High` 交互任务能优先抢占空闲/下一个可用槽位完成，不被 FIFO 队尾阻塞。
  - `jbgit-engine` watcher 测试：验证 `RepoWatcher` 对 `.gitignore` 规则的过滤准确性以及大量连续修改时不占用 `Git2WorkerPool` 通道。
  - `jbgit-tui` 草稿测试：验证连续输入 20 个字符不会触发同步文件写入，等待防抖到期或模拟退出/提交时文件准确落盘；当正文被清空后文件被删除。

---

## 3. 验收标准 (Acceptance Criteria)

- [ ] `cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
- [ ] 在仓库执行构建或其他进程高频产出文件事件期间，`RepoWatcher` 不再调用 `Git2WorkerPool::is_path_ignored_sync`，交互 `LoadFileDiff` / `LoadStageFileDiff` 不被重型扫描任务排死。
- [ ] 在 `[1 Commit]` 提交信息框高速连续输入与删改期间，UI 主线程不再产生逐按键 `std::fs::write` 调用，且关闭或提交时草稿状态完全一致无丢失。

---

## Comments

- 2026-11-21 (agent): 基于引擎池通道架构、watcher 同步阻塞点与提交编辑器磁盘写盘审计创建本 P1 并发/IO 隔离工单。
