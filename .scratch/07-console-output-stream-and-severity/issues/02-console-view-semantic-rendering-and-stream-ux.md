# 02: 重构控制台详情视图为时序语义渲染并移除误导性 [stderr] 标红

Status: ready-for-agent
Blocked by: 01
Spec: [../spec.md](../spec.md)
Milestone: M6+ — 控制台体验与输出语义修正

- **关联文件**：
  - `crates/jbgit-tui/src/views/console.rs`
  - `crates/jbgit-tui/src/theme/mod.rs`
  - `crates/jbgit-tui/src/i18n/locales/zh-CN.toml`
  - `crates/jbgit-tui/src/i18n/locales/en-US.toml`

---

## 1. 背景与目标

在 Issue 01 完成 `GitConsoleEntry::output_lines` 时序化与 `classify_console_line` 语义分类器后，本 Issue 负责改造 `[4 Console]` 右侧详情面板的渲染逻辑：按真实输出顺序展示日志行，去除对正常 Git 进度/摘要的 `[stderr] ` 前缀与错误红字，并以语义配色清晰区分正常信息、警告与真实错误。

---

## 2. 核心任务清单

- [ ] **重构 `render_command_output`（`crates/jbgit-tui/src/views/console.rs`）**：
  - 将空态判断从 `entry.stdout_lines.is_empty() && entry.stderr_lines.is_empty()` 切换为 `!entry.has_output()`（或 `entry.output_lines.is_empty()`）。
  - 按 `entry.output_lines` 原始顺序逐行渲染，不再分开循环 `stdout` 与 `stderr`。
  - 移除硬编码的 `[stderr] ` 文本前缀，统一使用 `format!(" {}", line.text)`。
  - 引入辅助函数（例如 `severity_style(severity: ConsoleLineSeverity, theme: &ThemePalette) -> Style`）完成语义着色：
    - `ConsoleLineSeverity::Normal` -> `Style::default().fg(ctx.theme.fg_primary)`
    - `ConsoleLineSeverity::Info` -> `Style::default().fg(ctx.theme.fg_primary)`
    - `ConsoleLineSeverity::Warning` -> `Style::default().fg(ctx.theme.ref_detached.icon)`
    - `ConsoleLineSeverity::Error` -> `Style::default().fg(ctx.theme.status_conflicted)`
- [ ] **检查并审阅文案一致性（`zh-CN.toml` / `en-US.toml`）**：
  - 核对 `view.console.select_prompt` 等既有文案是否与新的控制台体验一致（保持现有 key 兼容，不破坏已有翻译）。
- [ ] **补齐 TUI 渲染与行为测试**：
  - 在 `crates/jbgit-tui` 中新增针对 `render_console_tab` / `render_command_output` 的 Ratatui `TestBackend` 单元测试：
    1. **成功 `git pull` 场景**：先推入 `Stderr("From https://github.com/sxyazi/yazi")` 与 `Stderr("   40a91421..d5e4beb6  main       -> origin/main")`，再推入 `Stdout(" create mode 100644 yazi-widgets/src/cmp/mod.rs")`，`exit_code = Some(0)`：
       - 断言渲染缓冲中 **不包含** `[stderr]`；
       - 断言 `From https://...` 出现在 `create mode 100644 ...` **之前**（行号更小）；
       - 断言这些行的前景色均为 `theme.fg_primary`（而非 `theme.status_conflicted`）。
    2. **警告与错误混合场景**：包含 `Stderr("warning: pulling without specifying...")`、`Stderr("fatal: Not possible to fast-forward, aborting.")`（`exit_code = Some(128)`）：
       - 断言 `warning:` 行前景色为 `theme.ref_detached.icon`；
       - 断言 `fatal:` 行前景色为 `theme.status_conflicted`。
    3. **长输出滚动兼容**：确认既有 `console_output_scroll` 滚动上限与裁剪行为在 `output_lines` 下工作正常。

---

## 3. 验收标准 (Acceptance Criteria)

- [ ] `cargo test --workspace` 与 `cargo clippy --workspace --all-targets` 全部通过且无新增告警。
- [ ] 在 `[4 Console]` 查看成功的 `git pull` / `git fetch` / `git push` 输出时：
  - 不再出现红色 `[stderr]` 前缀；
  - `From https://...` 与分支更新摘要按真实发生顺序显示在合并文件列表之前。
- [ ] 命令失败（非零退出码）或出现 `fatal:` / `error:` / `CONFLICT` / `warning:` 时，对应行分别以错误红 / 警告琥珀色清晰高亮。
- [ ] 在 `darcula`、`catppuccin-mocha`、`gruvbox-dark` 三套内置主题下均无配色异常或对比度问题。

---

## Comments

- 2026-11-19 (agent): 拆分为独立 TUI 视图与测试 Issue，依赖 Issue 01 提供的 `ConsoleLine` 与 `classify_console_line`。
