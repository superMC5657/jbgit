# 01: 消除 `with_thread_repo` 强刷 `.git/index`、`SnapshotCommitCache` 三份深拷贝与全仓 Copy 扫描

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M13 — 大仓库、渲染热路径与引擎并发性能二期治理
Priority: P0

- **关联文件**：
  - `crates/jbgit-engine/src/pool/worker_pool.rs`（`with_thread_repo` `:66-100`，`THREAD_REPOS` `:19`）
  - `crates/jbgit-engine/src/supervisor.rs`（`spawn_load_repo_snapshot` `:1354-1361`）
  - `crates/jbgit-engine/src/pool/snapshot.rs`（`SnapshotCommitCache` `:56-62`，`load_repo_snapshot_incremental_sync` `:533-542`，`try_incremental_commits_refresh` `:964-970`，`load_conflict_document_sync` `:175-378`）
  - `crates/jbgit-core/src/graph/joiner.rs`（`join_commits_impl` `:140-163`，`get_new_commits_and_saved_green_index` `:193-208`，`insert_indexed` `:457-484`）
  - `crates/jbgit-engine/src/stage.rs`（`find_head_to_index_source` `:492-535`，`find_index_to_workdir_rename_target` `:537-582`，`resolve_three_doc_paths` `:587-636`，`load_amend_preview_sync` `:1863-1924`）
  - `crates/jbgit-engine/src/pool/stage_status.rs`（`detect_staged_copies` `:17-53`，`load_stage_status_with_config_sync` `:222-235`）
  - `crates/jbgit-engine/src/pool/scan_config.rs`（`LARGE_INDEX_ENTRIES` `:9`）

---

## 范围修订（2026-11-28）

- **本轮完成**（Commit `0d94bee`）：全部三项任务，详见文末 Comments。
- **遗留**：`IncrementalJoined` 确有新增/删除提交时仍复制未变化的尾部并重建 `oid_to_index`，彻底消除需改为 `Vec<Arc<CommitSummary>>`，改动面过大，不做。

## 1. 根因定位

### 1.1 `with_thread_repo` 每次调用无条件强制重读整个 `.git/index`
- 在 `crates/jbgit-engine/src/pool/worker_pool.rs:79-89` 中，每次命中线程局部 `Repository` 缓存时，都无条件执行：
  ```rust
  let reload_failed = !cached.repo.is_bare()
      && cached.repo.index().ok().is_some_and(|mut index| index.read(true).is_err());
  ```
- 在 libgit2 中 `index.read(true)`（`force = 1`）会完全绕过 mtime/stat 校验，强行重新打开、读取、SHA-1 校验并反序列化整个 `.git/index` 二进制文件；且若该 `Repository` 此前尚未加载过 index，`cached.repo.index()` 会先读一遍，`index.read(true)` 再立即重读第二遍。
- 包括 `load_repo_snapshot_incremental_sync`、`load_commit_details_sync`、`load_file_diff_with_oids_sync`、`load_outgoing_sync`、`load_tree_sync` 等纯 ODB/revwalk/tree 查询在内的全部线程池任务都要承受这笔开销。在万级文件仓库（index 2–30+ MB，Windows NTFS/Defender 下尤其明显）上每次调用多耗时 **5–50+ ms**。
- 此外，`THREAD_REPOS`（`worker_pool.rs:19`）仅插入不淘汰，在长期使用仓库切换器时旧仓库句柄与 pack 索引会一直驻留。

### 1.2 `SnapshotCommitCache` 在 Tokio 异步线程持锁深拷贝与 `joiner.rs` 尾部深拷贝
- `crates/jbgit-engine/src/supervisor.rs:1354-1361` 在 Tokio 异步工作线程上持 `std::sync::Mutex` 对整份 `SnapshotCommitCache` 调用 `.clone()`。默认分页 `DEFAULT_COMMIT_PAGE_SIZE = 50_000` 且 `load_commit_bodies = true`，每条 `CommitSummary` 含 5 个堆分配字段（`parents: Vec<Oid>`、`author_name`、`author_email`、`subject`、`body`）。
- 进入 `load_repo_snapshot_incremental_sync` 后：
  - 即使命中 `IncrementalNoOp`（`snapshot.rs:964-970`），仍再克隆一遍 `prev.commits` 与 `prev.oid_to_index`；随后 `snapshot.rs:534` 调用 `SnapshotCommitCache::build(commits.clone(), ...)` 制造**第三份**完整拷贝并重建 `FxHashMap<Oid, usize>`。
  - 在 `IncrementalJoined` 路径，`crates/jbgit-core/src/graph/joiner.rs:156` 对未变更尾部 `saved_log[unsafe_block_size..]` 整段 `extend_from_slice` 深拷贝；且 `joiner.rs:193-204` 在传入 `oid_to_index: Some(map)` 时仍白白构造了一个从未被读取的 `existing_in_prefix: FxHashSet<Oid>`；`insert_indexed`（`:473-481`）线性扫桶并做数组 `insert` 呈 `O(K²)`。
- 在 5万–20万 commits 仓库下，每次刷新造成 **50–300+ ms Tokio 异步线程阻塞** 与 **100–400+ MB 瞬时内存尖峰**。

### 1.3 全仓 `copies_from_unmodified(true)` 扫描在点击任意 Untracked 文件时被误触发
- 在 `crates/jbgit-engine/src/stage.rs:609-629` 的 `resolve_three_doc_paths` 中，处理 `StageDiffSide::Unstaged` 时仅检查目标路径是否位于 `HEAD`（`!in_head`）。对**任何未跟踪文件（Untracked）**，`!in_head` 恒为真，于是直接调用 `find_head_to_index_source`（`stage.rs:492-535`）——在**唯一专用 Interactive 线程**上对最多 `LARGE_INDEX_ENTRIES = 20_000` 个索引条目执行 `diff_tree_to_index(include_unmodified(true))` + `find_similar(copies_from_unmodified(true))`！未跟踪文件根本不在索引中，该全仓扫描 100% 徒劳。
- 同理，`stage.rs:537-582` 的 `find_index_to_workdir_rename_target` 在暂存区文件于工作区被删除时也会跑全仓带未跟踪目录递归的 `find_similar`；`stage_status.rs:222-235` 只要存在任一 `Added` 暂存文件即对 `<= 20_000` 索引执行 `detect_staged_copies(..., true)`（`O(added × index_entries)` 相似性对比）。
- 此外，`load_amend_preview_sync`（`stage.rs:1863-1924`）与 `load_conflict_document_sync`（`snapshot.rs:175-378`）内部循环缺少 `token.is_stale()` 检查。

---

## 2. 任务清单

- [x] **优化 `with_thread_repo` 索引刷新策略与线程局部仓库缓存边界**：
  - 修改 `crates/jbgit-engine/src/pool/worker_pool.rs:66-100`：将无条件 `index.read(true)` 改为 `index.read(false)`（stat/mtime 校验），或拆分为仅在需要读取 `.git/index` 的入口（如 `stage_status`、索引读写）显式执行 `index.read(false)`，而纯 ODB / revwalk / tree / commit-diff 查询完全跳过 `.git/index` 读取；
  - 为 `THREAD_REPOS` 设置合理的数量上限（如每线程最多保留最近 4 个仓库 `git_dir` 缓存，超出时淘汰最久未用项），防止频繁切换多仓库后内存不释放。
- [x] **通过 `Arc` 消除 `SnapshotCommitCache` 多重深拷贝并优化 `joiner.rs`**：
  - 将 `SnapshotCommitCache`（`supervisor.rs`、`snapshot.rs`）及其中 `commits`（或 `CommitSummary` 切片/容器 `Arc<Vec<CommitSummary>>` / `Vec<Arc<CommitSummary>>`）改为 `Arc` 共享，确保 `spawn_load_repo_snapshot` 在 Tokio `Mutex` 内仅克隆 `Arc` 指针（`O(1)`），且 `IncrementalNoOp` 零分配复用 `prev.commits` 与 `prev.oid_to_index`；
  - 评估将 `SnapshotLoadMode` 首次加载默认 `load_commit_bodies` 或初始页策略保持与现有 UI 兼容的前提下，确保 `RepoSnapshot` 与 `SnapshotCommitCache` 共享同一份不可变提交存储，不再在 `snapshot.rs:534` 整段 `.clone()` 并重建 `FxHashMap`；
  - 修改 `crates/jbgit-core/src/graph/joiner.rs:193-204`：仅当 `oid_to_index.is_none()` 时才构造 `existing_in_prefix`；
  - 优化 `joiner.rs:457-484` `insert_indexed`：当 `K` 较大时消除 `O(K²)` 桶扫描与中部插入位移（如按有序块合并或二分区间拼接）。
- [x] **阻止 Untracked 文件触发全仓 `copies_from_unmodified` 并收紧相似度扫描阈值**：
  - 修改 `crates/jbgit-engine/src/stage.rs:609-629`：在 `StageDiffSide::Unstaged` 分支中，先校验 `staged_path` 是否真实存在于索引（且仅当它确实是已暂存的 `Added` / `Renamed` / `Copied` 且 `key.old_path.is_none()` 时），才按需探测 `HEAD -> Index` 源路径；对于**未跟踪文件**与普通工作区修改，直接跳过 `find_head_to_index_source`；
  - 收紧 `copies_from_unmodified(true)` 的触发保护（`stage_status.rs:222-235` 与 `stage.rs:505`）：将触发 `copies_from_unmodified` 的门槛下调至小规模索引且受控 `added` 数量（例如 `index_entries <= 2_000` 且 `added_count <= 32`，或配置可控），避免中等以上仓库 `git add` 批量文件时卡死状态刷新；
  - 为 `load_amend_preview_sync`（`stage.rs:1863-1924`）和 `load_conflict_document_sync`（`snapshot.rs:175-378`）的长循环/文件读取前补齐 `token.is_stale()` 中断检查。

---

## 3. 验收标准与回归测试

- **单元测试（修复前可复现，修复后必过）**：
  - `worker_pool.rs`：验证纯 ODB 查询不再触发强刷重建 `.git/index`（同时新增/修改索引条目后 `index.read(false)` 能正确感知外部 `git add` 变化），且 `THREAD_REPOS` 在打开超过上限个仓库时触发淘汰。
  - `snapshot.rs` / `joiner.rs`：验证 `IncrementalNoOp` 路径下与 `prev` 共享同一 `Arc` 底层地址（指针相等，零 `CommitSummary` 克隆）；验证 `join_commits_indexed` 在 `oid_to_index = Some(...)` 时输出与线性模式严格一致，且在 `K = 2_000` 时具备近线性吞吐。
  - `stage.rs` / `stage_status.rs`：构造 3,000+ 文件测试仓库，验证选中 `Untracked` 文件调用 `resolve_three_doc_paths` / `load_stage_file_diff_sync` 不触发 `copies_from_unmodified` 扫描且耗时显著低于旧基线；验证存在 `Added` 文件的 `load_stage_status_with_config_sync` 在超阈值仓库不阻塞。
- **全量检查**：
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace --locked`

## Comments

- **2026-11-28**：基于 `jbgit-engine` 与 `jbgit-core` 性能审计创建本 P0 工单，聚焦消除引擎线程池强制重载 index、快照多副本克隆阻塞 Tokio 与 Stage 点击未跟踪文件卡顿三大头号瓶颈。
- **2026-11-28（实现完成）**：
  - **索引刷新 / 线程局部缓存**（`crates/jbgit-engine/src/pool/worker_pool.rs`）：`with_thread_repo` 改为 `refresh_cached_index` → `index.read(false)`（libgit2 stat 戳 + 尾部校验和比对，未变化时零重解析；未加载过 index 的句柄不再"读一遍再强刷一遍"）。上一次作业返回 `Err` 时置 `force_index_reload`，下次强制 `read(true)`，丢弃失败作业可能残留的未写入内存索引修改。`THREAD_REPOS` 改为每线程 LRU（`THREAD_REPO_CACHE_CAPACITY = 4`），且执行 `f` 期间不再持有 `RefCell` 借用。
  - **快照 Arc 共享**：`RepoSnapshot::commits` 改为 `Arc<Vec<CommitSummary>>`（`crates/jbgit-core/src/repo.rs`）；`SnapshotCommitCache.commits` / `oid_to_index` 改为 `Arc`，与 `RepoSnapshot` 共享同一份提交表（删除 `commits.clone()`）；`IncrementalNoOp` 直接 `Arc::clone` 上一份表和索引；Supervisor 存 `Option<Arc<SnapshotCommitCache>>`，在 Tokio 线程持锁只克隆指针。新增 `jbgit_core::graph::join_commits_indexed_if_changed`：ref 只移动到已加载提交（新 tag/分支、快进）且没有任何提交变化时返回 `Ok(None)`，引擎复用原 `Arc`（`IncrementalJoined{0,0}`），不再 `to_vec` 整表、不再重建 `FxHashMap`。TUI 只改了测试夹具（`vec![..].into()` / `Arc::make_mut`）和两处 `for c in &snap.commits` → `.iter()`。
  - **joiner**（`crates/jbgit-core/src/graph/joiner.rs`）：`existing_in_prefix` 只在 `oid_to_index.is_none()` 时构建；`insert_indexed` 改用分块序列 `InsertedSequence`（每块 ≤64，按块维护 min_ts / max_bucket / oid 集合，加全局已插入 oid 集合），每次插入 `O(K/B + B)`，替代原来 `O(K²)` 的桶扫描加中部 `Vec::insert`；结果顺序与旧算法完全一致。
  - **Copy 扫描**：`resolve_three_doc_paths` 的 Unstaged 分支在路径既不在 `HEAD`、又存在于索引（stage 0–3）时才调用 `find_head_to_index_source`，未跟踪文件直接跳过。新增 `COPY_SCAN_MAX_INDEX_ENTRIES = 2_000`、`COPY_SCAN_MAX_ADDED = 32`、`copies_from_unmodified_allowed` 和 `StatusScanConfig::copies_from_unmodified`（`scan_config.rs`），`stage_status.rs` 与 `find_head_to_index_source` 都改用它们控制 `copies_from_unmodified`。`find_head_to_index_source` 先跑一次只含变更项的 diff 来统计 added 数量。`load_amend_preview_sync` 和 `load_conflict_document_sync` 新增 `token` 参数并补齐 `is_stale()` 检查（supervisor 已透传）。
  - **测试 / 基准**：worker_pool 3 项（未变化的 index 不强制重解析，同时能感知外部 `git add`；失败作业后强制重载；LRU 淘汰）；joiner 3 项（与旧算法在随机 DAG 上逐 OID 等价；K=2000 时 indexed 与线性模式结果一致，且与旧算法一致；K 从 1000 增到 8000 的耗时比 <30，旧算法在 flat 负载下约 55、新算法约 8–12）；`incremental_log_and_pagination.rs` 增加 `Arc::ptr_eq` 断言（NoOp、ref 快进/打 tag、快照与缓存共享）；stage.rs 1 项（3,000 项索引中选中未跟踪文件，HEAD→Index 扫描计数为 0，约 18 ms；已暂存的 add 仍然会探测）；stage_status 2 项（批量 >32 个 added、2,501 项索引时不做 copies_from_unmodified，约 20 ms）。Criterion 新增 `incremental_head_join_100k_2000_new_commits`：7.3 ms，旧实现 8.2 ms（这个规模下 100k 尾部拷贝占主要耗时）。
  - **门禁**：`cargo clippy -p jbgit-core -p jbgit-engine -p jbgit-tui --all-targets -- -D warnings` 通过；`cargo test -p jbgit-core -p jbgit-engine --locked` 与 `cargo test -p jbgit-tui --locked` 全部通过；`cargo bench -p jbgit-core --no-run` 通过。按协调方要求，没有跑全量 workspace test/clippy，留给最终统一门禁。没有执行 `cargo fmt --all`；改动过的文件逐一用 rustfmt `--check` 对比 HEAD，未新增格式差异。
  - **偏差 / 后续**：(1) Unstaged 分支的门槛是"在索引中且不在 HEAD"，而不是额外要求 `key.old_path.is_none()`，以保留双阶段重命名（HEAD:a→Index:b→Workdir:c）的源路径解析。(2) `IncrementalJoined` 在确有新增或删除时，仍会深拷贝未变化的尾部（`extend_from_slice`）并重建 `oid_to_index`；要彻底消除需要改成 `Vec<Arc<CommitSummary>>`，属于更大范围的改动，没有做。(3) `find_index_to_workdir_rename_target` 仍沿用 `LARGE_INDEX_ENTRIES` 门槛（不属于本工单任务清单）。(4) `copies_from_unmodified` 阈值目前是常量，`large_index_entries` 配置更低时以配置为准，没有新增单独的配置项。
- **2026-11-28（Code Review 修复）**：
  - `worker_pool.rs`：`with_thread_repo` 排除 `EngineError::Cancelled`，避免快速切换导航取消只读任务后触发下一次全量 `index.read(true)`（补单测 `cancelled_job_does_not_force_index_reload_on_next_call`）。
  - `stage.rs`（CS-4.1）：在所有通过 `with_thread_repo` 修改索引的 libgit2 写入入口（`apply_staged_buffer_sync`、`remove_conflict_path_sync`、`stage_targets_sync`、`stage_all_tracked_sync`、`unstage_targets_sync`、`commit_with_libgit2_sync`）入口显式执行 `index.read(true)?`，并将 `stage_targets_sync` 末尾 `let _ = repo.index().and_then(...)` 改为 `repo.index()?.read(true)?` 传播错误。
  - `scan_config.rs`：`copies_from_unmodified_allowed` 增加 `added > 0` 约束，避免 `added == 0` 时对缺失于 `HEAD` 的冲突/类型变更条目触发冗余 `include_unmodified(true)` diff（补单测 `copies_from_unmodified_requires_non_zero_added_and_bounds`）。
