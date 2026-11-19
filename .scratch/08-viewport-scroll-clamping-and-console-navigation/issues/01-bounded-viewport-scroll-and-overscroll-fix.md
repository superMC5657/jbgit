# 01: 全视图滚动上界双端夹紧与 Overscroll 卡死修复

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M6+ — 交互体验与滚动稳定性修复

- **关联文件**：
  - `crates/jbgit-tui/src/app.rs`
  - `crates/jbgit-tui/src/app_git.rs`
  - `crates/jbgit-tui/src/app_stage.rs`
  - `crates/jbgit-tui/src/views/mod.rs`
  - `crates/jbgit-tui/src/views/console.rs`
  - `crates/jbgit-tui/src/views/log_details.rs`
  - `crates/jbgit-tui/src/views/commit_diff.rs`
  - `crates/jbgit-tui/src/main.rs`（现有测试夹具校准）

---

## 1. 背景与目标

当前所有 5 个可滚动内容视口（控制台输出、Log Diff、Log Details、Stash Diff、Stage Diff）在滚动事件派发时均未按真实 `max_scroll` 做上界夹紧，且渲染阶段仅截断局部变量而不回写 `AppState`。这导致：
1. 用户在控制台或 Diff 底部继续向下滚动后，偏移量无限积压，往回（上）滚时出现长时间死区、看似卡死。
2. 在 Log Details / Diff 按 `G`（传 `isize::MAX / 2`）后，向上滚几乎永久无法移动，连按多次还存在 `isize` 加法溢出风险。

本 Issue 建立统一的溢出安全滚动函数与每帧视口上界回写机制，彻底根治全部 5 个视口的 Overscroll 问题。

---

## 2. 核心任务清单

- [ ] **实现溢出安全的纯滚动辅助函数（`crates/jbgit-tui/src/app.rs`）**：
  - 实现 `pub fn apply_scroll_delta(current: usize, delta: isize, max_scroll: usize) -> usize`：
    - 先将 `current` 夹紧至 `max_scroll`；
    - 正增量使用 `saturating_add(delta as usize).min(max_scroll)`；
    - 负增量使用 `saturating_sub(delta.unsigned_abs())`（安全覆盖 `isize::MIN`）。
- [ ] **新增每帧视口滚动边界度量（`ViewportScrollMetrics` / `PaneScrollBound`）**：
  - 在 `AppState` 上新增 `pub scroll_metrics: ViewportScrollMetrics`（包含 `console_output`、`log_diff`、`log_details`、`stash_diff`、`stage_diff` 5 个 `Option<PaneScrollBound>`）。
  - 在 `RenderContext`（`crates/jbgit-tui/src/views/mod.rs`）中携带可变的 `scroll_metrics`（或由 `render_app` 局部取出并在渲染结束后写回 `state`）。
  - 在以下渲染函数中写入真实 `(viewport_h, max_scroll)`：
    - `crates/jbgit-tui/src/views/console.rs` -> `render_command_output`（写入 `console_output`）
    - `crates/jbgit-tui/src/views/log_details.rs` -> 区分 `Log` 与 `Stash` 的 Diff 渲染写入 `log_diff` / `stash_diff`，详情渲染写入 `log_details`
    - `crates/jbgit-tui/src/views/commit_diff.rs` -> 写入 `stage_diff`
  - 在 `views/mod.rs` 的 `render_app` 收尾阶段，依据最新 `scroll_metrics` 自动回写钳制对应的 `state.*_scroll` 字段（保证终端缩放、分割线拖拽、内容变短时立刻回弹至合法范围）。
- [ ] **改造全部 5 处滚动 Action 派发逻辑**：
  - `Action::ScrollConsoleOutput(delta)`（`app.rs`）：基于当前选中 `GitConsoleEntry` 的动态总行数（`output_lines.len().max(1) + 2`）与 `scroll_metrics.console_output` 计算实时 `max_scroll`，调用 `apply_scroll_delta`。
  - `Action::ScrollDiff(delta)`（`app.rs`）：使用 `scroll_metrics.log_diff` 上界与 `apply_scroll_delta`。
  - `Action::ScrollDetails(delta)`（`app.rs`）：使用 `scroll_metrics.log_details` 上界与 `apply_scroll_delta`。
  - `Action::StashDiffScroll(delta)`（`app_git.rs`）：使用 `scroll_metrics.stash_diff` 上界与 `apply_scroll_delta`。
  - `Action::StageDiffScroll(delta)`（`app_stage.rs`）：优先使用 `scroll_metrics.stage_diff` 上界（未渲染回退 `len.saturating_sub(1)`），调用 `apply_scroll_delta`；同时 `StageDiffMoveCursor` 跟随滚动可复用真实 `viewport_h`（回退 18）。
- [ ] **校准既有测试并补充回归单元测试**：
  - 校准 `crates/jbgit-tui/src/main.rs` 约 L517 处的控制台滚轮测试：原先构造了 30 行输出在 40 行高的 `TestBackend` 中本就无需滚动（靠 Overscroll Bug 才通过 `console_output_scroll > 0`），将其扩充为真正溢出的行数（如 `0..60`），并额外断言未溢出时滚轮不会把 `console_output_scroll` 推高。
  - 新增纯单元测试验证 `apply_scroll_delta`：覆盖 `isize::MIN`、`isize::MAX`、连续多次 `isize::MAX / 2`、以及 `current > max_scroll` 后执行 `-1` 立即变成 `max_scroll - 1`。
  - 新增集成/渲染测试验证：
    1. `[4 Console]` 向下连续滚动 100 次后 `console_output_scroll == max_scroll`，随后向上滚 1 次（`delta = -3`）立刻变为 `max_scroll.saturating_sub(3)`；
    2. `LogDetailsDiff` 连续触发两次 `GotoBottom`（`isize::MAX / 2`）不溢出，紧接着向上滚 1 次立刻移动；
    3. 窗口高度变大（`Resize`）后重新渲染，已有 `console_output_scroll` 自动收缩到新的 `max_scroll`。

---

## 3. 验收标准 (Acceptance Criteria)

- [x] `cargo test --workspace` 与 `cargo clippy --workspace --all-targets` 全部通过。
- [x] 在 `[4 Console]` 对包含 100+ 行的 `git pull` 输出持续向下猛滑到底后，第一次向上滑动即刻流畅回滚，无任何卡死/死区。
- [x] 在 Log Details / Log Diff / Stash Diff / Stage Diff 任意视图按 `G` 或持续向下滚轮后，第一次向上滑动即刻生效且无整数溢出。
- [x] 当内容不足一屏时，`console_output_scroll` 等滚动偏移量恒为 `0`。

---

## Comments

- 2026-11-19 (agent): 根据用户反馈 `git pull` 后在控制台向下滑动停在 `create mode` 卡死问题排查并创建本 Issue，同步覆盖其余 4 个同类无上界滚动视口。
- 2026-11-19 (agent): 已实现 `apply_scroll_delta`、`ViewportScrollMetrics` / `PaneScrollBound` 收集与 `render_app` 收尾钳制，完成全部 5 个视口派发改造与回归测试；`cargo test --workspace` 与 `cargo clippy --workspace --all-targets` 通过。
