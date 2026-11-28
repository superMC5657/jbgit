# 06: Watcher 路径过滤与缓存治理、提交详情快速路径

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M13 — 大仓库、渲染热路径与引擎并发性能二期治理
Priority: P1

- **关联文件**：
  - `crates/jbgit-engine/src/watcher.rs`（`classify_git_meta_path`、`rel_path_under`、`WorktreeEventFilter`）
  - `crates/jbgit-engine/src/details/commit_changes.rs`（`line_stats`、`resolve_compare_with_local`、`load_commit_details_sync`）

---

## 1. 根因定位

1. **Watcher `.git/` 与工作区构建目录风暴过滤低效**：`WorktreeEventFilter` 仅缓存直接父目录而非祖先目录，深层构建目录（`target/`、`node_modules/`）触发大量 libgit2 查询；`statically_relevant_rel_path` 在 Windows `\\?\` / 盘符大小写不一致时容易失效。
2. **提交详情纯新增/删除文件与超大索引本地对比耗时过高**：`load_commit_details_sync` 对纯 `Added`/`Deleted` 文件也运行完整 `InternedInput` + `Diff::compute` + `postprocess_lines`；`CompareWithLocal` 缺少 `LARGE_INDEX_ENTRIES` 重命名探测保护。

---

## 2. 任务清单

- [x] **治理 `RepoWatcher` 元数据过滤、祖先目录缓存与 Windows 路径归一化**：
  - `.git/` 元数据路径在 `classify_git_meta_path` 顶部做零分配首段路径判定，丢弃 `objects/`、`logs/` 等无关路径并保持白名单触发集合不变；在 linked worktree 下按 CS-3.5 优先匹配 `git_dir`（含归一化回退）再匹配 `common_dir`；
  - `WorktreeEventFilter` 改为有界祖先目录缓存（自深向浅命中即拒、未知时自浅向深查询并缓存）并先处理 `.gitignore` 变更；
  - `rel_path_under` 常规拼写走零分配 `strip_prefix`，失败时回退到 `\\?\` 与盘符/目录大小写归一化比较。
- [x] **优化 `load_commit_details_sync` 纯新增/删除计行与 `CompareWithLocal` 护栏**：
  - 任一侧为空时走 `line_stats` 直接按 `\n` 计数，跳过完整 Histogram diff；
  - `resolve_compare_with_local` 在索引条目数超过 `LARGE_INDEX_ENTRIES` 时跳过 `find_similar` 重命名配对。

> 注：`unstage_path_in_index`、`UpdateProject` 串行 spawn 与 `recurse_untracked_dirs` 产品决策已归入 [`07`](./07-deferred-perf-candidates.md)。

---

## 3. 验收标准与回归测试

- `watcher.rs`：验证 `.git/objects/` 等风暴路径被忽略、`HEAD`/`index`/`refs/heads/x`/`MERGE_HEAD`/`rebase-merge/*` 仍触发刷新；Windows `\\?\`、盘符大小写及 linked worktree `git_dir` 优先归一化回归测试；祖先目录缓存（500 个不同深层目录仅 1 次查询）与有界测试。
- `commit_changes.rs`：验证空文件/有无尾换行/CRLF/非法 UTF-8 下快速路径与完整 diff 结果相等、取消检查及真实仓库端到端统计测试。

## Comments

- **2026-11-28（Commit `9e76d21`）**：完成 Watcher 路径过滤/祖先缓存/Windows 归一化，以及 `commit_changes.rs` `line_stats` 快速路径与 `CompareWithLocal` 大索引护栏。
- **2026-11-28（Code Review 修复）**：`watcher.rs` 中 `classify_git_meta_path` 在 `git_dir != common_dir` 时遇到 `common_dir/worktrees/...` 不再早退于未归一化 `common_dir`，改为先尝试 `strip_prefix_normalized_fallback(path, git_dir)`（CS-3.5），并补充 `cfg(windows)` linked worktree 混合大小写/`\\?\` 回归测试。
