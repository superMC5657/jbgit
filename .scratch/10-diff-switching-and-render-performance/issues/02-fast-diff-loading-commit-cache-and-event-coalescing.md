# 02: 极速 Diff 加载路径、Commit 工作台缓存防抖与事件合并

Status: done
Spec: [../spec.md](../spec.md)
Blocked by: 01
Milestone: M7 — 性能架构治理与交互零卡顿优化 (P0)

- **关联文件**：
  - `crates/jbgit-core/src/details.rs`
  - `crates/jbgit-engine/src/details.rs`
  - `crates/jbgit-engine/src/stage.rs`
  - `crates/jbgit-engine/src/supervisor.rs`
  - `crates/jbgit-tui/src/stage_view.rs`
  - `crates/jbgit-tui/src/app_stage.rs`
  - `crates/jbgit-tui/src/app.rs`
  - `crates/jbgit-tui/src/main.rs`

---

## 1. 背景与目标

即使把语法高亮异步化，切换代码修改时仍有三条 P0 级链路直接拉长等待时间与造成交互滞后：
1. **Log / Compare 切文件重跑全树扫描**：`load_file_diff_sync`（`crates/jbgit-engine/src/details.rs:590-677`）在普通路径下每次切文件都会重跑整棵树的 `resolve_changes`（`diff_tree_to_tree` + `diff.find_similar(renames(true))`，`CompareWithLocal` 模式下甚至重新遍历整个工作区与未跟踪目录）；`load_commit_details_sync`（`details.rs:513-561`）则会为了验证 GPG 签名同步启动 `git log -1 --format=%G?` 子进程。
2. **Commit 工作台零缓存、零防抖与取消缺失**：`StageWorkbenchState`（`stage_view.rs:389-406`）没有任何 Diff/Syntax LRU 缓存，也没有 `30 ms` 防抖（`app_stage.rs:808-831`），来回切换同一修改每次都全量重算三文档 Diff；同时 `load_stage_file_diff_sync` 中段缺少 `token.is_stale()` 检查，且 `read_index_entry`（`stage.rs:134-154`）每次都遍历整份 `index.conflicts()`。
3. **主事件循环缺少输入合并（Coalescing）**：`main.rs:127-185` 每来 1 个终端事件就重绘 1 帧并触发请求，长按 `↓`/`j` 或快速滚轮后会产生严重的队列拖影。

本 Issue 建立单文件 `O(tree_depth)` 直读快路径、异步化签名校验、补齐 Commit 工作台缓存与防抖，并在主循环实现安全的事件批处理合并。

---

## 2. 核心任务清单

- [x] **改造 `load_file_diff_sync` 为直接读取目标树/Blob 快路径（`crates/jbgit-engine/src/details.rs`, `jbgit-core/src/details.rs`）**：
  - 当 `FileChange` 或 `FileDiffKey` 已知文件信息时不再调用整树 `resolve_changes`：
    - `DetailsMode::Commits`（单提交 `oids.len() == 1`）：直接查找 `commit.parent(parent_index)` 树中 `old_path.unwrap_or(&key.path)` 与 `commit.tree()` 中 `&key.path`，按 `O(tree_depth)` 直接读取新旧两份 blob；
    - `DetailsMode::CompareVersions`（`oids.len() >= 2`）：直接读取 `base_tree[old_path.unwrap_or(&key.path)]` 与 `target_tree[&key.path]`；
    - `DetailsMode::CompareWithLocal`（`!oids.is_empty()`）：直接读取 `base_tree[old_path.unwrap_or(&key.path)]` 与工作区文件 `workdir.join(&key.path)`，不再重扫工作区与未跟踪目录；
    - 多提交净差异（`Commits` 且 `oids.len() > 1`）：在 `FileChange` 中保留聚合所得 `old_oid` / `new_oid`（可选传递或经 `FileDiffKey` 元数据携带），切换文件直接 `read_blob_bytes`，仅在缺少明确 OID 元数据时回退 `resolve_changes`。
  - 兼容边界情况：文件为 `Added`（父提交树无此项）、`Deleted`（目标树无此项）、跨路径 `Renamed`/`Copied`（带 `old_path`）、以及二进制标识检查（`blob.is_binary()` 与 `\0`）。
- [x] **将 GPG 签名验证从 `load_commit_details_sync` 同步路径拆离（`crates/jbgit-engine/src/details.rs`, `supervisor.rs`）**：
  - `load_commit_details_sync` 遇到含 `gpgsig` 的提交时不再就地同步阻塞等待 `Command::new("git").args(["log", "-1", "--format=%G?", ...])` 结束，先以 `SignatureStatus::SignedUnverified`（或新增 `Pending` 状态标识）立即返回变更列表与元数据，保证用户立刻看到文件树与首份 Diff。
  - 在异步后台补充 `verify_commit_signature` 请求/事件（附 `EpochToken` 取消与小型 OID 缓存），验证完成后静默更新已渲染的 `CommitChangesBundle` 签名徽章。
  - 同时确认 `imara_line_stats` 循环内部每步检查 `token.is_stale()` 且遵守 Issue 01 的 `MAX_DIFF_FILE_BYTES`。
- [x] **为 `StageWorkbenchState` 增加 Diff/Syntax LRU 缓存与 30 ms 防抖（`stage_view.rs`, `app_stage.rs`, `app.rs`）**：
  - 在 `StageWorkbenchState` 中引入 `diff_cache: LruCache<StageDiffKey, Arc<ThreeDocStageDiff>>`（容量 `32`）、`diff_syntax: LruCache<StageDiffKey, Arc<HighlightedDiff>>`（与 Issue 01 异步高亮对齐）以及 `pending_diff: Option<(StageDiffKey, Instant)>`。
  - 在 `reconcile_stage_requests` 切换选中键时：
    - 优先查 `diff_cache`：命中时立即置为 `current_diff`（`0 ms` 切换）；
    - 未命中时写入 `pending_diff = Some((key, now + DETAILS_DEBOUNCE))` 并纳入 `AppState::next_deadline()`，等待防抖到期再在 `FlushPendingDetails` 发起 `Effect::RequestStageFileDiff`。
  - 精准失效策略：
    - 当 `StageStatusLoaded` 到达或执行 `ApplyStageBuffer` / `WriteWorktreeBuffer` / `StageTargets` / `UnstageTargets` / `DiscardTargets` / `CommitCompleted` / `AmendPreviewLoaded` 后，精确失效受影响路径（或全量状态变动时清空 `diff_cache` 与 `diff_syntax` 仅对当前选中文件重新请求），防止陈旧三文档缓存影响局部暂存行号追踪。
- [x] **强化 `load_stage_file_diff_sync` 取消点与冲突定点查询（`crates/jbgit-engine/src/stage.rs`）**：
  - 在读取三版本前后、构建 `StageLineTracker::new` 后、构建 hunks 后（以及冲突行展开循环内）定期检查 `token.is_stale()`，快速放弃已过期的 Diff 计算。
  - 优化 `read_index_entry`（`stage.rs:134-154`）：先通过 `index.get_path(Path::new(rel_path), 1..=3)` 直接查询该路径是否存在更高 stage 冲突条目，替代每次全表枚举 `index.conflicts()`（无冲突仓库/多冲突大仓库均降为 `O(log N)`）；对含空或非 stage 0 路径保留精准兼容。
- [x] **主事件循环输入与引擎事件批处理合并（`crates/jbgit-tui/src/main.rs`）**：
  - 当 `tokio::select!` 从 `event_stream` 或 `engine_rx` 醒来后，在最多单批上限 `MAX_COALESCE_EVENTS = 64` 内非阻塞排空当前已到达的事件（如使用 `futures::FutureExt::now_or_never` 配合 `event_stream.next()` 与 `engine_rx.try_recv()`）：
    - 仅当本批次全部派发合并完最终位置后，才执行一次 `terminal.draw` 与矢量图 `write_emission`；
    - 遇到 `Event::Resize`、`state.should_quit == true`、或产生需要挂起终端执行外部编辑器 `handle_external_editor_effects == true` 的动作时，立即终止当前合并批次以免误吞状态切换；
    - `Paste` 与连续文本输入按到达顺序忠实派发不乱序，仅合并冗余中间渲染帧，且通过 `reconcile_*` 防抖自动消解快速按键产生的中间 `LoadFileDiff` / `LoadStageFileDiff` 请求。
- [x] **补齐单元测试与集成验证**：
  - `jbgit-engine` 单测：验证带重命名、增删改、`CompareWithLocal`、多提交折叠场景下，新版 `load_file_diff_sync`（不跑全树 `resolve_changes`）与原行为输出完全一致；验证 `read_index_entry` 对正常文件（stage 0）与真实合并冲突文件（stage 1/2/3）均精准判定 `is_conflicted`。
  - `jbgit-tui` 状态机与事件循环测试：验证 Commit 工作台反复切换 `A -> B -> A` 第二次 0 ms 命中 LRU 缓存不发重复 `Effect::RequestStageFileDiff`；局部暂存成功后对应路径缓存准确失效；快速连续发送 20 个 `Down` 按键在防抖窗口内仅触发最终选中文件的后台请求。

---

## 3. 验收标准 (Acceptance Criteria)

- [x] `cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
- [x] `load_file_diff_sync` 在单提交、`CompareVersions`、`CompareWithLocal` 路径上不再调用 `resolve_changes`、`diff_tree_to_tree` 或 `diff.find_similar(renames(true))`。
- [x] 在含工作区未跟踪文件与较大提交历史仓库中，Log 与 Compare 模式切换单文件 Diff 耗时与工作区总文件数解耦。
- [x] Commit 工作台切回已访问文件直接命中 LRU 缓存即时显示；快速连续按 `↓`/`j` 穿越列表期间光标与画面跟手无拖影尾巴，松手 `30 ms` 内显示最终文件 Diff。
- [x] 切换至带 GPG 签名的大提交时，文件列表与首份 Diff 不被外部 `git log` 签名验证进程阻塞。

---

## Comments

- 2026-11-21 (agent): 基于诊断结果拆解为 P0 收尾工单，依赖 Issue 01 提供的异步高亮与文件大小保护机制，共同根治切换代码修改延迟。
- 2026-11-21 (agent): Issue 02 全量完成并验证：
  1. `load_file_diff_with_oids_sync` 实现 O(tree_depth) Blob 直读快路径；
  2. GPG 签名拆离异步后台任务，首次呈现 0ms 阻塞；
  3. Commit 工作台补齐 32 容积 LRU Diff/Syntax 缓存与 30ms 防抖；
  4. 主事件循环接入 64 事件上限非阻塞批处理合并；
  5. 全部单元测试与回归测试通过。
