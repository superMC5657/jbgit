# 04: 实现基于 `tiny-skia` 与 Kitty/Sixel 图形协议的像素级抗锯齿矢量轨道后端（可选增强）

Status: ready-for-agent
Blocked by: 03, 03-vcs-log-three-pane-linkage/issues/02-vcs-log-table-and-filter-bar.md, 05-branches-popup-context-menus-and-workflows/issues/02-context-menu-framework-and-mouse-ux.md
Spec: [../spec.md](../spec.md)
Milestone: M6 — 现代图形终端高保真增强（可选扩展，不阻塞 M1~M5 全功能交付）

- **参考源码**：
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/paint/PaintParameters.java`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/paint/SimpleGraphCellPainter.kt`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/paint/HeadNodePainter.kt`

---

## 1. 背景与目标

在默认 `CellGridGraphBackend`（圆角字符网格）100% 覆盖所有终端的基础上，针对支持 **Kitty Graphics Protocol / Sixel / iTerm2 Inline Images** 的现代终端（WezTerm、Kitty、Ghostty、Windows Terminal 1.22+、iTerm2），基于纯 Rust 2D 矢量库 **`tiny-skia`** 实现 `PixelVectorGraphBackend`，1:1 复刻 IntelliJ IDEA `SimpleGraphCellPainter` 与 `HeadNodePainter` 的亚像素抗锯齿斜向分叉线、异色交叉无损叠加以及三层同心圆 HEAD 靶心节点。

---

## 2. 核心实现任务清单

- [ ] **Cargo Feature 与终端图形协议探测 (`graphics-protocol` Cargo Feature)**：
  - 引入 `tiny-skia` 与终端图像协议编码支持（集成 `ratatui-image` 或轻量自研 Kitty RGBA/PNG 分块编码器 + Sixel 调色板量化编码器 + iTerm2 `OSC 1337` 编码器）。
  - 启动及 `Event::Resize` 时调用 `crossterm::terminal::window_size()` 校准单字符格像素宽高 `(cell_px_w, cell_px_h)`；自动识别不支持图形协议或未开启透传的 `tmux`/SSH 环境，静默回退至 `CellGridGraphBackend`。
  - 在 `[⚙ Presentation]` 设置菜单及快捷键中提供 `Graph Renderer: Auto | Vector (Kitty/Sixel) | Character Grid` 实时切换。
- [ ] **`tiny-skia` 精确复刻 `SimpleGraphCellPainter` + `HeadNodePainter`**：
  - 按 `PaintParameters.scaleWithRowHeight(val, cell_px_h)` 计算动态线宽（`1.5 * h / 22`）、选中加粗线宽（`2.5 * h / 22`）与圆点半径（`4.0 * h / 22`）。
  - 实现**双倍跨行无缝对接斜线**（在单行 Clip 区域内从 `(x1, row_center)` 绘制至 `(x2, row_center ± row_h)`），确保相邻行斜线在边界处斜率与亚像素抗锯齿边缘 100% 对接无断缝，且异色交叉轨道各自保留真实颜色。
  - 实现自适应虚线步长（`getDashLength`）与旋转箭头翼片（`ARROW_ANGLE_COS2 = 0.7, ARROW_LENGTH = 0.3`）。
  - 实现 `HeadNodePainter` 的外环轨道色 + 中环 `row_bg` 镂空 + 内核实心圆点的三层同心圆绘制。
- [ ] **整条视口条带合成 (`ViewportStripCache`) 与性能优化**：
  - 将可见视口整个图形列区域 `(area.width * cell_px_w) × (area.height * cell_px_h)` 合成为单张连续 `Pixmap`（每行预填对应 `row_bg` 背景色，彻底消除 Sixel 6 像素高度对齐横纹与逐行多图传输开销）。
  - 建立帧级脏哈希缓存：滚动偏移、选中行、悬停元素及窗口尺寸未变化时不重复执行光栅化与图像重传。
- [ ] **Z-Order 浮窗与右键菜单遮挡防护**：
  - 在支持 Kitty Unicode Placeholders (`U=1`, `U+10EEEE`) 的终端上使用字符格占位模式，天然兼容 `ratatui::widgets::Clear` 擦除；
  - 在 Sixel / iTerm2 等位图置于文本上层的终端上，当 `GraphViewportSlice::occluding_rects`（如右键菜单、`BranchesPopup`、确认对话框）与图形区 `area` 相交时，自动对相交区域裁剪镂空或将该帧无缝切回 `CellGridGraphBackend`，严防图形穿透遮盖上层菜单文字！
- [ ] **亚像素级鼠标悬停/点击命中增强**：
  - 当终端开启像素级鼠标坐标报告（SGR-Pixels `1016` 模式）或按单元格映射时，复用 `SimpleGraphCellPainter.isOverNode` 与 `isOverEdge` 的椭圆/线段距离公式精确高亮悬停轨道与长边跳转箭头。

---

## 3. 边界情况与不变量

- **终端字体缩放（`Ctrl +` / `Ctrl -`）**：触发 `Resize` 事件重新查询 `window_size()` 后，必须立即失效旧像素尺寸缓存并按新 `(cell_px_w, cell_px_h)` 重建条带，防止图像与右侧表格文字行错位。
- **高分屏 HiDPI（200% 缩放，如 4K 显示器下单行高 44px）**：限制条带最大纹理上限并复用预分配 `Pixmap` 内存缓冲区，零每帧堆分配，保证单次重绘 + 编码耗时 ≤ 2ms。

---

## 4. 验收标准 (Acceptance Criteria)

- [ ] 离线金标图像测试（Golden PNG Test）：给定多分支分叉、合流、异色十字交叉、虚线边与 HEAD 节点测试集，断言 `tiny-skia` 输出的 `Pixmap` 在跨行边界 `y = k * cell_px_h` 处连续无 1px 断层，且交叉中心保留上层轨道抗锯齿颜色。
- [ ] 遮挡回归测试：在开启矢量轨道模式下于图形区正上方打开右键上下文菜单或 `BranchesPopup`，验证菜单区域 100% 清晰无底层轨道线条穿透。
- [ ] 降级测试：模拟 `window_size()` 返回像素为 0 或关闭图形能力时，自动零报错切换至 `CellGridGraphBackend` 且布局列宽与交互热区完全一致。

---

## Comments
