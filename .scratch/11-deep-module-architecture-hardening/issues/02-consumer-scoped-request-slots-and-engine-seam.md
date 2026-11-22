# 02: 消费者级 `RequestSlot` 单飞取消与 `jbgit-engine` 窄接缝收敛

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M8 — 深模块架构治理与测试面收敛 (P0)

- **关联文件**：
  - `crates/jbgit-core/src/{lib,epoch}.rs`
  - `crates/jbgit-engine/src/{lib,supervisor}.rs`
  - `crates/jbgit-engine/src/pool/{mod,worker_pool}.rs`
  - `crates/jbgit-engine/tests/engine_integration.rs`
  - `crates/jbgit-tui/src/app/{state,types,effects,events,dispatch}.rs`
  - `crates/jbgit-tui/src/log_view/{details_pane,file_history}.rs`
  - `crates/jbgit-tui/src/stash_view.rs`
  - `crates/jbgit-tui/src/stage_view/workbench.rs`
  - `crates/jbgit-tui/src/graph/state.rs`

---

## 1. 背景与目标

当前异步单飞取消机制与 `jbgit-engine` 对外模块接口存在两处高风险架构摩擦：

1. **全局共享 `EpochTracker` 引发跨消费者误杀与 `"Loading diff…"` 永久卡死**：
   - `EpochTracker`（`crates/jbgit-core/src/epoch.rs:64-73`）按「任务种类」而非「UI 消费者槽位」分配单个 `RequestEpoch`（例如 `VCS Log`、`Stash`、`File History` 共用同一个 `epochs.file_diff`，`VCS Log` 与 `Stash` 共用同一个 `epochs.commit_details`）。
   - 同时，`spawn_epoch_task`（`crates/jbgit-engine/src/supervisor.rs:270-287`）在后台返回 `Err(EngineError::Cancelled)` 时静默丢弃、不发送任何回执。一旦 `File History` 发起 `request_file_diff(key_B)`，正在执行中的 `VCS Log` `request_file_diff(key_A)` 即被判为 `stale` 中途取消，而 `VCS Log` 的 `requested_diff` 永远保留为 `Some(key_A)`，切回后不再重发请求，界面永久停留在 `"Loading diff…"`。
   - 此外，代码库中并存 5 套各自手写的单飞序号方案（`EpochTracker`、`AppState` 上的 7 个 `latest_*_epoch: u64`、`SyntaxJobSlot<K>`、`DetailsPaneState::containing_epochs`、`LogGraphState::graph_epochs`），且 `CommitDetailsLoaded`（`app/events.rs:125-137`）漏掉了过期 epoch 丢弃判断，`AmendPreviewLoaded` 与 `ConflictDocumentLoaded` 完全无 epoch 保护。
2. **`jbgit-engine` 接口宽浅且缺少确定性本地替身接缝**：
   - `Git2WorkerPool`（`pool/worker_pool.rs:129-390`）暴露了 18 个与 `TaskRequest` / `EngineHandle` 1:1 重复的三行透传异步方法，`crates/jbgit-engine/src/lib.rs:11-33` 还将 20 余个内部 `*_sync` 实现函数全部 `pub use` 到根命名空间；`EngineHandle` 同时暴露 `pub epochs: EpochTracker` 与底层 `pub fn send(TaskRequest)`，调用方绕过 `send` 直接构造 `TaskRequest` 会漏增计数器。
   - 由于缺少窄且可本地替换（`local-substitutable`）的引擎接缝，`engine_integration.rs` 被迫绕过 `EngineHandle` 直接调 `*_sync` 或插入 `150ms–3s` 真实 `sleep`，TUI 测试则直接往内部 `cache` 字段塞假数据。

本 Issue 构建按消费者槽位隔离的深模块 **`RequestSlot`**，并将 `jbgit-engine` 外部接口大幅收敛为窄接缝 + 本地测试替身。

---

## 2. 核心任务清单

- [ ] **设计并落地消费者级单飞模块 `RequestSlot`（`jbgit-core/src/epoch.rs`, `jbgit-engine/src/supervisor.rs`, `jbgit-tui/src/app/*`）**：
  - 定义显式的异步消费者槽位标识 `RequestSlotId`（覆盖 `RepoSnapshot`、`StageStatus`、`LogCommitDetails`、`StashCommitDetails`、`CommitSignature`、`LogFileDiff`、`StashFileDiff`、`FileHistoryDiff`、`StageFileDiff`、`FileHistoryList`、`PathFilter`、`AmendPreview`、`ConflictDocument` 等独立消费者），或泛化统一的单飞控制器 `RequestSlot<K>`（内聚管理当前期待键 `desired: Option<K>`、在途键與代数 `(K, u64)`、协同取消令牌 `CancelToken` 及完成/取消核销逻辑）。
  - 引擎侧按 `(RequestSlotId, u64)` 追踪最新代数：同一 `RequestSlotId` 内的后发请求仅取消该槽位自身的前序任务，**绝不取消其他槽位**（如 `FileHistoryDiff` 绝不取消 `LogFileDiff` 或 `StashFileDiff`）。
  - 统一 TUI 侧的后台语法高亮任务（原 `SyntaxJobSlot<K>`）与图谱/包含分支后台任务复用同一套单飞槽位原语；**删除 `AppState` 上的 7 个 `latest_*_epoch` 字段、`EpochTracker` 结构以及 `SyntaxJobSlot`**。
  - 为 `CommitDetailsLoaded`、`AmendPreviewLoaded`、`ConflictDocumentLoaded` 补齐严格的槽位代数校验，确保快速切换文件或连续开关 `Amend` 时旧异步结果绝不覆盖新状态。
- [ ] **收敛 `jbgit-engine` 公开接口并内聚 `Git2WorkerPool`（`jbgit-engine/src/lib.rs`, `pool/worker_pool.rs`, `supervisor.rs`）**：
  - 将 `Git2WorkerPool` 的 18 个业务透传方法收敛为 `jbgit-engine` 内部私有的通用优先级任务提交方法（完整保留 Spec 10 Issue 04 的 `TaskPriority::Interactive` / `Background` 双通道 `select_biased!` 调度与过期任务早退逻辑）。
  - 清理 `crates/jbgit-engine/src/lib.rs` 的根导出：将 `Git2WorkerPool`、`MutationGuard`、`RepoWatcher` 及各子模块下的 `*_sync` 实现函数降级为 `pub(crate)`，仅公开领域请求/事件协议、配置类型、仓库探测与引擎句柄接缝。
  - 封装 `EngineHandle` 内部字段（隐藏 `epochs` 与裸 `tx`），使外部每次发起请求必定自动绑定正确的槽位令牌，从类型与可见性层面杜绝漏传 epoch。
- [ ] **建立可本地替换（`local-substitutable`）的引擎测试接缝并改造集成测试（`jbgit-engine/tests/*`, `jbgit-tui/src/app/effects.rs`）**：
  - 在引擎与 TUI `execute_effects` 交界处建立窄接缝（支持真实 `EngineSupervisor` 适配器与测试用确定性即时/可控队列替身适配器，满足「两个适配器证明真实接缝」原则）。
  - 新增确定性交错取消回归测试：模拟 `LogFileDiff(key_A)` 在途期间连续触发 `FileHistoryDiff(key_B)` 与 `StashFileDiff(key_C)`，断言三者互不取消、全部正常落盘且无任何槽位残留卡死；模拟同一槽位连续请求 `key_1 -> key_2`，断言 `key_1` 立即中止且 `key_2` 正常返回。
  - 清理 `crates/jbgit-engine/tests/engine_integration.rs` 中不必要地跨过 `EngineHandle` 调用内部 `*_sync` 函数及固定时长 `sleep` 等待的脆弱测试，改为通过 `EngineHandle` 接收确定性 `EngineEvent` 断言。

---

## 3. 验收标准 (Acceptance Criteria)

- [ ] `cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
- [ ] `AppState` 中不再存在 `latest_repo_epoch` 等 7 个散落 `u64` 计数器，`EpochTracker` 与 `SyntaxJobSlot` 被统一的槽位单飞模块替代。
- [ ] 在 `VCS Log`、`File History` 与 `Stash` 之间快速切换或交错请求 Diff / Commit Details 时，任一视图均不会因兄弟视图请求而永久停留在 `"Loading diff…"` 或 `"Loading…"`。
- [ ] `crates/jbgit-engine/src/lib.rs` 不再向外部 crate 暴露 `Git2WorkerPool` 与底层 `*_sync` 实现细节，Spec 10 的双优先级调度与 Watcher 隔离行为 100% 保持。
- [ ] `jbgit-engine` 集成测试通过公开引擎接缝与事件流完成确定性验证，无固定时长忙等 `sleep` 残留。

---

## Comments

- 2026-11-22 (agent): 基于架构审查 Candidate 03 创建本 P0 工单，修复跨视图全局 Epoch 误杀导致的 `"Loading diff…"` 卡死缺陷，并收敛 `jbgit-engine` 对外接缝。
- 2026-11-22 (agent): 已完成实现与验证：`RequestSlotId` / `SlotRegistry` / `RequestSlot<K>` / `RequestSlotTable` 替代 `EpochTracker`、`SyntaxJobSlot` 及 `AppState` 7 个散落 `latest_*_epoch` 字段；补齐 `CommitDetailsLoaded`、`AmendPreviewLoaded`、`ConflictDocumentLoaded` 槽位代数校验；收敛 `jbgit-engine` 对外接口并引入 `EngineBackend` + `RecordingEngineBackend` 接缝；全仓测试与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
