# 03: 三方冲突视图、Diff 软换行高度缓存、滚动惰性 Fallback 与二分搜索优化

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M13 — 大仓库、渲染热路径与引擎并发性能二期治理
Priority: P0

- **关联文件**：
  - `crates/jbgit-core/src/conflict.rs`（`ConflictBlock::pane_row_count` `:239-255`，`three_way_merge_blocks` `:325-454`，`ConflictDocument::from_three_way` `:554-655`，`three_pane_rows` `:1053-1183`）
  - `crates/jbgit-engine/src/pool/snapshot.rs`（`load_conflict_document_sync` `:175-378`）
  - `crates/jbgit-tui/src/views/conflict_merge.rs`（`:424`）
  - `crates/jbgit-tui/src/app/state.rs`（`AppState::scroll_pane` `:410-464`）
  - `crates/jbgit-tui/src/app/scroll.rs`（`ScrollViewport::scroll_by` `:56-60`）
  - `crates/jbgit-tui/src/diff_session.rs`（`DiffSearchState::recompute` / `ranges_for` `:666-763`，`visual_row_offset_of` / `total_rows` `:877-909`）
  - `crates/jbgit-tui/src/views/log_details/diff_view.rs`（`render_file_diff_body` `:1003-1030`）
  - `crates/jbgit-tui/src/views/commit_diff.rs`（`render_stage_diff_workbench` `:651-665`）
  - `crates/jbgit-tui/src/log_view/workbench.rs`（`LogWorkbench::scroll_pane` `:332-337`）
  - `crates/jbgit-tui/src/stage_view/workbench.rs`（`diff_visual_row_offset_of` / `diff_total_visual_rows` `:335-368`，`Action::StageDiffMoveCursor` `:2127-2142`）
  - `crates/jbgit-tui/src/app/dispatch.rs`（`Action::ModalPaste` `:402-460`）
  - `crates/jbgit-core/src/stage_tracker.rs`（`build_diff_hunks_with_whitespace` `:440-467`，`compute_diff_blocks_with_whitespace` `:477-532`，`StageLineTracker::new_with_whitespace` `:733-755`）
  - `crates/jbgit-core/src/details.rs`（`BlameBundle::content_lines` `:384-390`，`paired_line_in_hunk` `:1302-1354`）
  - `crates/jbgit-tui/src/syntax.rs`（`:847`）、`crates/jbgit-tui/src/views/annotate.rs`（`:301`）、`crates/jbgit-tui/src/views/file_history.rs`（`:775`）

---

## 范围修订（2026-11-28）

> 当前需求以本节为准；下文原始根因与任务清单保留作审计记录，未勾选项以本节结论为准。

- **本轮完成**（Commit `013495f`）：
  - `ConflictPaneRow::choice` 改为 `Option<ConflictChoiceKind>`，消除 `Custom` 块逐行克隆的 `O(L²)`；Result 列按行借用。
  - `ConflictDocument::total_pane_rows()` 与 `three_pane_rows_window()`：渲染只构建视口行，滚动取总行数不再整份构建。
  - `ScrollViewport::scroll_by_with` 惰性 fallback，`max_scroll` 已绑定后滚动零全量扫描。
  - `DiffSearchState::ranges_for` 改为 `partition_point` 二分；模态 diff 搜索框粘贴只重算一次。
  - 4 MiB 冲突文件护栏核查已存在（`637e13a`），补回归测试。
- **移入 [`07`](./07-deferred-perf-candidates.md) 待评估**：软换行行高前缀和缓存；非模态 Log / Stage / Stash diff 搜索粘贴仍逐字符重算；超过 4 MiB 的文本冲突以 "Binary" 标题降级。
- **不做**：`-b` / `-w` 的 `StageLineTracker` 文档拷贝、`paired_line_in_hunk` 区块级缓存、Annotate / FileHistory 行偏移缓存、`three_way_merge_blocks` 中 `Unchanged` 切片三倍克隆。

## 1. 根因定位

### 1.1 三方冲突解决器每帧全文件 3 倍克隆与 `Custom` 选择 `O(L²)` 内存爆炸
- `crates/jbgit-core/src/conflict.rs:1053-1183` 的 `ConflictDocument::three_pane_rows()` 每次调用都会为整份文件分配完整的 `Vec<ConflictPaneRow>`：将每一行普通文本克隆 **3 次**（`ours` / `merged` / `theirs`），并逐行克隆冲突块内容；然而 `views/conflict_merge.rs:424` 随后仅渲染视口内的 `~40` 行便将整份 `Vec` 丢弃。
- 更严重的是 `conflict.rs:1173` 在 `for i in 0..max_len` 中对块内每一行执行 `choice: Some(b.choice.clone())`。一旦用户自定义编辑（`ConflictChoice::Custom(Vec<String>)`）一个 1,000 行的冲突块，单帧就会克隆 **1,000 × 1,000 = 1,000,000 个 `String`**！
- 同时 `app/state.rs:461` 在每次滚轮/按键滚动时为了获取总行数而调用 `d.three_pane_rows().len()`；`pane_row_count`（`:250`）为了读取 `.len()` 而调用 `self.resolved_lines()` 克隆整段已解决文本；`from_three_way` / `three_way_merge_blocks`（`:355-361, 444-450`）在加载阶段对 `Unchanged` 文本先拼接成字节再三倍克隆，且 `snapshot.rs:341-376` 缺少 `MAX_DIFF_FILE_BYTES`（4 MiB）体积上限保护。

### 1.2 软换行 Diff 每帧与每次滚动重复全量测宽，且滚动路径提前算被丢弃的 `fallback`
- 当开启 `soft_wrap` 时，`diff_view.rs:1003-1017` 与 `commit_diff.rs:651-665` **每一帧**都分配 `heights: Vec<usize>` 并遍历全部（最高 `20,000`）`DiffRow`，逐字符运行 `UnicodeWidthChar` + Tab-Stop 折行模拟（Split 模式两边各算一遍），即使内容、列宽与 `tab_size` 均未变化。
- 更隐蔽的是 `LogWorkbench::scroll_pane`（`log_view/workbench.rs:335`）、`AppState::scroll_pane`（`app/state.rs:412, 433, 421, 461`）在调用 `ScrollViewport::scroll_by(delta, fallback)`（`app/scroll.rs:56-60`）**之前**先急切算出 `fallback`（遍历全量 Diff 折行、`t.lines().count()` 或 `d.three_pane_rows().len()`）——而只要上一帧渲染过一次，`self.max_scroll` 已是 `Some(...)`，`scroll_by` 会直接忽略 `fallback`！
- 在 Commit 页 Stage Diff（`stage_view/workbench.rs:2138-2139`），每次 `j`/`k` 移动光标会依次调用 `diff_visual_row_offset_of`（`O(cursor)`）、`diff_total_visual_rows`（`O(N)`）以及渲染阶段的第三遍 `O(N)` 测宽。

### 1.3 Diff 内搜索高亮 `O(H × M)` 线性扫描、粘贴逐字符重搜与忽略空白/Blame/FileHistory 文本重复拷贝
- `DiffSearchState::ranges_for`（`diff_session.rs:748-763`）在 `self.matches` 已按 `(row_idx, side)` 严格有序的前提下，依然对每行可见行（Split 模式每行 2 次）做全数组线性扫描。搜单字符（如 `"e"`）命中数万次时，单帧比较数百万次；而 `Action::ModalPaste`（`dispatch.rs:402-460`）对粘贴文本逐字符派发 `DiffSearchChar`，导致长度 `L` 的粘贴触发 `L` 次全量重搜。
- `StageLineTracker::new_with_whitespace`（`stage_tracker.rs:440-532, 733-755`）在开启 `-b`/`-w` 时将 3 份文档反复复制并切片成 `SlicedDocument`（共 7 份缓冲区拷贝），再为每行分配 `Vec<Vec<u8>>`。
- `syntax.rs:847` 在超出 `EAGER_WORD_DIFF_PAIR_BUDGET = 256` 之后，对每一可见行在缓存未命中前调用 `paired_line_in_hunk`（`details.rs:1302-1354`），单行粒度重算整块 bigram 与 `O(D × A)` Dice 矩阵。
- `annotate.rs:301` 每帧调用 `bundle.content_lines()` 分配整文件 `Vec<&str>`；`file_history.rs:775` 每帧 `text.lines().collect()` 切分整文件。

---

## 2. 任务清单

- [ ] **重构 `ConflictDocument` 为轻量总行数计算与视口窗口按需投影，修复 `O(L²)`**：
  - 在 `crates/jbgit-core/src/conflict.rs` 中重构 `ConflictPaneRow`（例如以小型枚举或 `ConflictChoiceSummary` 标记替代把整份 `ConflictChoice::Custom(Vec<String>)` 克隆进每一行，彻底消除 `O(L²)` 克隆）；
  - 重写 `ConflictBlock::pane_row_count` 直接计算最大行数而不调用 `resolved_lines()` 深拷贝；提供 `O(segments)` 的 `ConflictDocument::total_pane_rows()` 与仅生成视口区间 `[start, start + len)` 的 `three_pane_rows_window(start, len)`（或基于版本代际缓存的视图切片），供 `views/conflict_merge.rs:424` 与 `app/state.rs:461` 使用；
  - 在 `three_way_merge_blocks` 中避免对 `Unchanged` 切片三倍克隆且不在不需要保留的三份中重复存储，并在 `snapshot.rs:341-376` 加载冲突文档前应用 `MAX_DIFF_FILE_BYTES`（4 MiB）体积护栏与安全降级提示。
- [ ] **缓存 Diff 软换行行高前缀和并将滚动 `fallback` 改为惰性求值**：
  - 在 `DiffSession`（`diff_session.rs`）与 `CommitWorkbench`（`stage_view/workbench.rs`）中增加软换行行高前缀和缓存（键为 `(Arc/diff ptr, side_by_side, unified_w, split_left_w, split_right_w, tab_size)`，值存储 `prefix_heights: Vec<usize>`），使 `total_rows`、`visual_row_offset_of` 与渲染循环跳过视口前区间均达到 `O(1)` / `O(log N)`；
  - 修改 `ScrollViewport::scroll_by`（`app/scroll.rs:56-60`）增加或改为接收 `FnOnce() -> usize` 惰性 `fallback` 的方法（`scroll_by_with_fallback`），同步改造 `LogWorkbench::scroll_pane` 与 `AppState::scroll_pane` 中的 `LogDiff`、`FileHistoryDiff`、`FileHistoryBlob`、`AnnotateDiff`、`StashDiff`、`ConflictMerge` 分支，确保 `max_scroll` 已绑定时零全文件扫描。
- [x] **将 `DiffSearchState::ranges_for` 改为二分查找并合并粘贴搜索**：
  - `crates/jbgit-tui/src/diff_session.rs:748-763`：利用 `self.matches` 按 `(row_idx, side)` 有序的不变量，改用 `partition_point` 定位起止切片，复杂度由 `O(M)` 降至 `O(log M + k)`；
  - `crates/jbgit-tui/src/app/dispatch.rs:402-460`：处理 `Action::ModalPaste` 针对 Diff 搜索框一次性追加字符串后仅触发单次 `recompute()`。
- [ ] **消除 `-b`/`-w`、大区块行配对与 Blame/FileHistory 视图的重复全量处理**：
  - `crates/jbgit-core/src/stage_tracker.rs`：`StageLineTracker::new_with_whitespace` 与 `build_diff_hunks_with_whitespace` 复用已有的 `SlicedDocument` 与字节切片行区间视图，消除 7× 文档拷贝与每行 `Vec<Vec<u8>>` 分配；
  - `crates/jbgit-core/src/details.rs` & `crates/jbgit-tui/src/syntax.rs:847`：在区块级别按 `(hunk_idx, block_start)` 缓存/批量生成行配对映射，避免每行重复跑 bigram Dice 匹配；
  - `crates/jbgit-core/src/details.rs` & `crates/jbgit-tui/src/views/annotate.rs:301`、`file_history.rs:775`：预计算或缓存行起止偏移表（`Vec<Range<usize>>`），渲染只切分并展示当前视口区间行。

---

## 3. 验收标准与回归测试

- **单元测试（修复前可复现，修复后必过）**：
  - `conflict.rs`：构造含 5,000 行普通文本及 1,000 行 `ConflictChoice::Custom` 块的 `ConflictDocument`，验证 `three_pane_rows_window(100, 40)` 输出与全量 `three_pane_rows()[100..140]` 内容逐字段等价、`total_pane_rows()` 与全量 `.len()` 严格一致，且耗时/分配呈 `O(viewport)` 常数级；校验 `> MAX_DIFF_FILE_BYTES` 冲突文件安全降级。
  - `diff_session.rs` / `scroll.rs`：验证软换行前缀和缓存命中时 `total_rows` 与 `visual_row_offset_of` 与逐行累加结果一致，宽度/diff 变化时精确失效；验证 `scroll_by_with_fallback` 在 `max_scroll` 已设置时不调用闭包。
  - `diff_session.rs` (`DiffSearchState`)：验证 `partition_point` 版 `ranges_for` 与全扫描在单/双边模式下返回完全一致的字节范围，覆盖命中数为 0、首尾及上万命中场景。
  - `stage_tracker.rs`：验证优化后的 `new_with_whitespace`（`-b` / `-w`）生成与旧实现完全一致的 `Hunk` 与局部暂存字节结果。
- **全量检查**：
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace --locked`

## Comments

- **2026-11-28**：基于 `jbgit-core` 与 `jbgit-tui` 性能审计创建本 P0 工单，重点解决三方冲突与大 Diff 软换行/搜索下高概率掉帧和 `O(L²)` 分配缺陷。
- **2026-11-28**：经用户收窄，本轮只做 6 项低成本、确有收益的改动，其余延后（收益低）：
  1. **`Custom` 的 `O(L²)` 克隆**：新增无载荷的 `ConflictChoiceKind`（`ConflictChoice::kind()`，从 `jbgit_core` 导出）。`ConflictPaneRow::choice` 改为 `Option<ConflictChoiceKind>`，每行不再克隆整份 `Custom(Vec<String>)`。merged 列经 `ConflictBlock::resolved_line(i)` 按行借用，不再为每个块构建 `resolved_lines()`。渲染器 `views/conflict_merge.rs` 改为匹配 kind。
  2. **廉价总行数与视口投影**：`pane_row_count` 改走 `resolved_len()`，不再深拷贝。新增 `ConflictDocument::total_pane_rows()`（`O(segments)`）与 `three_pane_rows_window(start, len)`，后者只为视口区间构建行，跳过区间外的段；`three_pane_rows()` 改为 `window(0, usize::MAX)`。渲染器用 `total_pane_rows()` 求 `max_scroll`、用 window 只取 `body_h` 行。`AppState::scroll_pane(ConflictMerge)` 改用 `total_pane_rows()`。
  3. **4 MiB 护栏**：核查后确认已存在。`load_conflict_document_sync` 读取各 stage blob 与工作区文件时走 `blob_reader`，结果为 `CheckedBlob::TooLarge` 即降级为 `LoadedConflict::Special(Binary)`（整文件选 Ours/Theirs 对话框，显示各侧大小），由 Spec 13 的 637e13a 引入，审计行号已过时。本轮未改行为，只补回归测试。
  4. **惰性滚动 fallback**：新增 `ScrollViewport::scroll_by_with(delta, impl FnOnce() -> usize)`，`scroll_by` 委托给它。以下调用点改为惰性，`max_scroll` 已绑定后零全量扫描：`LogWorkbench::scroll_pane(LogDiff)`，`AppState::scroll_pane` 的 `FileHistoryDiff`、`FileHistoryBlob`、`AnnotateDiff`、`ConflictMerge`、`TreeBrowserPreview`、`ApplyPatchPreview`，以及 `StashWorkbench::scroll_diff`、`TreeBrowser::scroll_preview`。fallback 需借用所属结构体时，利用 `ScrollViewport: Copy` 先拷出再写回。
  5. **`ranges_for` 二分查找**：利用 `matches` 按 `(row_idx, side)` 有序，用两次 `partition_point` 定位切片，复杂度为 `O(log M + k)`。
  6. **粘贴只重算一次**：`Action::ModalPaste` 的 Annotate、FileHistory、Reflog、Push 四个 diff 搜索框，经 `split_diff_search_paste` 过滤后，前缀直接 `DiffSearchState::push_str` 追加，只把最后一个字符作为 `DiffSearchChar` 派发，因此每次粘贴只重算一次搜索。非模态的 Log、Stage、Stash 粘贴路径（`input/mod.rs`）仍逐字符派发，未在本轮范围内。
  - **测试**：
    - `conflict.rs`：5,000 行普通文本 + 1,000 行 `Custom` 块 + `BothTheirsFirst` 块，验证 `total_pane_rows() == three_pane_rows().len()`，多个窗口（含跨块和尾部越界）与全量切片逐字段相等，块行的 kind 与 merged 列等价于填充后的 `resolved_lines()`。
    - `git_ops_integration.rs`：大于 4 MiB 的文本冲突降级为 `SpecialConflictLoaded(Binary)`，且不构建 `ConflictDocument`。
    - `scroll.rs`：`max_scroll` 已绑定时不调用闭包。
    - `diff_session.rs`：`ranges_for` 与线性扫描参考实现一致，覆盖 0 命中、首尾行、上千命中和左右分侧。
    - `diff_enhancements_tests.rs`：FileHistory 粘贴（含换行）与逐字符键入得到相同的 query、matches 和 current。
  - **延后**：软换行行高前缀和缓存、`-b`/`-w` 的 `StageLineTracker` 拷贝、`paired_line_in_hunk` 缓存、annotate/file_history 行缓存，以及 `three_way_merge_blocks` 中 `Unchanged` 切片的三倍克隆。因此任务 1、2、4 只部分完成或未做，未勾选；任务 3 已完成并勾选。
- **2026-11-28（范围修订）**：按文首「范围修订」节更新 `Status: resolved`；暂缓项汇总至 [`07`](./07-deferred-perf-candidates.md)，不做项不再跟踪。
