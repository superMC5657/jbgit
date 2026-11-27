# 04: Details / Footer / Filter 弹窗缓存、UI 线程零同步 I/O、异步折叠构图与内存有界化

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M13 — 大仓库、渲染热路径与引擎并发性能二期治理
Priority: P1

- **关联文件**：
  - `crates/jbgit-tui/src/views/log_details/commit_details.rs`（`:125-130, 210-223, 485-549`）
  - `crates/jbgit-tui/src/views/footer.rs`（`plan_footer_bar` `:88-126`，`display_width` `:317-336`，`collect_footer_hints` `:519-777`，`layout_footer_pills` `:779-847`）
  - `crates/jbgit-tui/src/log_view/filter_bar.rs`（`LogSpeedSearchState::recompute` `:233-275`，`filter_suggestions_with_context` `:413-501`）
  - `crates/jbgit-tui/src/views/overlays/popups.rs`（`render_filter_editor_popup` `:459-465`）
  - `crates/jbgit-tui/src/graph/state.rs`（`has_multi_selection` `:1078-1092`，`rebuild_collapsed_now` / `collapse_all` / `expand_all` / `expand_bek_merge` `:1304-1409`）
  - `crates/jbgit-tui/src/views/log.rs`（`render_commits_table` `:1155, 1305, 1360-1377`）
  - `crates/jbgit-tui/src/log_view/workbench.rs`（`ensure_graph_cache` / `reconcile_requests` `:272-293, 1638-1661`）
  - `crates/jbgit-tui/src/views/console.rs`（`render_command_output` `:255-336`）
  - `crates/jbgit-tui/src/console_workbench.rs`（`entries` `:10`，`push_started` / `push_output` `:52-62`）
  - `crates/jbgit-tui/src/stage_view/workbench.rs`（`RollbackUndoEntry` / `rollback_undo_stack` `:61-66, 153, 657-670, 2540-2544`，`load_draft` / `spawn_draft_write` `:905-962`，`Action::StageDiffUndoRollback` `:2335-2354`）
  - `crates/jbgit-tui/src/log_view/branches_pane.rs`（`load_settings` / `save_settings` `:50-62`）
  - `crates/jbgit-tui/src/app/events.rs`（`apply_engine_event` `:243-246, 847-849`）、`crates/jbgit-tui/src/app/state.rs`（`has_active_tasks` `:702-723`）
  - `crates/jbgit-tui/src/diff_session.rs`（`change_rows` / `expand_state` `:317-398`）、`crates/jbgit-tui/src/syntax.rs`（`highlight_single_line` `:319-365`）
  - `crates/jbgit-core/src/details.rs`（`ChangeDir::count` `:578-580`，`build_change_rows_filtered` `:596-673`）
  - `crates/jbgit-core/src/stage.rs`（`StageDir::count` `:232-234`，`build_stage_tree_rows` `:240-490`）

---

## 1. 根因定位

1. **Commit Details 每帧 `O(Tokens × Commits)` 线性扫**：`commit_details.rs:485-549` 每帧对全消息折行并用 `snap.resolve_revision()` 线性遍历全量 `snap.refs`（最多 3 次）与 `snap.commits`（全部 5 万条）来识别 7..=40 hex 链接；加载期间（`:129`）亦线性扫描 `s.commits.iter().find(...)` 而非走图谱索引；渲染循环逐行 `lines[idx].clone()` + 全量 `hits` 扫描。
2. **Footer 栏每帧无缓存与 `O(P²)` `format!` 堆分配**：`plan_footer_bar` 每帧重新汇总作用域与冲突判定；`FooterPill::display_width()`（`footer.rs:317-327`）在 `layout_footer_pills`（`:799-809`）的 `O(P²)` 溢出枚举循环中反复调用 `format!` 分配两个堆 `String`。
3. **Filter 弹窗渲染同步磁盘遍历与未防抖 SpeedSearch**：`filter_suggestions_with_context`（`filter_bar.rs:428-500`）在弹窗每帧重绘时被调用——`Paths` 同步执行 `std::fs::read_dir(&s.workdir)`，`User` 每帧遍历全量 `s.commits` 统计排序作者，`Branch` 用 `Vec::contains` 做 `O(R²)` 去重；`LogSpeedSearchState::recompute` 每击键同步扫全表并分配 `to_lowercase()`。
4. **大图折叠与表格渲染热路径缺陷**：`graph/state.rs:1304-1409` 在大仓库（`>= 2,000` commits）下折叠/展开/LinearBek 仍在 UI 线程同步构建 `VisibleGraph`；`has_multi_selection()`（`:1078-1092`）将 `O(K)` 的 `selection_count()` 放在 `O(1)` 的 `!selected_rows.is_empty()` 之前，在 `views/log.rs:1155, 1305` 每帧被调用 `2 × H` 次；`align_labels_right` 对每行算两遍 `layout_pills`；`ensure_graph_cache` / `reconcile_requests` 未先判 `is_stale_for` 即分配 `favorite_branch_names`。
5. **UI 线程同步 I/O、无谓刷新与内存无界增长**：
   - `BranchesPaneState::save_settings` 与 `StageDiffUndoRollback` 在 UI 线程执行同步 `fs::write`/`fs::read`；`spawn_draft_write` 每次防抖新建原始 OS 线程；
   - `apply_engine_event` 在拒绝过期事件前即设 `needs_redraw = true`，且非 Console 标签页收到 `ConsoleOutput` 仍重绘全应用；`has_active_tasks()` 在等待用户输入凭据时维持 10 Hz `spinner_sleep` 空转重绘；
   - `render_command_output`（`views/console.rs:255-336`）每帧构造全量 `O(N)` `Vec<Line>` 后丢弃视口外部分；`ConsoleWorkbench::entries` 与 `rollback_undo_stack` 无历史/内存上限；
   - `ChangesTreeSession::change_rows` 以 `FxHashSet<String>` 相等性检验缓存；`highlight_single_line` 每次命中均克隆 `(syntax.name, line)`；`ChangeDir::count` / `StageDir::count` 递归重复计数呈 `O(F × depth)`，且逐级目录新建都先 `to_string()`。

---

## 2. 任务清单

- [ ] **Memoize Commit Details 与 `O(1)` Hash 解析**：
  - 在 `DetailsPaneState` 或专用缓存中按 `(details_ptr, snapshot_ptr, width)` 缓存折行后 `Vec<Line>` 与 `Vec<LineHit>`（或至少缓存正文 hash-link 解析结果），避免悬停/滚动/spinner 重绘反复扫描；
  - 将 `commit_hash_ref_ranges` 的 7..=40 hex 匹配改为基于 `SnapshotCommitCache` / 排序 OID 前缀索引（或 `PermanentGraph` `oid_to_node` 精确查 + 有序短前缀二分），彻底消除对 `snap.commits` 的全表线性遍历；加载中占位渲染（`commit_details.rs:129`）改用图谱/缓存 `O(1)` 查询；视口渲染行避免不必要的全量 `hits` 嵌套扫。
- [ ] **缓存 `FooterRenderPlan` 并消除 `display_width` 分配**：
  - 将 `FooterPill::display_width()` 改为纯基于 `UnicodeWidthStr::width` 常量加和计算（零 `format!` 堆分配）；
  - 将 `layout_footer_pills` 溢出回退枚举由 `O(P²)` 求和改为单趟 `O(P)` 累加前缀宽度；
  - 在应用/渲染层对 `FooterRenderPlan` 按核心影响状态指纹（当前 tab、聚焦 pane、modal stack 顶部、选中/多选/高亮状态、终端宽度、keymap 代际）跨帧缓存。
- [ ] **净化 Filter 弹窗建议、防抖 SpeedSearch 与异步化折叠构图**：
  - 为 `FilterField::Paths` 建议建立异步/一次性目录缓存（仅在打开弹窗或切仓库时于后台读取一次 `read_dir`，渲染帧纯只读切片）；为 `FilterField::User` 按 `snapshot_ptr` 缓存排序后的高频作者列表；将 `filter_suggestions_with_context` 去重集合由 `Vec::contains` 改为 `FxHashSet`；
  - 为 `LogSpeedSearchState` 接入输入防抖（或增量/ASCII 零分配大小写匹配），避免 5 万提交大表逐键卡顿；
  - 让 `graph/state.rs` 的 `collapse_all`、`expand_all`、`set_segments_collapsed`、`expand_bek_merge` 在 `commits.len() >= ASYNC_GRAPH_THRESHOLD` 时走后台异步图任务（带 `EpochToken` 取消）而非同步卡死 UI 线程；
  - 修复 `has_multi_selection()` 判定顺序使 `!self.selected_rows.is_empty()` 短路为 `O(1)`；消除 `align_labels_right` 双重 `layout_pills`；`ensure_graph_cache` 与 `reconcile_requests` 先通过轻量指针/代际判定是否过期，再决定是否收集 `favorite_branch_names`。
- [ ] **虚拟化 Console 渲染、加内存上界、移除剩余 UI 同步 I/O 与空闲滴答**：
  - `views/console.rs:255-336`：视口切片仅针对 `scroll..scroll + visible_h`（含折叠头部）构造并高亮 `Line`，复杂度从 `O(N)` 降至 `O(H)`；
  - `console_workbench.rs`：将 `ConsoleWorkbench::entries` 改为 `VecDeque` 并设置合理命令历史条数上限（如 `MAX_CONSOLE_ENTRIES = 200`），同时遵循 `#07`/`#08` 架构约定**不对单条普通命令的 `output_lines` 设硬截断**；
  - `stage_view/workbench.rs`：为 `rollback_undo_stack` 增加条目上限（如 20 条）与总字节预算上限（如 `32 MiB`），超出时弹出最旧条目；使用 `tokio::task::spawn_blocking` 替代 `spawn_draft_write` 每次新建 OS 线程；将 `BranchesPaneState::save_settings` 与 `StageDiffUndoRollback` 文件读取移至后台任务或异步效果；
  - `app/events.rs` & `app/state.rs`：在 `request_slots.accept(...)` 确认有效后再置 `needs_redraw = true`；当前不在 `Console` tab 且非底部错误 toast 相关更新时跳过 `ConsoleOutput` 全帧重绘；当仅 `!self.credentials.is_empty()` 而无真实后台进度任务时，停止 10 Hz `spinner_sleep` 空转；
  - `diff_session.rs`（以单调 `collapsed_rev: u64` 替代 `FxHashSet` 相等比对）、`syntax.rs`（借用键或轻量哈希键避免每次 `highlight_single_line` 命中分配 `String`）、`jbgit-core/{details.rs, stage.rs}`（`ChangeDir::count` / `StageDir::count` 改自底向上 `O(F)` 计算，已有目录用 `.get_mut()` 零分配查找）。

---

## 3. 验收标准与回归测试

- **单元测试 / `TestBackend` 渲染测试（修复前可复现，修复后必过）**：
  - `commit_details.rs`：验证含 20 个十六进制串的大提交消息经缓存解析后与旧结果像素/命中区域一致，且二次渲染不扫描 `snap.commits`。
  - `footer.rs`：验证 `layout_footer_pills` 在极窄/中等/宽终端宽度下与原 `O(P²)` 算法输出完全一致；`display_width` 无堆分配结果一致。
  - `graph/state.rs`：验证 `has_multi_selection()` 在选区很大时与原逻辑等价；验证大图模式下折叠/展开走异步通路且旧请求可被取消。
  - `console_workbench.rs` / `views/console.rs` / `rollback_undo_stack`：验证超过 `MAX_CONSOLE_ENTRIES` 时淘汰最旧命令条目、单条命令审计行不被硬截断、虚拟化渲染与全量渲染输出一致、`rollback_undo_stack` 超字节/条数预算时按 FIFO 淘汰旧记录。
  - `jbgit-core/{details.rs, stage.rs}`：验证自底向上 `count` 与原递归计数、零分配目录查找与原 `build_change_rows_filtered` / `build_stage_tree_rows` 输出逐行一致。
- **全量检查**：
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace --locked`

## Comments

- **2026-11-28**：基于 `jbgit-tui` 与 `jbgit-core` 性能审计创建本 P1 工单，解决每帧高频率热点、弹窗同步磁盘读写与会话长周期内存增长问题。
