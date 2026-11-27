# 05: 矢量图垂直滚动 `copy_within` 行搬移、命中区缓存、Sixel 量化加速与 `Resize` 合并

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M13 — 大仓库、渲染热路径与引擎并发性能二期治理
Priority: P1

- **关联文件**：
  - `crates/jbgit-tui/src/graph/vector/mod.rs`（`PixelVectorGraphBackend::try_render` `:238-246`，`rasterize` `:414-461`，`register_hits` `:467-582`，`finish_frame` `:644-712`）
  - `crates/jbgit-tui/src/graph/vector/encode.rs`（`encode_png` `:70-80`，`encode_sixel` `:177-306`，`nearest` `:308-321`）
  - `crates/jbgit-tui/src/graph/renderer.rs`（`GraphRenderer::set_capabilities` `:337-341`）
  - `crates/jbgit-tui/src/session.rs`（`ProdHost::on_resize` `:190-221`，`TuiSession::ingest_into_state` `:520-531`）
  - `crates/jbgit-tui/src/hit_test.rs`（`HitTestRegistry::hit_test` `:555-573`）

---

## 范围修订（2026-11-28）

> 当前需求以本节为准；下文原始根因与任务清单保留作审计记录，未勾选项以本节结论为准。

- **本轮完成**（Commit `042aa3b`）：`GraphRenderer::set_capabilities` 在能力未变时不再 `invalidate()`；`collapse_ready_resizes` 合并同批连续 `Resize`，只按最终尺寸处理一次，后续事件顺延到新尺寸重绘之后。
- **不做**：矢量条带 `copy_within` 滚动搬移、`HitRegion` 按 `content_key` 缓存与水平合并、Sixel 量化加速——仅 Kitty / Sixel / iTerm2 矢量模式受益且实现复杂；若后续默认启用矢量图后端再重新评估。

## 1. 根因定位

### 1.1 垂直滚动 1 行导致矢量图行缓存 100% 失效并触发全条带重绘与同步编码
- 在 `crates/jbgit-tui/src/graph/vector/mod.rs:414-461` 中，`row_hashes` 按**屏幕行索引 `i`** 存储。当 Log 列表垂直滚动 1 行时，原来位于 `i + 1` 的全部可见行挪到了 `i`，导致几乎所有 `i` 的 `self.row_hashes[i] != row.row_hash`，缓存命中率跌至 **0%**！
- 在 HiDPI 终端（单元格 `16×32` 或 `21×45` px、40–60 行、10+ 轨道）下，每次 1 行滚动都会重新用 `tiny-skia` 光栅化全部 40–60 行，并在 UI 线程同步编码整条 RGBA 图像（`encode_png` + Base64 或 `encode_sixel`）。

### 1.2 Sixel 抗锯齿颜色量化存在 `O((K - 256) × 256)` 暴力最近邻查找
- `crates/jbgit-tui/src/graph/vector/encode.rs:230-233, 308-321` 在抗锯齿矢量条带出现 `K > 256` 种颜色时，对超出调色板的每种颜色遍历全部 256 项运行 `nearest(&scratch.palette, c)`；同时每次重传都在 UI 线程跑完整直方图与 6 行带扫描。

### 1.3 每帧无条件重算几何命中区且推送上千个 1-cell `HitRegion`
- `register_hits`（`vector/mod.rs:467-582`）在**每一帧**都会运行，哪怕 `raster_key == Some(content_key)`（图谱内容未发生任何变化）；对每条边在其跨越的每一列逐像素行调用 `g.is_over_edge(...)`（含 3 次 `f32::hypot`），并为每个 `(row, col, edge)` 独立注册一个 `1×1` 的 `HitRegion`。密集 50 行 × 12 轨道每帧会推入 **1,000+ 个区域**，进而拖慢 `MouseMove` 时 `HitTestRegistry::hit_test`（`hit_test.rs:555-573`）的线性查找。

### 1.4 拖拽缩放窗口时既未合并 `Resize` 事件又无条件清空图形缓存
- `crates/jbgit-tui/src/session.rs:520-531` 的事件批合并遇到 `Event::Resize` 便立即打断批次（`|| is_resize`），导致拖动终端边框时每一个中间像素尺寸变化都同步触发完整 `ProdHost::on_resize`（`session.rs:190-221` 含环境探测与 `window_size()`）+ `GraphRenderer::set_capabilities` + 矢量条带全量重绘编码与发送。
- 并且 `GraphRenderer::set_capabilities`（`renderer.rs:337-341`）**无论新旧 `caps` 是否相等都会调用 `self.invalidate()`**，使一切尺寸不变或能力等价的回调也强制作废全条带缓存。

---

## 2. 任务清单

- [ ] **实现矢量条带垂直滚动 `copy_within` 行级搬移**：
  - 在 `PixelVectorGraphBackend::rasterize`（`crates/jbgit-tui/src/graph/vector/mod.rs`）记录上一帧各屏幕行对应的稳定行键（例如 `(row.visible_row, row.row_hash)` 或检测统一行偏移 `delta`）；
  - 当布局宽度/单元格像素/主题色未变且发生纯垂直平移（如 `+1` / `-1` / `+page` 行）时，通过 `self.strip.copy_within(...)`（按向上/向下方向注意不覆盖未拷贝行）直接整块搬移保留的重叠行像素与对应 `row_hashes`，仅对新露出或确实变更的 1–N 行调用 `self.painter.paint_row(...)`。
- [ ] **缓存矢量几何 `HitRegion` 并合并水平连续单元格**：
  - 在 `PixelVectorGraphBackend` 中按 `content_key`（及 `area` 偏移基准）缓存计算好的相对 `(rel_x, rel_y, w, h, target)` 命中矩形列表；当 `content_key` 未变时直接平移偏移量后快速注册，跳过所有 `is_over_edge` 像素 `hypot` 循环；
  - 对同一行同一边的连续水平命中列合并为单个跨多列的 `Rect`（而非逐列注册 `1×1` 矩形），大幅减少 `HitTestRegistry` 区域数量。
- [ ] **加速 Sixel 抗锯齿颜色量化（`encode.rs`）**：
  - 在 `crates/jbgit-tui/src/graph/vector/encode.rs` 中优化超出 256 色时的色映射（例如使用 RGB555/RGB444 查找表或缓存颜色映射到调色板索引，替代每次遍历 256 个调色板项的 `nearest` 暴力扫描），并保持复用 `SixelScratch` 缓冲零逐帧新分配。
- [x] **合并 `Event::Resize` 并修复 `GraphRenderer::set_capabilities` 无条件 `invalidate()`**：
  - `crates/jbgit-tui/src/graph/renderer.rs:337-341`：仅当 `self.caps != caps` 时才更新并执行 `self.invalidate()`；
  - `crates/jbgit-tui/src/session.rs:520-531`：在单轮事件合并循环（`MAX_COALESCE_EVENTS = 64`）中若连续遇到多个 `Event::Resize`，覆盖合并仅保留并应用最后一次终态尺寸（同时保持不跨模态切换/退出事件安全边界），减少拖拽窗口时的无效探测与终端图像帧洪水。

---

## 3. 验收标准与回归测试

- **单元测试（修复前可复现，修复后必过）**：
  - `vector/mod.rs`：模拟 40 行矢量图向下滚动 1 行、向上滚动 1 行及跳页滚动，断言新 `strip` RGBA 字节与全量重新栅格化结果**逐字节完全一致**，且 `last_rasterized_rows` 等于实际新增/变更行数（滚动 1 行时 `<= 2`，不再等于 `40`）。
  - `vector/mod.rs` (`register_hits`)：断言合并连续命中区间后，在 `area` 内每个单元格坐标的 `hit_test` 返回与逐格注册完全一致的 `HitTarget`，且连续同图重绘不重新执行几何计算；`HitRegion` 总数显著下降。
  - `vector/encode.rs`：构造含 > 1,000 种抗锯齿渐变像素的图像，验证优化后 Sixel 编码输出合法 Sixel 序列且色彩映射正确性无退化。
  - `renderer.rs` & `session.rs`：验证传入相同 `GraphicsCapabilities` 时 `set_capabilities` 不清空现有条带缓存；验证同一批事件中连续 5 个 `Event::Resize` 合并为最终尺寸一次生效。
- **全量检查**：
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace --locked`

## Comments

- **2026-11-28**：基于 `jbgit-tui` 矢量图后端与事件循环性能审计创建本 P1 工单，治理 HiDPI 终端下矢量图滚动重绘、Sixel 量化、命中区膨胀及缩放卡顿。
- **2026-11-28**：用户将本工单范围收窄为两项：`GraphRenderer::set_capabilities` 去掉无条件 `invalidate()`，以及合并 `Event::Resize`。改动：(1) `renderer.rs` 中 `set_capabilities` 在新旧 `caps` 相等时直接返回（`GraphicsCapabilities` 已含 `cell_px`，字体缩放导致的单元格像素变化仍会作废缓存；纯终端尺寸变化由光栅/发送 key 中的视口几何自行处理，Kitty 使用 `U=1` 虚拟放置、Sixel/iTerm2 的 `emitted_key` 含 `terminal_area`）；(2) `session.rs` 新增 `collapse_ready_resizes`：一个 `Resize` 到达后继续非阻塞地拉取已就绪的连续 `Resize` 只保留最后一个，仅调用一次 `on_resize`；连续段之后遇到的第一个非 `Resize` 事件（或流结束）由 `main.rs` 暂存到 `carried_terminal`，在按新尺寸重绘后的下一轮最先处理，语义与原先 `Resize` 打断批次一致（退出/终端交接/模态边界不变）；`TuiSession::step_turn` 同样把相邻的 `Resize` 输入折叠为最后一个。测试：`set_capabilities_only_invalidates_when_caps_change`、`tui_session_collapses_consecutive_resizes_into_one_on_resize`、`collapse_ready_resizes_keeps_last_size_and_carries_the_next_event`；`cargo test -p jbgit-tui --locked` 与 `cargo clippy -p jbgit-tui --all-targets -- -D warnings` 通过。其余三项（`copy_within` 滚动搬移、命中区缓存/合并、Sixel 量化加速）延后：它们只对 Kitty/Sixel 矢量模式有收益。
- **2026-11-28（范围修订）**：按文首「范围修订」节更新 `Status: resolved`；暂缓项汇总至 [`07`](./07-deferred-perf-candidates.md)，不做项不再跟踪。
