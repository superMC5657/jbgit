# 06: Tokio 异步线程解阻、Windows 大工作区/并发与 Watcher 治理及 Criterion 基准补齐

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M13 — 大仓库、渲染热路径与引擎并发性能二期治理
Priority: P1

- **关联文件**：
  - `crates/jbgit-engine/src/stage.rs`（`renormalize_staged_crlf_entries_sync` `:324-419`，`unstage_path_in_index` `:1612-1703`，`execute_commit_cli_async` `:2307-2311`）
  - `crates/jbgit-engine/src/ops.rs`（`compile_patch_source` `:851-866`，`run_accept_side` / `plan_accept_side` `:998, 1774`，`run_create_patch_file` `:2108-2164`，`collect_update_info` / `run_update_project` `:2321-2596` 及同步 `Repository::open`/`fs` 段落 `:1535-2164`）
  - `crates/jbgit-engine/src/cli.rs`（`GitCliExecutor::detect_with_executable` `:145-151`，`GitCliExecutor::execute` `:245-315`，`set_configured_git_executable` / `require_git_executable` / `cached_git_executable` `:621-680`）
  - `crates/jbgit-engine/src/supervisor.rs`（`EngineSupervisor::start_with_options` `:699-706`，`spawn_load_commit_details` `:1444-1462`，`forward_cli_event` `:1938-1954`）
  - `crates/jbgit-engine/src/details/commit_changes.rs`（`verify_commit_signatures_batch_sync` `:44-109`，`resolve_compare_with_local` `:210-239`，`load_commit_details_sync` `:512-543`）
  - `crates/jbgit-engine/src/details/mod.rs`（`run_git_output_cancellable` `:50-125`）、`remote_tag.rs`（`:50-71`）、`outgoing.rs`（`:71-83`）、`tree.rs`（`:56, 185-214`）、`blame.rs`（`:146-326, 391`）、`path_matches.rs`（`:43-218`）
  - `crates/jbgit-engine/src/pool/snapshot.rs`（`collect_worktrees` `:656-680`，`collect_refs` `:831-942`，`try_incremental_commits_refresh` `:974-982`）、`stage_status.rs`（`:74-88, 237-268`）、`scan_config.rs`（`:52`）
  - `crates/jbgit-engine/src/watcher.rs`（`RepoWatcher::start` `:168-208`，`WorktreeEventFilter` / `statically_relevant_rel_path` `:350-435`）
  - `crates/jbgit-core/benches/graph_layout.rs`（基准测试覆盖扩展）

---

## 范围修订（2026-11-28）

> 当前需求以本节为准；下文原始根因与任务清单保留作审计记录，未勾选项以本节结论为准。

- **本轮完成**（Commit `9e76d21`）：
  - Watcher：`.git/objects`、`logs` 等无关路径在分类入口按首段零分配丢弃（刷新触发集合不变）；工作区过滤改为有界祖先目录缓存，`target/`、`node_modules/` 深层事件按前缀拒绝；`\\?\` 前缀与盘符大小写归一化。
  - 提交详情：纯 `Added` / `Deleted`（任一侧为空）文件直接计行，跳过完整 Histogram diff；`CompareWithLocal` 在超大索引上跳过重命名检测。
- **移入 [`07`](./07-deferred-perf-candidates.md) 待评估**：`unstage_path_in_index` 的 `O(targets × index_entries)`；`UpdateProject` / `collect_update_info` 按分支串行 spawn `git.exe`；`recurse_untracked_dirs` 默认值（产品决策，非性能修复）。
- **不做**：Tokio 线程同步阻塞迁移、启动 `git --version` 双探测、多选批量验签、`CheckRemoteTag` / `collect_refs` 取消检查、`run_git_output_cancellable` 挂 `WindowsJobGuard`（非性能项，如出现残留进程再单独立项）、CRLF 批处理、CLI 静默缓冲 / tree 流式写盘 / blame `--porcelain`、Criterion 基准补齐（仅 [`02`](./02-core-dag-edges-filter-and-visible-graph-perf.md) 补了图谱基准）。

## 1. 根因定位

1. **Tokio 异步工作线程残留多处同步阻塞与启动重复探测**：
   - `stage.rs:2307-2311`（`execute_commit_cli_async`）在 `async fn` 内直接调用 `with_thread_repo`，不仅阻塞 Tokio 线程，还会把 `CachedThreadRepo`（含打开的 pack 索引）永久泄漏到 Tokio 线程 TLS；
   - `ops.rs:998, 1535-2164`（如 `plan_accept_side`、`run_create_patch_file` 等）在 `async fn` 中直接调用 `git2::Repository::open` 与同步 `std::fs::{read, write, remove_dir_all}`；`details/tree.rs:185-191` 在 Tokio 线程拆分 10 万条文件路径；
   - `supervisor.rs:699-702` 调用 `set_configured_git_executable` 清空 `CACHED_GIT_BINARY` 后调用 `detect_with_executable` 却未回填缓存，导致随后首次在 `async fn` 中调用 `require_git_executable`（`cli.rs:646-680`）又在 Tokio 线程上再跑一次同步 `git --version` 子进程。
2. **Interactive 唯一线程队头阻塞与取消缺口**：
   - `load_commit_details_sync`（`commit_changes.rs:512-543`）对最多 `2,000` 个变更文件全部读 blob 并跑完整 Histogram diff + `postprocess_lines` + `is_file()`（哪怕纯 `Added`/`Deleted` 文件也跑完整 Diff），严重堵塞唯一的 Interactive worker；`CompareWithLocal`（`:210-239`）缺少 `LARGE_INDEX_ENTRIES` 保护；
   - 多选提交签名验证（`supervisor.rs:1446-1461`）串行启动 `N` 个 `git log -1` + `gpg` 子进程，未使用已存在的 `verify_commit_signatures_batch_sync`；
   - `CheckRemoteTag` 未接入 `EpochToken` 取消；`run_git_output_cancellable`（`details/mod.rs:59-112`）缺少 `WindowsJobGuard`（Windows 下取消后子孙进程可能残留）；`collect_refs`（5k+ tag 逐个 peel）与 `try_incremental_commits_refresh` 验尖循环无 `token.is_stale()` 检查。
3. **Windows 大工作区、子进程风暴、无界缓冲与 Watcher 放大**：
   - `unstage_path_in_index`（`stage.rs:1622-1646`）对每个目标路径线性扫整张索引且每次分配 `String`（`O(targets × index_entries)`），对目录还全树走 `head_tree`；
   - `scan_config.rs:52` 默认 `recurse_untracked_dirs = StatusScanMode::Always`；`stage_status.rs:245` 强制 `SubmoduleIgnore::None` 无视 `.gitmodules` 忽略配置；
   - `run_update_project` / `collect_update_info`（`ops.rs:2321-2596`）与 `renormalize_staged_crlf_entries_sync`（`stage.rs:386`）按分支/文件串行 spawn `git.exe`（Windows 单次创建耗时 40–100 ms）；
   - `GitCliExecutor::execute`（`cli.rs:245-315`）无限制缓冲 `stdout_lines` 并逐行经 `event_tx` 推送 `ConsoleOutput`（`CreatePatchFile` 等大导出也淹没控制台通道并 `join` 二次拷贝）；`blame.rs:391` 用 `--line-porcelain` 使输出膨胀 5×–10×；`export_tree_blob_sync`（`tree.rs:204-211`）未流式写盘；
   - `RepoWatcher`（`watcher.rs:168-208, 350-435`）递归监听整个 `.git/`（含 `objects/`）与整棵工作区（含 `target/`/`node_modules/`），`WorktreeEventFilter` 仅缓存直接父目录而非祖先目录（`IGNORE_DIR_CACHE_MAX = 4096` 易抖动），且 `statically_relevant_rel_path` 用原始 `strip_prefix` 在 Windows `\\?\` / 盘符大小写下易失效。
4. **Criterion 基准覆盖盲区**：`crates/jbgit-core/benches/graph_layout.rs` 存在三处盲区——以仅含 1 个堆字段的 `RawCommitNode` 测 `join_commits_indexed`（低估真实 `CommitSummary` 5 个堆字段）、生成全跨度固定 `17` 行的无长边/2 ref 图谱（未触发 `ultra_long_edges` 与真实 `recommended_track_width`），且缺失 `VisibleGraph::build_projected`、`edges_in_row`、`LogFilterSpec`、`ConflictDocument::three_pane_rows`、`build_change_rows`（10k 文件）与 `StageLineTracker::new_with_whitespace` 基准。

---

## 2. 任务清单

- [ ] **清除 Tokio 异步线程上的所有同步阻塞与启动双探测**：
  - 将 `execute_commit_cli_async`（`stage.rs:2307`）的 `with_thread_repo`、`plan_accept_side`（`ops.rs:998, 1774`）、`ops.rs` 中的同步 `Repository::open`/`fs`、以及 `details/tree.rs:185-191` 路径拆分统一迁移至 `spawn_blocking` 或 `Git2WorkerPool`；
  - 修改 `EngineSupervisor::start_with_options`（`supervisor.rs:699-702`）与 `cli.rs`：探测完成后同步填充 `CACHED_GIT_BINARY`，且保证 `require_git_executable` 缓存未命中路径在 blocking 线程执行，消除启动双重 `git --version`。
- [ ] **消除 Interactive 阻塞与补齐全链路取消 / Windows 进程树保护**：
  - 优化 `load_commit_details_sync`（`commit_changes.rs:512-543`）：纯 `Added`/`Deleted` 文件直接按字节计数换行符，跳过完整 `InternedInput` + `Diff::compute` + `postprocess_lines`；为 `CompareWithLocal`（`:210-239`）补充 `LARGE_INDEX_ENTRIES` 保护；
  - 在 `supervisor.rs:1444-1462` 对多选提交改用 `verify_commit_signatures_batch_sync`，并在批验签等待循环中轮询 `token.is_stale()`；
  - 为 `CheckRemoteTag`（`remote_tag.rs` / `supervisor.rs`）传入并检查 `EpochToken`；为 `run_git_output_cancellable`（`details/mod.rs`）挂载 `WindowsJobGuard`；在 `collect_refs`（tag/branch 循环）、`try_incremental_commits_refresh` 验尖循环、`load_outgoing_sync`（改为 `references_glob`）补齐 `token.is_stale()` 与增量优化。
- [ ] **治理 Windows 大工作区、子进程风暴、大流缓冲与 Watcher 风暴**：
  - `stage.rs:1612-1703`：将 `unstage_path_in_index` 重构为基于 `HashSet`/前缀匹配的 `O(targets + index_entries)` 索引查找，目录仅遍历对应子树 `repo.find_tree(entry.id())`；
  - `scan_config.rs:52` & `stage_status.rs:237-268`：将 `recurse_untracked_dirs` 默认设为 `StatusScanMode::Auto`，并在子模块状态扫描尊重 `.gitmodules` 忽略配置与 `token.is_stale()`；
  - `ops.rs:2321-2596` & `stage.rs:386`：在 `run_update_project` / `collect_update_info` 用 libgit2 `graph_descendant_of`/`graph_ahead_behind` 替代按分支串行 `git merge-base` spawn；批量 CRLF 归一化合并或通过 `git hash-object --stdin-paths` 批处理；
  - `cli.rs:245-315`、`ops.rs:2160`、`details/tree.rs:204-211`、`details/blame.rs:391`：为不需要控制台审计行的纯输出命令（如 `CreatePatchFile`、`PatchToClipboard`、`Blame`）提供静默缓冲模式（不逐行克隆发送至 `event_tx`），`export_tree_blob_sync` 改分块流式写入；评估 `load_blame_async` 改用 `--porcelain` 结合共享 `Arc<str>` 路径降低 5×–10× 传输与内存占用；
  - `watcher.rs`：精细化 `.git/` 监听（聚焦 `HEAD`、`index`、`refs`、`packed-refs`、`MERGE_*`/`REBASE_*`/`CHERRY_PICK_*` 等关键路径而非盲目递归 `objects/`），为 `WorktreeEventFilter` 实现**祖先目录缓存**与常见构建目录（`.git`、明显忽略前缀）快速过滤，修复 `statically_relevant_rel_path` 的 Windows `\\?\` / 大小写前缀归一化，延迟分配去重集合。
- [ ] **补齐 `crates/jbgit-core/benches/graph_layout.rs` Criterion 回归基准**：
  - 将 `join_commits_indexed` 基准升级为使用真实多字段提交结构（对应 `CommitSummary` 的 5 个堆字段）；
  - 构造含长边与超长边（span > 1000）及数千 ref 的真实分布 DAG，新增 `edges_in_row`、`recommended_track_width`、`VisibleGraph::build_projected`（过滤 + LinearBek）、`LogFilterSpec::compute_matches_with_favorites`（文本/正则/hex 前缀）、`ConflictDocument::three_pane_rows` / 窗口投影、`build_change_rows`（10k 文件）与 `StageLineTracker::new_with_whitespace` 基准。

---

## 3. 验收标准与回归测试

- **单元测试（修复前可复现，修复后必过）**：
  - `stage.rs` (`unstage_path_in_index`)：构造 5,000+ 索引条目，批量取消暂存 200 个文件与子目录，验证结果与 `git` 索引行为一致且无 `O(targets × index)` 扫描。
  - `commit_changes.rs`：验证纯新增/删除文件的换行符统计与原 `imara_line_stats` 结果完全相等（含无尾换行边界用例）。
  - `watcher.rs`：构造多层嵌套忽略目录与 Windows 大小写/`\\?\` 路径，验证祖先目录缓存命中及大小写归一化过滤行为正确；验证 `.git/objects/` 变更风暴不反复穿透触发工作区全量状态刷新。
  - `details/blame.rs` & `cli.rs`：验证静默模式 CLI 输出与 `--porcelain` 解析在重复提交与重命名路径下输出与原 `--line-porcelain` 逐行等价。
- **基准与全量检查**：
  - `cargo bench -p jbgit-core --no-run` 编译通过并可在本地运行对比优化前后指标；
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace --locked`

## Comments

- **2026-11-28**：基于 `jbgit-engine` 与 `jbgit-core` 性能审计创建本 P1 工单，治理 Tokio 阻塞、Windows 进程/状态风暴、Watcher 过滤效率与 Criterion 基准覆盖缺口。
- **2026-11-28**：用户将本工单本轮范围收窄为两项（Watcher 治理 + 提交详情快速路径），其余条目延后，本工单保持 `ready-for-agent`。
  - **Watcher（`watcher.rs`）**：
    - `.git/`：保留递归监听（Windows `ReadDirectoryChangesW` 无法排除 `objects/` 子树；`MERGE_HEAD`、`rebase-merge/` 等不存在的路径无法预先注册；非裸仓库的工作区递归监听本就会收到 `.git/**` 事件），改为在 `classify_git_meta_path` 顶部做零分配的首段路径判定：`objects/`、`logs/` 首段即丢弃，`*.lock`、`COMMIT_EDITMSG`、`hooks/`、`info/`、`lfs/`、`modules/`、其他 `worktrees/`、`config`、`FETCH_HEAD`、`ORIG_HEAD` 等不在白名单内者照旧返回 `None`。白名单（`index` → StageStatus；`HEAD`、`refs/**`、`packed-refs`、`shallow`、`MERGE_HEAD`、`REBASE_HEAD`、`CHERRY_PICK_HEAD`、`REVERT_HEAD`、`BISECT_LOG`、`rebase-merge/**`、`rebase-apply/**` → RefsAndGraph）与改动前逐项一致，不会漏掉现有刷新；仅当原始 `strip_prefix` 失败时才走字符串归一化慢路径。
    - 工作区：`WorktreeEventFilter` 改为**祖先目录缓存**——先零分配地自深向浅查已缓存的祖先结论，命中被忽略祖先即按前缀拒绝；有未知祖先时自浅向深查询并缓存，遇到第一个被忽略目录即停止。`dist/a/b/c.js`、`out/x/y/z.o` 之类深层风暴只需 1 次 libgit2 查询。缓存满时先淘汰过期项再整体清空（仍以 `IGNORE_DIR_CACHE_MAX` 为上限）。去掉了批内 `HashSet`/`Vec` 分配（debouncer 已按路径去重），先单独扫描 `.gitignore` 变更再逐条判定、首个相关路径即返回。
    - `statically_relevant_rel_path` 与 `classify_git_meta_path` 统一使用 `rel_path_under`：常规拼写走 `Path::strip_prefix` 借用结果，失败时回退到对 Windows `\\?\` 前缀与盘符/目录大小写不敏感的归一化比较（同时去掉了旧实现中的 `to_ascii_lowercase` 双分配）。
  - **提交详情（`details/commit_changes.rs`）**：`load_commit_details_sync` 改用 `line_stats`，任一侧为空（纯 `Added`/`Deleted`，以及清空/填充文件）时直接按 `\n` 计数（非空且无尾换行的末段计 1 行，与 imara `&str` 行切分一致），跳过 `InternedInput` + `Diff::compute` + `postprocess_lines`；二进制判定不变。`resolve_compare_with_local` 在索引条目数超过 `LARGE_INDEX_ENTRIES` 时跳过 `find_similar` 重命名配对（与状态扫描策略一致，此时显示为删除 + 新增）。
  - **测试**：`watcher.rs` 新增 `.git/objects` 等风暴路径忽略且 `HEAD`/`index`/`refs/heads/x`/`MERGE_HEAD`/`rebase-merge/*` 仍触发、Windows `\\?\` 与盘符大小写归一化（`cfg(windows)`）、祖先缓存（500 个不同深层目录仅 1 次查询）与缓存有界测试；`commit_changes.rs` 新增空文件/有无尾换行/CRLF/非法 UTF-8 下快速路径与完整 diff 结果相等、取消检查，以及真实仓库中新增/删除/空文件/二进制文件统计的端到端测试。`cargo test -p jbgit-engine --locked` 与 `cargo clippy -p jbgit-engine -p jbgit-tui --all-targets -- -D warnings` 通过。
  - **延后**：Tokio 线程解阻、`git --version` 双探测、多选批量验签、`WindowsJobGuard`、`unstage_path_in_index`、`UpdateProject`/CRLF 子进程合并、CLI 静默缓冲、blame `--porcelain`、Criterion 基准补齐等仍未处理，对应任务框保持未勾选。`recurse_untracked_dirs` 默认值（`StatusScanMode::Always`）属于产品决策而非性能修复，本轮不改动。
- **2026-11-28（范围修订）**：按文首「范围修订」节更新 `Status: resolved`；暂缓项汇总至 [`07`](./07-deferred-perf-candidates.md)，不做项不再跟踪。
