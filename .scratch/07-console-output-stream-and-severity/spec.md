# Spec 07: Git 控制台时序输出流与语义分级渲染规约

- **目录**：`.scratch/07-console-output-stream-and-severity/`
- **下属 Issues**：
  - [`01-ordered-console-stream-and-severity-classifier.md`](./issues/01-ordered-console-stream-and-severity-classifier.md)
  - [`02-console-view-semantic-rendering-and-stream-ux.md`](./issues/02-console-view-semantic-rendering-and-stream-ux.md)
- **参考源码**：
  - `reference/plugins/git4idea/backend/src/commands/GitImplBase.java`（`ERROR_INDICATORS`、`looksLikeError`、`git.allow.stderr.to.stdout.mixing`）
  - `reference/plugins/git4idea/backend/src/commands/GitHandlerUtil.java`（`isErrorLine`）

---

## 1. 背景与问题诊断

在当前 `[4 Console]` 控制台视图中，执行 `git pull`、`git fetch`、`git push`、`git checkout` 等常规 Git 命令时，即便命令成功结束（`exit_code == 0`），控制台仍会展示令人困惑的红色 `[stderr]` 行，且行序与真实执行过程相反。

具体问题由两层设计叠加造成：

1. **Git 的流约定与渲染语义错配**：
   - Git 将面向人类的进度、远端摘要与分支切换状态（例如 `From https://github.com/sxyazi/yazi`、`   40a91421..d5e4beb6  main -> origin/main`、`Switched to branch 'main'`）输出至 `stderr`，而将合并统计（例如 `create mode 100644 ...`）输出至 `stdout`。
   - 当前 [`crates/jbgit-tui/src/views/console.rs`](../../crates/jbgit-tui/src/views/console.rs) 对所有 `stderr_lines` 无条件添加 `[stderr] ` 前缀并以错误红色（`ctx.theme.status_conflicted`）渲染，完全未参考 `exit_code` 或行级错误关键字，导致成功操作被误判为报错。
2. **输出时序倒置**：
   - [`GitConsoleEntry`](../../crates/jbgit-core/src/console.rs) 将 `stdout_lines` 与 `stderr_lines` 拆分在两个独立的 `Vec<String>` 中。
   - 渲染层先遍历全部 `stdout_lines`，再遍历全部 `stderr_lines`，导致 `git pull` 中更早产生的 `fetch` 远端摘要反而排在 `merge` 文件变更统计之后。

---

## 2. 设计目标与职责边界

### 2.1 核心目标
1. **保序输出流（Chronological Output Stream）**：控制台条目按真实到达顺序记录每一行输出及其来源流（`Stdout` / `Stderr`），彻底消除 `stdout` / `stderr` 分段导致的时序错位。
2. **纯领域语义分级器（Pure Severity Classifier）**：在 `jbgit-core` 中实现对齐 JetBrains `git4idea`（`GitImplBase.ERROR_INDICATORS`）的行级语义分类器，结合来源流、行前缀特征及命令退出码，将每行输出归类为 `Normal`、`Info`、`Warning`、`Error`。
3. **无干扰控制台视觉（Clean Console UX）**：
   - 移除对普通/信息级 `stderr` 行的 `[stderr] ` 硬编码前缀与错误标红；
   - 按语义分级映射到 `ThemePalette` 已有颜色 token（兼容 `darcula`、`catppuccin-mocha`、`gruvbox-dark` 与用户覆写），无需破坏 `theme.toml` 配置结构；
   - 保留 `jbgit-engine` 内部 `GitCliOutput.stdout_lines` / `stderr_lines` 的分流结构，不影响 `ops.rs` 错误分类、冲突解析与 `index.lock` 检测。

### 2.2 非目标（Out of Scope）
- 不修改 `GitCliOutput` 对外提供的 `stdout_lines` / `stderr_lines` 字段及其机器解析逻辑。
- 不在本特性中新增 ANSI 转义码解析或交互式伪终端（PTY）仿真。
- 不修改 `theme.toml` 的 schema 字段定义。

---

## 3. 领域模型与分类规则

### 3.1 数据结构（`crates/jbgit-core/src/console.rs`）

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleLine {
    pub stream: ConsoleStream,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleLineSeverity {
    /// 常规标准输出（如 merge diffstat、git status、文件列表）
    Normal,
    /// 来自 stderr 的正常进度、远端摘要或状态提示（如 From https://...、Switched to branch ...）
    Info,
    /// 警告或提示信息（warning: / hint:）
    Warning,
    /// 明确的错误行，或命令非零退出时的 stderr 诊断信息
    Error,
}
```

`GitConsoleEntry` 将原先分离的 `stdout_lines` / `stderr_lines` 替换为统一的按时间顺序追加的 `pub output_lines: Vec<ConsoleLine>`，并提供便捷方法：
- `push_line(&mut self, stream: ConsoleStream, text: String)`
- `has_output(&self) -> bool`

### 3.2 语义分类规则（`classify_console_line`）

函数签名：
```rust
pub fn classify_console_line(
    line: &ConsoleLine,
    exit_code: Option<i32>,
) -> ConsoleLineSeverity
```

判定优先级如下（前缀匹配前先对 `line.text.trim_start()` 做大小写不敏感比较）：

1. **来源为 `ConsoleStream::Stdout`**：
   - 若以 `conflict (`、`fatal:`、`error:` 开头 -> `ConsoleLineSeverity::Error`（覆盖 `git merge` 将 `CONFLICT (content): ...` 写到 `stdout` 的场景）。
   - 若以 `warning:`、`hint:` 开头 -> `ConsoleLineSeverity::Warning`。
   - 其余 -> `ConsoleLineSeverity::Normal`。
2. **来源为 `ConsoleStream::Stderr`**：
   - 若命中以下错误指示前缀（参考 `GitImplBase.ERROR_INDICATORS`）-> `ConsoleLineSeverity::Error`：
     - `fatal:`
     - `error:`
     - `remote: error`
     - `cannot `
     - `could not `
     - `unable to `
     - `refusing to pull`
     - `cannot rebase:`
     - `conflict`
     - `interactive rebase already started`
     - `the file will have its original`
     - `runnerw:`
   - 若以 `warning:` 或 `hint:` 开头 -> `ConsoleLineSeverity::Warning`。
   - 其余行：
     - 当命令已经失败（`matches!(exit_code, Some(code) if code != 0)`）-> `ConsoleLineSeverity::Error`（兜底展示未知格式的错误堆栈/远端拒绝信息，避免漏报）。
     - 当命令运行中（`None`）或已成功（`Some(0)`）-> `ConsoleLineSeverity::Info`（不按错误标红）。

---

## 4. TUI 视图渲染规范（`crates/jbgit-tui/src/views/console.rs`）

1. **顺序渲染**：按 `entry.output_lines` 的原始到达顺序逐行渲染，不再先画 `stdout` 后画 `stderr`。
2. **去除误导性前缀**：统一以单空格缩进 `format!(" {}", line.text)` 渲染，不再为 `stderr` 强制拼接 `[stderr] `。
3. **语义配色映射**：
   - `ConsoleLineSeverity::Normal` -> `ctx.theme.fg_primary`
   - `ConsoleLineSeverity::Info` -> `ctx.theme.fg_primary`（与 JetBrains Git Console 保持一致，正常远端同步摘要具备清晰可读性；不与错误色混淆）
   - `ConsoleLineSeverity::Warning` -> `ctx.theme.ref_detached.icon`（与 `OngoingRepositoryStateBanner` 统一的琥珀/告警色）
   - `ConsoleLineSeverity::Error` -> `ctx.theme.status_conflicted`（错误红色）
4. **顶部状态徽标与空态**：
   - 顶部 `$ <command>` 后的 `RUNNING` / `EXIT 0` / `EXIT <code>` 状态徽标保持不变。
   - 空输出判断切换为 `entry.output_lines.is_empty()`。
   - 保持现有滚动条上限计算（`max_scroll`）与鼠标滚轮交互逻辑不变。

---

## 5. 任务拆分与依赖顺序

1. **Issue 01**（`01-ordered-console-stream-and-severity-classifier.md`）：在 `jbgit-core` 中重构 `GitConsoleEntry` 时序行模型并实现 `classify_console_line` 纯分类器，同步接入 `jbgit-engine` 与 `jbgit-tui` 的事件状态更新及单元测试。
2. **Issue 02**（`02-console-view-semantic-rendering-and-stream-ux.md`，Blocked by `01`）：重构 `crates/jbgit-tui/src/views/console.rs` 渲染逻辑与语义样式映射，补齐 TUI 渲染测试并验证各类 Git 命令输出表现。
