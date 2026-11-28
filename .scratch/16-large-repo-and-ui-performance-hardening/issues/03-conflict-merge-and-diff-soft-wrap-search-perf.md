# 03: 三方冲突视口投影与 `Custom` `O(L²)` 修复、惰性滚动与 Diff 二分搜索

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M13 — 大仓库、渲染热路径与引擎并发性能二期治理
Priority: P0

- **关联文件**：
  - `crates/jbgit-core/src/conflict.rs`（`ConflictChoiceKind`、`ConflictPaneRow::choice`、`ConflictBlock::pane_row_count` / `resolved_line`、`ConflictDocument::total_pane_rows` / `three_pane_rows_window`）
  - `crates/jbgit-tui/src/views/conflict_merge.rs`
  - `crates/jbgit-tui/src/app/state.rs`、`crates/jbgit-tui/src/app/scroll.rs`、`crates/jbgit-tui/src/log_view/workbench.rs`、`crates/jbgit-tui/src/stash_view.rs`、`crates/jbgit-tui/src/tree_browser.rs`
  - `crates/jbgit-tui/src/diff_session.rs`（`DiffSearchState::ranges_for`）
  - `crates/jbgit-tui/src/app/dispatch.rs`（`Action::ModalPaste`）
  - `crates/jbgit-engine/tests/git_ops_integration.rs`

---

## 1. 根因定位

1. **三方冲突解决器每帧全文件 3 倍克隆与 `Custom` 选择 `O(L²)` 内存爆炸**：`ConflictDocument::three_pane_rows()` 每次调用为整份文件克隆所有行与 `ConflictChoice::Custom(Vec<String>)`（块内 `L` 行每行克隆一次导致 `O(L²)` 分配），`views/conflict_merge.rs` 与 `app/state.rs` 取总行数时也构建整份 `Vec`。
2. **滚动路径急切计算被丢弃的 `fallback`**：`ScrollViewport::scroll_by(delta, fallback)` 在 `max_scroll` 已绑定后仍先急切扫描全量行数或 diff hunks。
3. **Diff 搜索高亮 `O(H × M)` 线性扫描与模态粘贴逐字符重搜**：`DiffSearchState::ranges_for` 对有序 `matches` 线性扫描，`Action::ModalPaste` 对模态 diff 搜索框逐字符重搜。

---

## 2. 任务清单

- [x] **重构 `ConflictDocument` 为轻量总行数计算与视口窗口按需投影，修复 `O(L²)`**：
  - 新增无载荷 `ConflictChoiceKind` 替代每行克隆 `ConflictChoice::Custom(Vec<String>)`，merged 列通过 `ConflictBlock::resolved_line(i)` 按行借用；
  - `pane_row_count` 改走 `resolved_len()`，新增 `total_pane_rows()`（`O(segments)`）与 `three_pane_rows_window(start, len)` 供渲染与滚动使用；
  - 核查 `snapshot.rs` 中 `4 MiB` 冲突文件降级护栏（`CheckedBlob::TooLarge` → `LoadedConflict::Special(Binary)`）已存在，补齐回归测试。
- [x] **将滚动 `fallback` 改为惰性求值**：
  - 新增 `ScrollViewport::scroll_by_with(delta, impl FnOnce() -> usize)`，改造 `LogDiff`、`FileHistoryDiff`、`FileHistoryBlob`、`AnnotateDiff`、`PushDiff`、`ConflictMerge`、`TreeBrowserPreview`、`ReflogDiff`、`ApplyPatchPreview`、`StashWorkbench::scroll_diff`、`TreeBrowser::scroll_preview`。
- [x] **将 `DiffSearchState::ranges_for` 改为二分查找并合并模态粘贴搜索**：
  - `ranges_for` 改用两次 `partition_point` 定位切片（`O(log M + k)`）；
  - `Action::ModalPaste` 针对 Annotate / FileHistory / Reflog / Push 搜索框通过 `split_diff_search_paste` 前缀 `push_str` + 末字符单次重算。

> 注：软换行行高前缀和缓存、非模态 diff 搜索粘贴与超 4 MiB `TooLarge` 提示等特定场景项已暂缓至 [`07`](./07-deferred-perf-candidates.md)。

---

## 3. 验收标准与回归测试

- `conflict.rs`：5,000 行普通文本 + 1,000 行 `Custom` 块 + `BothTheirsFirst` 块，验证 `total_pane_rows() == three_pane_rows().len()`，跨块及越界窗口与全量切片逐字段等价。
- `git_ops_integration.rs`：大于 4 MiB 的文本冲突降级为 `SpecialConflictLoaded(Binary)` 且不构建 `ConflictDocument`。
- `scroll.rs`：`max_scroll` 已绑定时不调用闭包。
- `diff_session.rs`：`ranges_for` 与线性扫描在 0 命中、首尾行、上千命中与双侧模式下结果完全一致。
- `diff_enhancements_tests.rs`：FileHistory 粘贴（含换行）与逐字符键入得到相同状态。

## Comments

- **2026-11-28（Commit `013495f`）**：完成冲突视口投影与 `ConflictChoiceKind`、惰性 `scroll_by_with`、`ranges_for` 二分、模态搜索粘贴单次重算与 4 MiB 回归测试。
- **2026-11-28（Code Review 修复）**：`app/state.rs` 中 `ScrollPane::PushDiff` 与 `ScrollPane::ReflogDiff` 补充改用 `scroll_by_with`，消除 `max_scroll` 已绑定后的 eager hunk 求和。
