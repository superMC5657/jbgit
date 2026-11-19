# 03: 实现双列字符矩阵 DAG 轨道光栅化器与节点/箭头鼠标交互

Status: ready-for-agent
Blocked by: 02, 00-architecture-and-git-engine/issues/01-workspace-tui-runtime-and-hit-test.md, 01-visual-system-darcula-and-widgets/issues/01-darcula-theme-14-color-palette-and-icons.md
Spec: [../spec.md](../spec.md)
Milestone: M2 — 非交叉 DAG 图与 Pill 徽标

- **参考源码**：
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/paint/SimpleGraphCellPainter.kt`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/paint/HeadNodePainter.kt`
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/facade/VisibleGraphImpl.kt` (`performArrowAction`)

---

## 1. 背景与目标

将 `jbgit-core` 计算出的单行抽象 `Vec<GraphPrintElement>` 光栅化渲染到 `ratatui::buffer::Buffer` 的双列/轨字符网格中，完美呈现圆角弯头（`╭╮╰╯`）、多色交汇（`├┤┼`）、虚线折叠边（`┆`）、长边截断箭头（`▾▴`）与高亮圆点（`●◎`），并支持鼠标悬停高亮整条边/箭头与左键点击长边箭头瞬间跳转对端提交！

---

## 2. 核心实现任务清单

- [x] **抽取可插拔渲染后端接口 (`GraphRenderBackend` Trait)**：
  - 按 [Spec 02 第 6 节](../spec.md#6-阶段四双后端抽象与默认字符后端graphrenderbackend-与字符矩阵光栅化器-cellgridgraphbackend) 定义 `GraphRenderBackend`、`GraphViewportSlice` 与 `RowGraphSlice`（携带 `occluding_rects` 遮挡矩形切片），将本 Issue 实现为默认普适后端 `CellGridGraphBackend`，为后续 `04-kitty-sixel-vector-graph-backend` 预留零侵入扩展槽位。
- [x] **四向位掩码字符合成引擎 (`BitmaskGlyphComposer`)**：
  - 为单行内 `2 * max_tracks` 个字符单元建立中间态微缓冲区 `SmallVec<[GraphCell; 32]>`。
  - 处理同槽垂直边（`from == to`）、跨槽斜向/折线边（`from < to` 或 `from > to`），将连接拆解写入起点偶数列、中间奇偶桥接列与终点偶数列的 `up / down / left / right` 连接槽。
  - 通过 16 状态圆角盒线查找表合成精确 Unicode 字符（`│ ─ ╭ ╮ ╰ ╯ ├ ┤ ┬ ┴ ┼`），对 `LineStyle::Dashed` 映射为 `┆` / `╌`。
- [x] **多轨交汇精细着色仲裁**：
  - 当主干垂直线与右侧分支横线在偶数列交汇为 `├` 或 `┼` 时，该格前景取垂直贯通边的轨道色，而紧邻右侧奇数列 `─` 与分支列弯角 `╮`/`╯` 取分支自身轨道色。
  - 最后叠绘本行 `NodePrintElement`（`●` 普通节点、`◎` HEAD 金色/双环节点、`○` 未匹配过滤占位节点），节点恒位于最上层不被边线覆盖。
  - 当某条边或节点处于悬停/选中高亮态（`is_selected == true`）时，启用 `Modifier::BOLD` 及高亮色增强对比（对标 `SimpleGraphCellPainter` 的粗描边选中效果）。
- [x] **轨道元素鼠标命中与长边箭头点击跳转**：
  - 在渲染每一行图元素时，为 `NodePrintElement` 与带箭头的 `EdgePrintElement` / `TerminalEdgePrintElement` 注册 `HitTarget::GraphPrintElement`。
  - 复刻 `VisibleGraphImpl.performArrowAction`（第 94-129 行）：
    - **鼠标悬停在长边截断箭头 `▾`/`▴` 上**：高亮该边并在状态栏提示对端提交短哈希与标题；
    - **鼠标左键点击箭头 `▾`/`▴`**：提交表格立即滚动并居中选中该长边连接的远端父/子提交，同时记录导航历史支持 `Alt+Left` 返回！

---

## 3. 边界情况与异常处理

- **水平空间受限截断**：当极端并发仓库某行轨道数超过表格分配给图形区的最大列数上限（例如上限为表格宽度的 45%）时，安全裁剪超出列并在图右侧边缘渲染柔和省略标记 `…`，绝不越界写入提交标题区域或引发 `Buffer` 越界 panic。

---

## 4. 验收标准 (Acceptance Criteria)

- [x] `insta` 文本 + ANSI 颜色快照测试：覆盖线性链、单分支分叉与合并、连续两个分支同点合入、交叉跨列合并、长边截断上下箭头、虚线过滤边共 6 种标准拓扑模板，确保无断线、无错位尖角。
- [x] 交互测试：在跨度 > 30 行的长边截断箭头 `▾` 处触发模拟鼠标左键点击，表格准确跳转并选中第 `down` 行的目标父提交。

---

## Comments

### 2026-11-18 — implemented

- 代码实现位于 `crates/jbgit-tui/src/graph/{mod, cell_grid, state}.rs`。
- 定义 `GraphRenderBackend` trait 与 `CellGridGraphBackend` 默认字符网格后端。
- `BitmaskGlyphComposer` 实现四向位掩码盒线合成（含圆角弯头、交叉着色与虚线过滤边）。
- 鼠标命中支持 `HitTarget::GraphPrintElement`，长边截断箭头点击平滑居中跳转远端父/子提交，并支持 `Alt+Left` 返回。
- 视口宽度受限时平滑截断并附加 `…`，insta 快照测试全量通过。
