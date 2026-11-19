# 01: 重构控制台时序输出流模型并实现 Git 语义分级分类器

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M6+ — 控制台体验与输出语义修正

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/commands/GitImplBase.java`（`ERROR_INDICATORS`、`looksLikeError`）
  - `crates/jbgit-core/src/console.rs`
  - `crates/jbgit-engine/src/cli.rs`
  - `crates/jbgit-tui/src/app.rs`

---

## 1. 背景与目标

当前 [`GitConsoleEntry`](../../../crates/jbgit-core/src/console.rs) 将标准输出和标准错误拆分为 `stdout_lines: Vec<String>` 与 `stderr_lines: Vec<String>` 两个独立数组，丢失了 `stdout`/`stderr` 的真实到达先后顺序；同时领域层缺少对 Git 输出行语义级别的判定，导致 UI 层只能把所有 `stderr` 行粗暴当成错误。

本 Issue 目标是在 `jbgit-core` 中建立按时序记录的 `ConsoleLine` 流模型与纯函数分类器 `classify_console_line`，并完成 `jbgit-engine` 与 `jbgit-tui` 状态层的无损接入（保持 `GitCliOutput` 内部机读结构不变）。

---

## 2. 核心任务清单

- [ ] **领域模型重构（`crates/jbgit-core/src/console.rs` & `lib.rs`）**：
  - 定义并从 `jbgit-core` 根导出：
    - `ConsoleStream { Stdout, Stderr }`
    - `ConsoleLine { pub stream: ConsoleStream, pub text: String }`
    - `ConsoleLineSeverity { Normal, Info, Warning, Error }`
  - 将 `GitConsoleEntry` 的 `stdout_lines` / `stderr_lines` 替换为 `pub output_lines: Vec<ConsoleLine>`。
  - 为 `GitConsoleEntry` 提供 `push_line(&mut self, stream: ConsoleStream, text: String)` 与 `has_output(&self) -> bool` 辅助方法。
- [ ] **实现 Git 语义分级分类器（`crates/jbgit-core/src/console.rs`）**：
  - 实现纯函数 `pub fn classify_console_line(line: &ConsoleLine, exit_code: Option<i32>) -> ConsoleLineSeverity`，严格遵循 Spec 07 §3.2 规则：
    - `Stdout`：`conflict (` / `fatal:` / `error:` -> `Error`；`warning:` / `hint:` -> `Warning`；其余 -> `Normal`。
    - `Stderr`：命中 `ERROR_INDICATORS` 错误前缀 -> `Error`；命中 `warning:` / `hint:` -> `Warning`；其余行在 `exit_code` 为非零 `Some(code)` 时兜底为 `Error`，在 `None`（运行中）或 `Some(0)`（成功）时归类为 `Info`。
  - 所有前缀判定基于 `line.text.trim_start()` 并忽略 ASCII 大小写。
- [ ] **接入 Engine 与 AppState**：
  - 更新 `crates/jbgit-engine/src/cli.rs` 中 `CliStreamEvent::Started(GitConsoleEntry { ... })` 初始化逻辑，使用 `output_lines: Vec::new()`。
  - **保留** `GitCliOutput.stdout_lines` 与 `GitCliOutput.stderr_lines` 不变，确保 `detect_index_lock_conflict`、`ops.rs` 与 `supervisor.rs` 无需改动机读解析逻辑。
  - 更新 `crates/jbgit-tui/src/app.rs` 中 `EngineEvent::ConsoleOutput` 处理分支，将 `is_stderr` 映射为 `ConsoleStream::Stderr` / `ConsoleStream::Stdout` 并调用 `entry.push_line(stream, line)`。
  - 更新 `crates/jbgit-tui/src/main.rs` 中依赖 `GitConsoleEntry` 的测试构造代码。
- [ ] **补齐单元测试（`crates/jbgit-core/src/console.rs`）**：
  - 验证 `git pull` 成功场景：`stderr` 中的 `From https://github.com/sxyazi/yazi`、`   40a91421..d5e4beb6  main       -> origin/main` 在 `exit_code = Some(0)` 与 `None` 下均判定为 `Info`。
  - 验证 `git checkout` / `git push` 成功时的常规 `stderr` 提示（`Switched to branch 'main'`、`To origin`）判定为 `Info`。
  - 验证 `warning:`、`hint:`（含大小写变体与前导空格）判定为 `Warning`。
  - 验证 `fatal:`、`error:`、`remote: error:`、`cannot `、`Could not `、`CONFLICT (content):`（包括在 `stdout` 上输出冲突行）均判定为 `Error`。
  - 验证非零退出码（如 `exit_code = Some(128)`）下未命中特定前缀的 `stderr` 行兜底判定为 `Error`，而 `stdout` 普通行仍为 `Normal`。
  - 验证 `push_line` 交替写入 `Stderr` 和 `Stdout` 后 `output_lines` 严格保留原始到达顺序。

---

## 3. 验收标准 (Acceptance Criteria)

- [ ] `cargo test -p jbgit-core` 与 `cargo test -p jbgit-engine` 全部通过。
- [ ] `GitConsoleEntry::output_lines` 能精确保留 `stderr`（如 `git fetch` 摘要）与 `stdout`（如 `git merge` 统计）的真实到达顺序。
- [ ] `classify_console_line` 覆盖全部核心边界场景且无 UI 依赖（纯 `jbgit-core` 逻辑）。
- [ ] `GitCliOutput` 相关已有集成测试（含 `ops.rs` 与 `index.lock` 检测）零回归。

---

## Comments

- 2026-11-19 (agent): 根据用户反馈 `git pull` 成功时控制台仍标红打印 `[stderr]` 问题创建 Spec 07 与本 Issue。
