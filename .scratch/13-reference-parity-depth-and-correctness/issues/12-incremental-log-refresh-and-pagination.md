# 12: 增量 Log 刷新与突破 50k 提交渐进分页

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M10 — reference 深度对齐与正确性加固
Priority: P2

- **参考源码**：
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/data/{VcsLogRefresherImpl.kt,VcsLogJoiner.java}`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/visible/CommitCountStage.kt`
- **关联文件**：
  - `crates/jbgit-core/src/graph/permanent.rs`、`crates/jbgit-core/src/graph/joiner.rs`、`crates/jbgit-core/src/repo.rs`
  - `crates/jbgit-engine/src/pool/snapshot.rs`（`DEFAULT_COMMIT_PAGE_SIZE = 50_000`）、`crates/jbgit-engine/src/supervisor.rs`
  - `crates/jbgit-tui/src/log_view/workbench.rs`、`crates/jbgit-tui/src/views/log.rs`

---

## 1. 背景与目标

当前 `jbgit` 的提交加载存在两个大仓瓶颈：① 硬编码上限 `DEFAULT_MAX_COMMITS = 50_000`，超过 5 万提交的仓库无法继续查看更早历史；② 每次提交、fetch 或分支变动触发 `RefreshScope::RefsAndGraph` 都会从头执行全量 `git2::Revwalk`。按 2026-11-24 维护者拍板，本工单采用**轻量级无 SQLite 方案**：实现 `VcsLogJoiner` 风格的增量头部拼接与渐进分页加载（不引入 SQLite 持久化索引）。

## 2. 任务清单

- [x] **渐进分页加载（突破 50k 硬上限）**：
  - `RepoSnapshot` 记录 `loaded_limit: usize` 与 `has_more_commits: bool`（区分“仓库真实结束 / 浅克隆边界”与“触达分页上限”）；
  - 当 `has_more_commits == true` 时，在 Log 表格底端渲染可点击/可回车触发的 `[Load More (+50,000 commits)… ]` 虚拟尾行（同时提供 `CommandCatalog` 命令 `LogLoadMore`）；
  - 触发加载更多后，以递增上限（或续接边界 OID）在后台异步加载并无损保留当前滚动位置与选中提交 `Oid`。
- [x] **增量头部刷新（`VcsLogJoiner` 轻量拼接）**：
  - 在引擎快照缓存保留上一轮已加载的 `commits` 表与 `oid -> index` 索引；
  - `RefreshScope::RefsAndGraph` 时，先对比新旧 refs 集合：
    - 若仅发生常规前进（新 commit、fetch 新增提交、新建/移动分支），`git2::Revwalk` 以新 ref tips 为起点、以旧已加载集合中的对应旧 tips 为 `hide` 停止边界，仅扫描新增差集提交并按拓扑序拼接到旧表头部，再按需裁剪不再可达的孤立旧 tip；
    - 若发生大规模历史改写或仓库切换/unshallow，则自动安全回退到全量加载；
  - 保持 `PermanentLinearGraph` 构建结果的确定性与全量构建 100% 等价。

## 3. 测试

- **纯函数 / 引擎单元测试**：增量拼接与全量构建对比测试（覆盖：普通 commit、merge commit、分支快进、force-push/reset 撤回、删除分支、新 tag），断言拼接后的提交集合与父子边与全量扫描完全一致。
- **引擎集成测试**：将测试分页步长设为小值（如 `page_size = 5`）在 12 提交仓库中验证首屏 5 条、`has_more_commits == true`、两次 `LoadMore` 后加载全部 12 条且无重复。
- **性能基准**：复用/扩展 `graph_layout` 基准，对比大仓单次 commit 后增量刷新与全量刷新的耗时提升。
