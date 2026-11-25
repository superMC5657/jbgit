# 04: 交互控件 Hover 视觉反馈一致性补齐（冲突按钮 / 筛选芯片 / 顶栏分支胶囊）

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M12 — 工作流正确性与 UI 交互加固
Priority: P3

- **关联文件**：
  - `crates/jbgit-tui/src/views/conflict_merge.rs`（底部按钮样式 `:755-776`）
  - `crates/jbgit-tui/src/views/log.rs`（`render_filter_bar` 筛选芯片与按钮样式 `:342-378`, `:633-755`）
  - `crates/jbgit-tui/src/views/header.rs`（顶栏分支胶囊 `BranchWidgetPill` `:168-196`）
  - `crates/jbgit-tui/src/hit_test.rs`（确认 `has_visual_hover()` 已包含上述 `HitTarget`）

---

## 1. 根因定位

`HitTarget::has_visual_hover()`（`crates/jbgit-tui/src/hit_test.rs:471-495`）对 `ConflictMergeButton`、`FilterChip`、`FilterPresentation`、`BranchWidgetPill` 均返回 `true`（鼠标悬停时已会触发重绘），但对应的 3 处渲染代码未读取 `ctx.hover_target` 切换悬停配色：

1. **Merge Conflict Resolver 底部操作按钮无 hover 高亮**：
   - `crates/jbgit-tui/src/views/conflict_merge.rs:762-774` 中，所有非主按钮固定使用 `fg_primary` + `bg_panel`（`#BCBEC4` / `#2B2D30`），未检查 `ctx.hover_target == Some(&HitTarget::ConflictMergeButton(kind))`，悬停 `[ 自动应用 (1) ]`、`[ 我方 ]` 等按钮时无颜色变化。
2. **Log 工具栏筛选芯片（`分支▾` 等）与 `⚙` 按钮无 hover 反馈**：
   - `crates/jbgit-tui/src/views/log.rs:633-744`：`active_chip_style`、`inactive_style`、`nm_inactive_style` 以及 `:348-353` 的 `⚙` 按钮均未检查 `ctx.hover_target`，而同工具栏相邻的 `◉ 差异` 与顶栏 `sandbox-repo ▾` 均会在悬停时切换到 `bg_hover`（`#2D2F34` / `fg_bright`）。
3. **顶栏分支胶囊（`main ↑3 ▾`）无 hover 反馈**：
   - `crates/jbgit-tui/src/views/header.rs:169-175`：`BranchWidgetPill` 固定使用 `pill_fg` / `pill_bg`，未在 `ctx.hover_target == Some(&HitTarget::BranchWidgetPill)` 时切换悬停态背景或前景样式。

---

## 2. 任务清单

- [ ] **为 Merge Conflict Resolver 底部按钮增加 hover 高亮**：
  - 在 `crates/jbgit-tui/src/views/conflict_merge.rs:761-775` 中判断 `let hovered = ctx.hover_target == Some(&HitTarget::ConflictMergeButton(kind));`；
  - 非 `primary` 按钮悬停时切换为 `fg: ctx.theme.fg_bright, bg: ctx.theme.bg_hover`（与 `render_toolbar` `:515-519` 一致）；`primary` 按钮悬停时增加 `Modifier::REVERSED` 或 `Modifier::UNDERLINED` / 提亮背景以区分态。
- [ ] **为 Log 筛选栏芯片与 `⚙` 按钮增加 hover 高亮**：
  - 在 `crates/jbgit-tui/src/views/log.rs:346-356`（`FilterPresentation`）及 `:360-378` 的 `put` 闭包中统一支持按 `target` 检查 `ctx.hover_target == target.as_ref()`：
    - 非激活态芯片（`分支▾`、`作者▾`、`日期▾`、`路径▾`、`[排除合并]`、`⚙`）悬停时使用 `fg: theme.fg_bright, bg: theme.bg_hover`；
    - 激活态芯片悬停时附加明显视觉反馈（如 `Modifier::UNDERLINED` 或 `fg_bright`）；`×` 清除钮悬停时切为 `status_deleted` / `bg_hover`。
- [ ] **为顶栏分支胶囊增加 hover 高亮**：
  - 在 `crates/jbgit-tui/src/views/header.rs:168-176` 中检查 `ctx.hover_target == Some(&HitTarget::BranchWidgetPill)`（或 `HeaderBehindBadge`），悬停时调整背景色/样式（如切换为 `ctx.theme.bg_hover` 或在原胶囊色基础上加 `Modifier::UNDERLINED` + `fg_bright`），与未悬停态形成可测量的 cell style 差异。

---

## 3. 测试与验收

- **`TestBackend` 渲染测试（修复前必挂）**：
  - `conflict_merge`: 打开 `ConflictResolver` 时分别对 `[ 自动应用 ]` / `[ 我方 ]` 按钮坐标发送 `MouseMove`，断言按钮单元格 `bg` 或 `fg` 发生变化（切换至 `theme.bg_hover` / `theme.fg_bright`）；
  - `log` & `header`: 在 `Log` 标签页分别对 `HitTarget::FilterChip(FilterField::Branch)`（`分支▾`）、`HitTarget::FilterPresentation`（`⚙`）与 `HitTarget::BranchWidgetPill`（`main ↑3 ▾`）坐标发送 `MouseMove`，断言对应渲染单元格样式发生可见变化。
- **Pilot 定向复验**：
  - `conflict` 场景使用 `hover-text` + `inspect-row` 验证底部按钮颜色变化；
  - `full` 场景使用 `hover-text "分支▾"` 与 `hover-text "main"`（带 `--row 0`）+ `inspect-row` 验证颜色变化。

## Comments
