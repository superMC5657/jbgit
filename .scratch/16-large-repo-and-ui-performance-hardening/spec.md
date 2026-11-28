# Spec 16: 大仓库、渲染热路径与引擎并发性能二期治理（Phase 11）

- **目录**：`.scratch/16-large-repo-and-ui-performance-hardening/`
- **状态**：`01`、`02`、`03`、`05`、`06` resolved；`07` needs-triage（暂缓候选项）；原 `04` 已删除。分支 `spec16-perf`，Commits `1f8c349`..`5ca7228` + Review 回归修复。
- **下属 Issues**（5 个已实施工单 + 1 个暂缓候选项汇总）：
  - [`01-engine-index-reload-snapshot-arc-and-copy-scan.md`](./issues/01-engine-index-reload-snapshot-arc-and-copy-scan.md)（**P0** · 消除 `with_thread_repo` 强刷 `.git/index`、`SnapshotCommitCache` 三份深拷贝与点击 Untracked 文件全仓拷贝扫描）
  - [`02-core-dag-edges-filter-and-visible-graph-perf.md`](./issues/02-core-dag-edges-filter-and-visible-graph-perf.md)（**P0** · `recommended_track_width` 单趟线性差分扫描）
  - [`03-conflict-merge-and-diff-soft-wrap-search-perf.md`](./issues/03-conflict-merge-and-diff-soft-wrap-search-perf.md)（**P0** · 消除冲突 `Custom` `O(L²)` 克隆与视口窗口投影、惰性滚动 fallback、`ranges_for` 二分与模态粘贴单次重算）
  - [`05-vector-graph-scroll-blit-and-resize-coalescing.md`](./issues/05-vector-graph-scroll-blit-and-resize-coalescing.md)（**P1** · `GraphRenderer::set_capabilities` 能力未变不作废与连续 `Resize` 合并）
  - [`06-engine-tokio-unblock-windows-scaling-and-benchmarks.md`](./issues/06-engine-tokio-unblock-windows-scaling-and-benchmarks.md)（**P1** · Watcher 早期路径过滤 + 祖先缓存 + Windows 归一化、Added/Deleted 直接计行与 `CompareWithLocal` 护栏）
  - [`07-deferred-perf-candidates.md`](./issues/07-deferred-perf-candidates.md)（**P2** · 暂缓的特定场景候选项与 `recurse_untracked_dirs` 产品决策，待评估）
- **关联源码**：
  - `crates/jbgit-core/src/`：`repo.rs`、`conflict.rs`、`filter.rs`、`lib.rs`、`graph/{edges_in_row.rs, print_elements.rs, joiner.rs, mod.rs}`
  - `crates/jbgit-core/benches/graph_layout.rs`
  - `crates/jbgit-engine/src/`：`supervisor.rs`、`stage.rs`、`watcher.rs`、`pool/{mod.rs, worker_pool.rs, snapshot.rs, stage_status.rs, scan_config.rs}`、`details/commit_changes.rs`
  - `crates/jbgit-tui/src/`：`session.rs`、`main.rs`、`diff_session.rs`、`stash_view.rs`、`tree_browser.rs`、`app/{state.rs, scroll.rs, dispatch.rs}`、`log_view/workbench.rs`、`graph/{renderer.rs, vector/mod.rs}`、`views/conflict_merge.rs`

---

## 1. 背景与实测基线

2026-11-28 完成 `01` 后实跑 10 万提交基准（`graph_layout`）得到：

| 基准 | 耗时 |
| :--- | ---: |
| `recommended_track_width_100k`（每次构图 / 筛选 / 折叠） | ≈ 190 ms → **0.35 ms**（`02` 后） |
| 构图（`permanent_graph_and_dfs_layout_100k` + `visible_graph_build_*`） | ≈ 21 ms + 15 ms |
| `print_elements_viewport_60_rows_100k`（每帧） | ≈ 0.36 ms |
| `incremental_head_join_100k_2000_new_commits` | 8.2 ms → **7.3 ms**（`01` 后） |

审计中多数每帧渲染估计耗时在实测下不成立，因此收窄为只实施真实卡顿或算法缺陷且改动可控的子集，原 `04` 已删除，特定场景候选项保留在 [`07`](./issues/07-deferred-perf-candidates.md)。

---

## 2. 本轮实施（分支 `spec16-perf`）

| Issue | 状态 | 实施内容 | Commit |
| :---: | :---: | :--- | :---: |
| `01` | resolved | `index.read(false)` 替代强刷（`EngineError::Cancelled` 不触发强刷）；`stage.rs` libgit2 写路径显式 `index.read(true)?`（CS-4.1）；`RepoSnapshot::commits` 改 `Arc` 共享；`insert_indexed` 去 `O(K²)`；未跟踪文件跳过全仓 copy 扫描，`added == 0` 时跳过 `include_unmodified` | `0d94bee` + review fix |
| `02` | resolved | `recommended_track_width` 单趟差分扫描：189.6 ms → 0.35 ms | `a4b0cc6` |
| `03` | resolved | 冲突 `Custom` `O(L²)` 克隆、视口窗口投影与廉价总行数；惰性滚动 fallback（含 `PushDiff`/`ReflogDiff`）；`ranges_for` 二分；模态粘贴单次重算；4 MiB 护栏回归测试 | `013495f` + review fix |
| `05` | resolved | `set_capabilities` 能力未变不作废；连续 `Resize` 合并；`invalidate_emission` 防止往返 `Resize` 漏发 Sixel/iTerm2 帧 | `042aa3b` + review fix |
| `06` | resolved | Watcher 早期路径过滤 + 祖先目录忽略缓存 + `\\?\` / 盘符归一化（优先匹配 linked worktree `git_dir`，CS-3.5）；Added/Deleted 文件直接计行；`CompareWithLocal` 大索引护栏 | `9e76d21` + review fix |
| `07` | needs-triage | 暂缓候选项汇总 | — |

公共 API 变化：`RepoSnapshot::commits: Arc<Vec<CommitSummary>>`；`ConflictPaneRow::choice: Option<ConflictChoiceKind>`；`GraphRenderer::invalidate_emission()` / `PixelVectorGraphBackend::invalidate_emission()`。

---

## 3. 保留的架构约束（Out of Scope）

- **不引入 SQLite 持久化索引**：继续基于内存 CSR 图 + 轻量增量 Join + 分页模型优化。
- **不替换 `syntect` 语法高亮引擎**：保留现有异步可取消 `syntect` 管线。
- **不破坏混合 Git 引擎读写边界**：保持 `git2` 只读/内存局部暂存（写前按 CS-4.1 强制 `index.read(true)?`）+ 原生 `git` CLI 写操作。

---

## 4. 验收与门禁

- 每个 Issue 均配套针对性单元测试或 Criterion 基准，全量门禁统一运行：
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo clippy -p jbgit-tui --features graphics-protocol --all-targets -- -D warnings`
  - `cargo test --workspace --locked`
  - `cargo test -p jbgit-tui --features graphics-protocol --locked vector::tests`
- 仓库未做 rustfmt 全量清理，不执行 `cargo fmt --all -- --check`；只对改动行按 rustfmt 风格手工格式化。
