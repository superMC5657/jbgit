# 02: 按选区查看历史（`git log -L`）

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M9 — reference 缺口补齐
Priority: P1
Blocked by: 01

- **参考源码**：
  - `reference/platform/vcs-impl/src/com/intellij/openapi/vcs/history/impl/VcsSelectionHistoryDialog.java`
  - `reference/plugins/git4idea/backend/src/history/GitFileHistory.kt`
- **关联文件**：
  - `crates/jbgit-core/src/details.rs`（`FileHistoryKey`）
  - `crates/jbgit-engine/src/details/file_history.rs`
  - `crates/jbgit-tui/src/log_view/file_history.rs`、`crates/jbgit-tui/src/views/file_history.rs`、`crates/jbgit-tui/src/views/annotate.rs`（Issue 01 新增）

---

## 1. 背景与目标

IDEA 的 "Show History for Selection" 让用户只追踪某个函数 / 代码段的演变。`jbgit` 已有基于 `git log --follow` 的单文件历史视图（Spec 09），只需扩展历史键与解析器即可复用该视图。

## 2. 任务清单

- [x] `FileHistoryKey` 增加 `line_range: Option<(u32, u32)>`（1-based、闭区间）；`line_range.is_some()` 时忽略 `follow_renames`（`-L` 本身会跨重命名追踪）。
- [x] 引擎：`git log -L <start>,<end>:<path> --format=<既有格式> [<start_rev>]` 的解析。`-L` 会输出每个提交的区间补丁：解析器提取提交元数据与区间内 `+`/`-` 行数，并把补丁文本作为 `FileHistoryEntry` 的可选 `range_patch: Option<Arc<str>>` 缓存，右侧 Diff 直接渲染它而无需再请求整文件 Diff。
- [x] 截断上限沿用 500 条并设置 `truncated`；超大区间（> 2000 行）给出确认提示。
- [x] 入口：Annotate 视图中按 `V` 进入行可视选择后执行 `Show History for Selection`；Stage Diff 视图的可视选择（`StageDiffToggleVisual`）也提供同一菜单项（作用于工作区版本的行号）。
- [x] File History 视图标题显示 `path:start-end`，右侧 Diff 标注"区间补丁"。

## 3. 测试

- 临时仓库：函数体在 3 个提交中被修改，另 2 个提交只改文件其他部分 → 仅返回 3 个提交。
- 区间跨重命名：`git mv` 后仍能追溯。
- 解析器单元测试覆盖 merge 提交与空结果。
