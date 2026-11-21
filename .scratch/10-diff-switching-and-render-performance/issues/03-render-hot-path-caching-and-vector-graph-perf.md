# 03: 每帧渲染缓存、图谱缓存持久化与矢量图增量更新

Status: done
Spec: [../spec.md](../spec.md)
Milestone: M7 — 性能架构治理与交互零卡顿优化 (P1)

- **关联文件**：
  - `crates/jbgit-core/src/graph/edges_in_row.rs`
  - `crates/jbgit-tui/src/graph/state.rs`
  - `crates/jbgit-tui/src/graph/vector/mod.rs`
  - `crates/jbgit-tui/src/graph/vector/encode.rs`
  - `crates/jbgit-tui/src/keymap/mod.rs`
  - `crates/jbgit-tui/src/keymap/resolve.rs`
  - `crates/jbgit-tui/src/views/mod.rs`
  - `crates/jbgit-tui/src/views/footer.rs`
  - `crates/jbgit-tui/src/views/log.rs`
  - `crates/jbgit-tui/src/views/log_details.rs`
  - `crates/jbgit-tui/src/views/commit_diff.rs`
  - `crates/jbgit-tui/src/log_view.rs`
  - `crates/jbgit-tui/src/stage_view.rs`
  - `crates/jbgit-tui/src/app_stage.rs`
  - `crates/jbgit-tui/src/input.rs`

---

## 1. 背景与目标

源码全量审计显示，在常规终端操作中，以下两个放大器会以极高频率触发全屏重绘：
1. **鼠标划过终端格子**：`input.rs:736-765` 在 `hover_target` 变化时即触发重绘；
2. **后台任务运行期间的 100 ms spinner**：`main.rs:149-155` 每 100 ms 触发整屏重绘。

而当前每一帧 `render_app` 都在重复执行大量毫无必要的 O(N) / O(K^2) 重建工作：每帧新建 `PrintElementGenerator` 导致 `EdgesInRowGenerator` 的 LRU 缓存零命中并重扫全图边；Sixel / Kitty 矢量图只要移动光标或 hover 改变单行背景就整条重栅格化；Footer 快捷键提示同帧计算两遍且每次都在循环内重解析 `KeySequence` 字符串；`change_rows()`、`sync_rows()`、`diff_rows()`、`refs` 查询和文本过滤正则每帧反复重建。

本 Issue 聚焦 P1 帧率与交互跟手度优化：消除全部高开销的每帧冗余重建与拷贝，并让矢量图支持行级增量重栅格化。

---

## 2. 核心任务清单

- [x] **持久化 `EdgesInRowGenerator` 缓存（`edges_in_row.rs`, `graph/state.rs`, `views/log.rs`）**：
  - 将 `EdgesInRowGenerator` 拥有的 `ultra_long_edges` 与 `above_anchor_cache` / `below_anchor_cache`（或封装为独立的 `PrintElementCache` 状态）持久化保存在 `CachedLogGraph`（`RefCell` 或可变状态）中，而非在 `views/log.rs:459-460` 每帧新建销毁。
  - 当仅在视口滚动、光标移动、hover 或 100 ms spinner 重绘时跨帧命中 `20` 块锚点缓存且不再重扫全部 `visible.linear.down_edges_data`；仅在 `visible` 重建时重置该缓存。
- [x] **预解析键位序列与合并单帧 Footer 计算（`keymap/mod.rs`, `keymap/resolve.rs`, `views/mod.rs`, `views/footer.rs`）**：
  - 在配置加载与重载时为 `KeymapConfig` 建立预解析缓存（将 `binding.on` 一次性解析为 `Option<KeySequence>`），让 `has_earlier_conflict`、`resolve_key_in_scopes`、`collect_claimed_keys` 与 `match_bindings_filtered` 彻底告别每帧/每次按键的字符串切分与大小写分配。
  - 在 `views/mod.rs` 单帧渲染流中将 `footer_bar_height`（L79）与 `render_footer_bar`（L178）合并为共享同一份当帧 `FooterRenderPlan`（或基于 `(active_tab, focused_pane, modal_stack, pending_keys, hover_override, width, config_gen)` 的轻缓存），避免同帧调用两次 `collect_footer_hints`。
- [x] **缓存视图衍生树与 Diff 展平行（`log_view.rs`, `stage_view.rs`, `app_stage.rs`, `views/commit_diff.rs`, `views/log_details.rs`）**：
  - `DetailsPaneState`（`log_view.rs:478`）：缓存 `cached_change_rows: Vec<ChangeTreeRow>`，按 `(current_key, bundle_ptr, changes_group_by_dir, collapsed_generation)` 失效，消除单帧渲染 + 多处选中状态查询时的 5–6 次 `BTreeMap` 目录树重建（需同时兼顾 `Stash` 复用的 `DetailsPaneState`）。
  - `StageWorkbenchState::sync_rows`（`stage_view.rs:470`、`app_stage.rs:40, 796`）：记录上次构建指纹 `(status_ptr, amend_ptr, tree_opts, collapsed_generation)`，同一按键下未变化时直接跳过，避免两次重复重建 `rows`。
  - Diff 行表缓存：在 `DetailsPaneState` 与 `StageWorkbenchState`（必要时含 `FileHistoryState`）缓存已展开的 `diff_rows`（按 `(diff_arc_ptr, side_by_side)` 失效），让渲染循环与 `StageDiffMoveCursor`（`app_stage.rs:283`）共享，消除每次滚屏重建整张 `Vec`。
- [x] **优化 Log 提交表与 Details Refs 索引与正则复用（`views/log.rs`, `views/log_details.rs`, `log_view.rs`, `views/mod.rs`）**：
  - 在 `CachedLogGraph` 构建/刷新时一次性建立 `refs_by_oid: FxHashMap<Oid, Vec<usize>>` 与 `ref_kind_by_name: FxHashMap<String, RefKind>`，替换 `views/log.rs:612-616` 与 `views/log_details.rs:946` 每帧对全量 `snap.refs` 的线性扫描。
  - 在 `FilterBarState`（`log_view.rs`）缓存已编译好的文本正则匹配器（仅在 `spec.text` 变更时重编），`views/log.rs:461` 渲染时直接只读借用，不再每帧 `compile()`。
  - 梳理 `views/mod.rs:122-123` 的所有权拆解（将只读配置/主题封装为不可变引用或使用 `Arc<ThemePalette>` / `Arc<BranchColorManager>`），消除每帧整块克隆 `theme` 与 `branch_colors`。
- [x] **矢量图按行局部重栅格化与 Sixel 编码提速（`graph/vector/mod.rs`, `encode.rs`, `input.rs`）**：
  - 拆分 `PixelVectorGraphBackend` 缓存策略：除全局维度与偏移外，记录每一可见行的内容签名 `(elements_hash, colors_hash, row_bg)`。当尺寸、总行数与拓扑元素均未改变、仅光标选区或 hover 导致 1–2 行的 `row_bg` 改变时，**仅重栅格化受影响的行像素切片回填到复用的 `self.strip` 缓冲**，不再整条重画 `tiny-skia` `PixmapMut`。
  - 在 `encode.rs:264-270` 使用定长栈数组 `[bool; 256]` 标记本带（band）已出现调色板索引，替代热像素循环内的线性 `scratch.band_colors.contains(&ci)`。
  - 在 `input.rs:736-765` 优化 `MouseEventKind::Moved`：对并未改变实际视觉效果的 `hover_target` 状态转移（例如在普通空白面板间移动且不显示悬浮高亮或 tooltip 时）避免无意义全屏重绘。
- [x] **验证与基准测试**：
  - 为 `EdgesInRowGenerator` 增加跨帧连续上下微幅查询测试，断言锚点缓存命中次数递增而非归零。
  - 为 `DetailsPaneState::change_rows`、`StageWorkbenchState::sync_rows`、`diff_rows` 与 `ParsedKeymap` 增加命中/失效单元测试（折叠目录、切换 side-by-side、更新 status/bundle 后必刷新）。
  - 为 `PixelVectorGraphBackend` 增加单行 `row_bg` 切换测试：验证局部行更新输出的最终 RGBA `strip` 字节与全量整条重栅格化逐字节完全一致。

---

## 3. 验收标准 (Acceptance Criteria)

- [x] `cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
- [x] 在 Log 视图与 Commit 视图渲染路径中，`PrintElementGenerator` / `EdgesInRowGenerator` 实现跨帧持久化复用，`change_rows` / `diff_rows` / `TextFilter` 正则 / `KeySequence` 解析均不再每帧从零重建。
- [x] 启用矢量图后端（如 Windows Terminal Sixel 模式）上下切换提交行时，仅重栅格化发生背景或悬浮变化的行且像素结果与全量重绘完全一致。
- [x] 100 ms 后台 spinner 运行或鼠标划过常规列表行期间，帧耗时显著下降、UI 无明显卡顿或闪烁。

---

## Comments

- 2026-11-21 (agent): 汇集每帧渲染热路径、矢量图重栅格化与键位/底栏/树表缓存发现，作为 P1 流畅度治理工单。
- 2026-11-21 (agent): Issue 03 全量完成并验证：
  1. `EdgesInRowCache` 成功从生成器中剥离并持久化保存在 `CachedLogGraph.edges_cache: RefCell<EdgesInRowCache>`，跨帧复用锚点缓存；
  2. `KeymapConfig` 实现 `ParsedKeyCache` 与 `precompile_all()`，所有作用域按键解析彻底静态化，并合并 Footer 为 `FooterRenderPlan` 单帧轻布局；
  3. 视图衍生树与 Diff 行表全量接入 `Arc<[T]>` 指针与状态哈希缓存，`RenderContext` 移除主题/颜色克隆；
  4. `PixelVectorGraphBackend` 实现 `row_hashes` 驱动的行级增量重栅格化，Sixel 调色板索引判重转为 `[bool; SIXEL_MAX_COLORS]` 栈数组 O(1) 查询；
  5. `HitTarget::has_visual_hover` 门禁普通被动面板移动事件，杜绝空白区域鼠标抖动导致的无意义全屏重绘；
  6. 视口可见区懒计算 `word_diff` 支持超长或超预算行；
  7. 全工作区 284 项单元与集成测试通过，`cargo clippy --workspace --all-targets -- -D warnings` 零告警通过。
