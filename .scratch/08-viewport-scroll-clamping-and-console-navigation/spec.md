# Spec 08: 全视图滚动上界夹紧（防 Overscroll 卡死）与控制台导航增强规约

- **目录**：`.scratch/08-viewport-scroll-clamping-and-console-navigation/`
- **下属 Issues**：
  - [`01-bounded-viewport-scroll-and-overscroll-fix.md`](./issues/01-bounded-viewport-scroll-and-overscroll-fix.md)
  - [`02-console-navigation-keybindings-and-scroll-indicator.md`](./issues/02-console-navigation-keybindings-and-scroll-indicator.md)
- **关联源码**：
  - `crates/jbgit-tui/src/app.rs`（`Action::ScrollConsoleOutput`、`Action::ScrollDiff`、`Action::ScrollDetails`）
  - `crates/jbgit-tui/src/app_git.rs`（`Action::StashDiffScroll`）
  - `crates/jbgit-tui/src/app_stage.rs`（`Action::StageDiffScroll`）
  - `crates/jbgit-tui/src/views/mod.rs`、`console.rs`、`log_details.rs`、`commit_diff.rs`
  - `crates/jbgit-tui/src/keymap/actions.rs`、`input.rs`

---

## 1. 背景与问题诊断

用户在执行 `git pull` 后于 `[4 Console]` 向下滑动输出时，画面停留在最后一行 `create mode ...` 后无法往回滑动，体感表现为“控制台输出达到上限并卡死”。

经源码排查，**`GitConsoleEntry::output_lines` 并无行数上限**，真正原因是由两类缺陷叠加导致：

1. **滚动偏移量无上界夹紧（Overscroll 积压 Bug）**：
   - `app.rs` 中 `Action::ScrollConsoleOutput(delta)` 仅执行 `(self.console_output_scroll as isize + delta).max(0) as usize`，完全没有上界限制。
   - `views/console.rs` 在渲染时仅对局部变量做 `let scroll = state.console_output_scroll.min(max_scroll);`，**并未将真实 `max_scroll` 写回或钳制 `state.console_output_scroll`**。
   - 当内容滑到底部后，鼠标滚轮/触控板惯性或长按方向键会继续把 `console_output_scroll` 累加到远大于 `max_scroll` 的数值；随后用户往回（上）滑动时，在偏移量减回 `max_scroll` 之前每一帧都仍被截断为 `max_scroll`，导致界面长时间纹丝不动。
   - **全局审计发现同类隐患遍及所有 5 个可滚动文本/Diff 视图**：
     - `state.console_output_scroll`（`app.rs:1678`）
     - `state.details.diff_scroll`（`app.rs:1633`）
     - `state.details.details_scroll`（`app.rs:1637`）
     - `state.stash.details.diff_scroll`（`app_git.rs:434`）
     - `state.stage_workbench.diff_scroll`（`app_stage.rs:298`，上限误用 `len - 1` 而非 `len - viewport_h`，存在一整屏的底部死区）
   - 此外，`GotoBottom`（`G`）通过传 `isize::MAX / 2` 实现；在无上界夹紧的算术 `(scroll as isize + delta)` 下，按一次 `G` 后向上滚几乎永久卡死，连按多次还存在有符号整数溢出风险。
2. **控制台详情视图缺少快捷导航与触底视觉反馈**：
   - `keymap/actions.rs` 中的 `GotoTop`（`gg`）、`GotoBottom`（`G`）、`PageDownHalf` / `PageUpHalf` 未处理 `PaneId::ConsoleOutputDetail`，导致无法一键回到顶部或半页翻页。
   - 控制台输出区域没有任何滚动条或行号/百分比指示，用户滑到真实输出末尾时难以区分“已到底部”还是“输出被截断”。

---

## 2. 设计目标与职责边界

### 2.1 核心目标
1. **零 Overscroll 死区**：所有 5 个文本/Diff/控制台滚动视口在**事件派发阶段**与**每帧渲染收尾阶段**均严格维持不变量 `0 <= scroll <= max_scroll`，滑到底部后任意次继续下滑，第一次上滑立即响应。
2. **溢出安全滚动算术**：提供统一的纯函数 `apply_scroll_delta(current, delta, max_scroll)`，安全处理 `isize::MIN`、`isize::MAX`、连续 `G` 跳转以及内容缩小时的瞬间回弹，杜绝 `isize` 加法溢出。
3. **控制台完整键盘导航**：为 `PaneId::ConsoleOutputDetail` 补齐 `gg`、`G`、`Home`、`End`、`PageUp`/`PageDown`（按真实视口高度）与 `Ctrl+d`/`Ctrl+u`（半页滚动）。
4. **清晰的滚动位置反馈**：在控制台输出面板标题栏与右边缘提供轻量滚动位置指示（行范围/百分比与细滚动条），让用户一眼分辨当前所处位置与是否触底。

### 2.2 非目标（Out of Scope）
- 不为 `GitConsoleEntry::output_lines` 引入强制行数截断（保持完整 Git 输出可见）。
- 不重构 `LogCommitTable` / 列表类基于 `selected_idx` 的视口逻辑（其已有 `last_viewport_h` 与 `max_off` 夹紧）。

---

## 3. 架构设计

### 3.1 纯滚动夹紧算术

在 `crates/jbgit-tui/src/app.rs`（或专用滚动辅助模块）提供纯函数：

```rust
/// Applies a signed scroll delta within `[0, max_scroll]` without integer
/// overflow and without overscroll dead-zone when reversing direction.
pub fn apply_scroll_delta(current: usize, delta: isize, max_scroll: usize) -> usize {
    let clamped = current.min(max_scroll);
    if delta >= 0 {
        clamped.saturating_add(delta as usize).min(max_scroll)
    } else {
        clamped.saturating_sub(delta.unsigned_abs())
    }
}
```

### 3.2 每帧视口度量与回写机制（`ViewportScrollMetrics`）

在 `AppState` 中维护各滚动视口最近一次渲染记录的真实几何参数（无须破坏各子视图函数签名中的 `&AppState` 只读约定，通过 `RenderContext` 收集并在 `render_app` 退出局部作用域后统一写回 `state`）：

```rust
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PaneScrollBound {
    pub viewport_h: usize,
    pub max_scroll: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ViewportScrollMetrics {
    pub console_output: Option<PaneScrollBound>,
    pub log_diff: Option<PaneScrollBound>,
    pub log_details: Option<PaneScrollBound>,
    pub stash_diff: Option<PaneScrollBound>,
    pub stage_diff: Option<PaneScrollBound>,
}
```

- **渲染阶段（`views/mod.rs`）**：
  - `RenderContext` 携带 `scroll_metrics: &mut ViewportScrollMetrics`。
  - 5 个视图在计算出各自的 `visible_h` 与 `max_scroll` 时写入对应字段。
  - `render_app` 在 `RenderContext` 作用域结束后，将结果回写至 `state.scroll_metrics`，并立即对相应 `state.*_scroll` 执行 `.min(bound.max_scroll)` 钳制（覆盖终端缩放、拖动分割线、切换文件/命令或流式输出收缩场景）。
- **派发阶段（`app.rs` / `app_git.rs` / `app_stage.rs`）**：
  - 控制台输出由于可能在两帧之间持续追加流式行，计算上界时优先取动态精确值：
    `total_lines = entry.output_lines.len().max(1) + 2`（含命令头与分隔线），若 `console_output` 已记录 `viewport_h > 0`，则 `max_scroll = total_lines.saturating_sub(viewport_h)`；未曾渲染时回退到 `total_lines.saturating_sub(1)`。
  - 其余 4 个视口（`log_diff`、`log_details`、`stash_diff`、`stage_diff`）若存在 `Some(bound)` 则直接以 `bound.max_scroll` 作为上界调用 `apply_scroll_delta`；若尚未渲染（仅纯状态单元测试场景）则回退到对应已知行数 `len.saturating_sub(1)`（未知时用 `usize::MAX` 并在首帧回写钳制）。

### 3.3 控制台导航键位与滚动位置反馈

1. **键位补齐**：
   - `KeyAction::GotoTop`（`gg`）在 `PaneId::ConsoleOutputDetail` -> `Action::ScrollConsoleOutput(isize::MIN / 2)`
   - `KeyAction::GotoBottom`（`G`）在 `PaneId::ConsoleOutputDetail` -> `Action::ScrollConsoleOutput(isize::MAX / 2)`
   - `KeyAction::PageDownHalf` / `KeyAction::PageUpHalf` 在 `PaneId::ConsoleOutputDetail` -> 按 `(viewport_h / 2).max(1)` 派发 `Action::ScrollConsoleOutput(±step)`
   - `input.rs` 中 `PaneId::ConsoleOutputDetail` 增加 `Home`（顶）、`End`（底），并将 `PageDown` / `PageUp` 步长对齐真实 `viewport_h.saturating_sub(1).max(1)`（未渲染回退 10）。
2. **视觉位置反馈（`views/console.rs`）**：
   - **副标题位置指示**：当 `max_scroll > 0` 时，在 `ConsoleOutputDetail` 面板副标题尾部附加紧凑位置标签，例如 `#1 17:29:38  L121-150/150 (100%)`（内容未超出一屏时不显示多余标记，保持界面清爽）。
   - **右边缘细滚动条**：当 `max_scroll > 0` 且可用宽度足够（`area.width >= 4`）时，预留最右侧 1 列绘制轻量垂直滚动条（轨道 `border_splitter` / 滑块 `fg_muted`），滑块到达最底端时直观呈现“已触底”。

---

## 4. 任务拆分与依赖顺序

1. **Issue 01**（`01-bounded-viewport-scroll-and-overscroll-fix.md`）：引入溢出安全的 `apply_scroll_delta` 与 `ViewportScrollMetrics` 双端夹紧机制，一次性根治控制台、日志详情/Diff、Stash Diff、暂存区 Diff 共 5 个滚动视口的 Overscroll 卡死与 `G` 键溢出问题，并更新/补齐回归测试。
2. **Issue 02**（`02-console-navigation-keybindings-and-scroll-indicator.md`，Blocked by `01`）：为 `ConsoleOutputDetail` 补齐 `gg` / `G` / `Home` / `End` / 视口自适应翻页与半页翻页键位，新增标题栏行号百分比指示与轻量右边缘滚动条，并补齐 TUI 交互与渲染测试。
