# 02: 补齐控制台详情页导航键位与滚动位置视觉指示

Status: ready-for-agent
Blocked by: 01
Spec: [../spec.md](../spec.md)
Milestone: M6+ — 交互体验与滚动稳定性修复

- **关联文件**：
  - `crates/jbgit-tui/src/keymap/actions.rs`
  - `crates/jbgit-tui/src/input.rs`
  - `crates/jbgit-tui/src/views/console.rs`
  - `crates/jbgit-tui/src/views/header.rs`（如需核对控制台快捷键提示）

---

## 1. 背景与目标

Issue 01 修复了滚动上界夹紧后，用户在 `[4 Console]` 详情面板仍有两点体验短板：
1. 常用跳转快捷键 `gg`（回到顶部）、`G`（跳到底部）、`Home` / `End` 与半页翻页（`Ctrl+d` / `Ctrl+u`）在 `PaneId::ConsoleOutputDetail` 上未接入，且 `PageUp` / `PageDown` 硬编码为固定 10 行而非按视口高度自适应。
2. 控制台输出区缺少滚动进度指示与滚动条，面对较长 `git pull` 输出时无法直观判断当前是否已经到达最后一行。

本 Issue 补齐上述键盘导航并增加克制、清晰的滚动位置指示与轻量滚动条。

---

## 2. 核心任务清单

- [ ] **补齐 `PaneId::ConsoleOutputDetail` 键盘导航**：
  - 在 `crates/jbgit-tui/src/keymap/actions.rs` 中：
    - `KeyAction::GotoTop`（`gg`）：为 `PaneId::ConsoleOutputDetail` 派发 `Action::ScrollConsoleOutput(isize::MIN / 2)`；
    - `KeyAction::GotoBottom`（`G`）：为 `PaneId::ConsoleOutputDetail` 派发 `Action::ScrollConsoleOutput(isize::MAX / 2)`；
    - `KeyAction::PageDownHalf` / `KeyAction::PageUpHalf`：当焦点在 `PaneId::ConsoleOutputDetail` 时，按 `(viewport_h / 2).max(1)`（未渲染回退 10）派发 `Action::ScrollConsoleOutput(±step)`。
  - 在 `crates/jbgit-tui/src/input.rs` 的 `PaneId::ConsoleOutputDetail` 分支中：
    - 增加 `KeyCode::Home` -> `Action::ScrollConsoleOutput(isize::MIN / 2)`；
    - 增加 `KeyCode::End` -> `Action::ScrollConsoleOutput(isize::MAX / 2)`；
    - 将 `KeyCode::PageDown` / `KeyCode::PageUp` 的步长改为基于 `state.scroll_metrics.console_output` 的 `viewport_h.saturating_sub(1).max(1)`（未渲染回退 10）。
- [ ] **控制台详情区滚动位置指示与轻量滚动条（`crates/jbgit-tui/src/views/console.rs`）**：
  - **副标题行号/百分比指示**：在 `render_console_tab` 构建 `ConsoleOutputDetail` 副标题时，若当前选中命令的总行数超过可见高度（`max_scroll > 0`），追加紧凑的滚动状态（例如 `#1 17:29:38  L121-150/150 (100%)`）；内容未超出一屏时维持原有 `#1 17:29:38` 格式不增噪音。
  - **右边缘 1 列垂直滚动条**：在 `render_command_output` 中，当 `max_scroll > 0` 且 `area.width >= 4` 时，右侧 1 列绘制轻量垂直滚动条（轨道用 `ctx.theme.border_splitter`，滑块用 `ctx.theme.fg_muted`，文本内容在剩余 `area.width - 1` 列内渲染），滑块长度与位置按 `visible_h / total_lines` 和 `scroll / max_scroll` 等比计算。
- [ ] **补齐交互与渲染测试**：
  - 单元测试验证在 `PaneId::ConsoleOutputDetail` 下触发 `gg`、`G`、`Home`、`End`、`PageUp`、`PageDown`、`Ctrl+d`、`Ctrl+u` 均正确派发并落地到预期 `console_output_scroll`。
  - `TestBackend` 渲染测试验证：
    1. 输出不足一屏时，副标题不显示百分比且不绘制多余滚动条；
    2. 输出超过一屏时，顶部显示 `0%`、滑到底部显示 `100%` 且最右列出现滚动条滑块。

---

## 3. 验收标准 (Acceptance Criteria)

- [x] `cargo test --workspace` 与 `cargo clippy --workspace --all-targets` 全部通过。
- [x] 在 `[4 Console]` 详情页按 `G` / `End` 立即跳到底部并显示 `100%`，按 `gg` / `Home` 立即回到顶部并显示 `0%`，按 `Ctrl+d` / `Ctrl+u` / `PageDown` / `PageUp` 按视口比例顺滑翻页。
- [x] 较长输出时右侧细滚动条与副标题百分比同步更新，触底状态一目了然；短输出时界面保持极简无冗余装饰。

---

## Comments

- 2026-11-19 (agent): 拆分为独立交互与视觉反馈 Issue，依赖 Issue 01 提供的 `ViewportScrollMetrics` 与上界夹紧机制。
- 2026-11-19 (agent): 已补齐 `ConsoleOutputDetail` 导航键位（`gg`/`G`/`Home`/`End`/`PageUp`/`PageDown`/`Ctrl+d`/`Ctrl+u`）、副标题行号百分比与右边缘 1 列垂直滚动条，并补齐交互与渲染测试；`cargo test --workspace` 与 `cargo clippy --workspace --all-targets` 通过。
