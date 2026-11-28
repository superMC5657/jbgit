# 05: `GraphRenderer::set_capabilities` 去无条件作废与 `Resize` 合并

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M13 — 大仓库、渲染热路径与引擎并发性能二期治理
Priority: P1

- **关联文件**：
  - `crates/jbgit-tui/src/graph/renderer.rs`（`GraphRenderer::set_capabilities`、`invalidate_emission`）
  - `crates/jbgit-tui/src/graph/vector/mod.rs`（`PixelVectorGraphBackend::invalidate_emission`）
  - `crates/jbgit-tui/src/session.rs`（`collapse_ready_resizes`、`TuiSession::ingest_into_state`、`TuiSession::step_turn`）
  - `crates/jbgit-tui/src/main.rs`（`carried_terminal`）

---

## 1. 根因定位

1. **拖拽缩放窗口时连续 `Resize` 未合并**：事件循环遇到 `Event::Resize` 立即打断批合并，拖动窗口边框时每个中间尺寸都触发 `on_resize` 与重绘。
2. **`GraphRenderer::set_capabilities` 无条件作废全部条带缓存**：即使新旧 `GraphicsCapabilities` 完全相等也会执行 `self.invalidate()`。

---

## 2. 任务清单

- [x] **合并 `Event::Resize` 并修复 `GraphRenderer::set_capabilities` 无条件 `invalidate()`**：
  - `crates/jbgit-tui/src/graph/renderer.rs`：仅当 `caps != self.caps` 时才更新并执行 `self.invalidate()`；
  - `crates/jbgit-tui/src/session.rs` & `main.rs`：新增 `collapse_ready_resizes` 合并连续已就绪的 `Event::Resize` 为最后一个，并暂存其后的非 `Resize` 事件到新尺寸重绘后处理；`TuiSession::step_turn` 同步折叠相邻 `Resize`；
  - 新增 `invalidate_emission()` 在 `Resize` 时仅清除 `emitted_key`（不丢弃已光栅化条带与 Kitty `U=1` 占位），防止 `A -> B -> A` 往返缩放合并后 Sixel/iTerm2 漏发重绘。

---

## 3. 验收标准与回归测试

- `vector/tests.rs`：
  - `set_capabilities_only_invalidates_when_caps_change`：相同 `GraphicsCapabilities` 不作废缓存，字号变化才作废重发；
  - `invalidate_emission_resends_sixel_and_iterm2_without_rerasterizing_and_keeps_kitty_cached`：验证 `invalidate_emission` 后 Sixel/iTerm2 重发且不重新光栅化条带，Kitty 保持缓存。
- `session_tests.rs`：`tui_session_collapses_consecutive_resizes_into_one_on_resize` 与 `collapse_ready_resizes_keeps_last_size_and_carries_the_next_event`。

## Comments

- **2026-11-28（Commit `042aa3b`）**：完成 `set_capabilities` 相等早退与 `collapse_ready_resizes` 合并。
- **2026-11-28（Code Review 修复）**：新增 `PixelVectorGraphBackend::invalidate_emission` / `GraphRenderer::invalidate_emission` 并在 `TuiSession::ingest_into_state` 的 `is_resize` 分支调用，修复往返 `Resize`（`A -> B -> A`）下 Sixel/iTerm2 `emitted_key` 误命中问题并补单测。
