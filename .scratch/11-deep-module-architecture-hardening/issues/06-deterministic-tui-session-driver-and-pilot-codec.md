# 06: 确定性 `TuiSession` 回合驱动、接口级测试替换与 `jbgit-pilot` 共享键位编解码

Status: resolved
Spec: [../spec.md](../spec.md)
Blocked by: 04, 05
Milestone: M8 — 深模块架构治理与测试面收敛 (P2)

- **关联文件**：
  - `Cargo.toml`
  - `crates/jbgit-tui/Cargo.toml`
  - `crates/jbgit-tui/src/{lib,main,external_editor}.rs`
  - `crates/jbgit-tui/src/keymap/mod.rs`
  - `crates/jbgit-tui/src/tests/{mod,runtime_and_editor_tests,footer_and_keymap_tests,file_history_and_changes_tests,context_menu_tests,branches_and_dialogs_tests}.rs`
  - `crates/jbgit-tui/src/log_tests.rs`
  - `tools/jbgit-pilot/Cargo.toml`
  - `tools/jbgit-pilot/src/{main,keys,server,screen}.rs`

---

## 1. 背景与目标

当前系统的顶层运行时编排与端到端测试工具链存在明显的接缝错位，导致「测试穿过的不是真实运行接口」：

1. **`main.rs` 独占回合编排，迫使 6 个测试模块各自手写残缺事件循环**：
   - `crates/jbgit-tui` 仅有二进制目标（`main.rs`），把事件排空批处理（`drain_coalesced_events`）、副作用去重合并（`coalesce_effects`）、副作用执行（`execute_effects`）、外部编辑器挂起恢复以及防抖时钟推进锁在 `main.rs:48-458` 内部。
   - 结果，`tests/mod.rs:193-228`、`log_tests.rs:116-135`、`file_history_and_changes_tests.rs:24-27` 等 6 个测试文件各自拼装了简化的 `route_terminal_event -> dispatch` 小循环，跳过了 `coalesce_effects` 与 `execute_effects`；为了验证单条流唤醒逻辑，`runtime_and_editor_tests.rs:1897-2017` 不得不手写 120 行模拟流桩代码；大量测试直接往内部缓存或私有状态塞值（如手动 `cache.insert`、手动改时间戳字段），一旦内部数据结构重构即成批脆弱断裂。
2. **`jbgit-pilot` 游离于 Workspace 之外并维护重复键位解析器**：
   - `tools/jbgit-pilot` 未纳入根 `Cargo.toml` `[workspace]`，`cargo test --workspace` 与 `cargo clippy --workspace` 不会检查 `jbgit-pilot` 的编译或测试回归。
   - `tools/jbgit-pilot/src/keys.rs` 与 `crates/jbgit-tui/src/keymap/mod.rs:386-495` 维护了两套相互独立的 `<C-...>` / `<A-...>` / 功能键字符串解析逻辑，容易产生语法漂移。
   - `tools/jbgit-pilot/src/server.rs:330` 在每次响应 `Clipboard` 命令时从头全量重读 `pty_raw.log` 扫描 OSC-52 序列，长会话下存在不必要的重复 I/O。

本 Issue 提取深模块 **`TuiSession`** 作为生产与测试唯一共享的回合驱动接口，遵循 **Replace, Don't Layer** 清理脆弱测试，并打通 `jbgit-pilot` 的 Workspace 集成与键位编解码共享。

---

## 2. 核心任务清单

- [x] **提取库级深模块 `TuiSession` 并将 `main.rs` 收敛为薄启动壳（`crates/jbgit-tui/Cargo.toml`, `src/lib.rs`, `src/session.rs`, `src/main.rs`）**：
  - 为 `jbgit-tui` 配置 `lib.rs` + 薄 `main.rs`（保留单一 `jbgit` 二进制产物名称不变）。
  - 构建深模块 `TuiSession`：完整封装单回合确定性驱动接口（接收批量终端/引擎/异步高亮事件与当前 `Instant` → 纯输入路由 → 工作台状态演进 → `coalesce_effects` → 通过注入的引擎/外部编辑器/剪贴板适配器执行副作用 → 计算 `needs_redraw` 与 `next_deadline()`，并提供 `render(&mut self, frame: &mut Frame)`）。
  - 在宿主环境接缝处明确两个适配器：
    - **生产适配器**：对接真实 Crossterm 终端流、`EngineSupervisor`、系统剪贴板与外部 `$EDITOR` 进程；
    - **确定性测试适配器**：对接虚拟单调时钟 `advance_to(Instant)`、`TestBackend` 离屏终端帧缓冲、Issue 02 的本地引擎替身及内存剪贴板/假编辑器，零真实磁盘/子进程副作用即可跑通完整回合。
  - 将 `main.rs` 精简为 CLI 参数解析（`--help` / `--init-config` / `--print-default-config`）、终端原始模式守卫与 `tokio::select!` I/O 泵（约 80–120 行）。
- [x] **遵循 Replace, Don't Layer 重整 TUI 测试套件（`crates/jbgit-tui/src/tests/*`, `log_tests.rs`）**：
  - 删除 6 个测试模块中重复手写的残缺 `route -> dispatch` 辅助循环，统一迁移至 `TuiSession` 接口驱动（通过按键序列、`advance_to(now + debounce)`、本地引擎事件投递与渲染帧缓冲文本/样式断言验证可观测行为）。
  - 清理直接篡改工作台内部缓存与私有字段的旧白盒测试，将其替换为穿过 `TuiSession` + 本地引擎替身的黑盒行为测试，使测试在未来内部重构时天然保持稳健；同时将根目录遗留的 `log_tests.rs` 统一归入 `src/tests/` 模块体系。
- [x] **将 `tools/jbgit-pilot` 纳入 Workspace 并共享键位编解码与增量剪贴板状态（`Cargo.toml`, `jbgit-core` / `jbgit-tui`, `tools/jbgit-pilot/*`）**：
  - 将 `tools/jbgit-pilot` 加入根 `Cargo.toml` 的 `members`（并使用 `[workspace.dependencies]` 对齐公共依赖版本），使其自动纳入常规 `cargo test --workspace` 与 `cargo clippy --workspace` 门禁。
  - 提取单一共享的按键标记解析模块（统一 `<C-...>`、`<A-...>`、`<S-...>`、`<CR>`、`<Esc>`、`<Tab>`、`<F1..F12>`、空格与组合键规范），同时服务 `jbgit-tui` 的 `keymap.toml` 解析与 `jbgit-pilot` 的 PTY 字节序列编码，删除重复解析实现并补充双向一致性单测。
  - 优化 `tools/jbgit-pilot/src/server.rs` 的 OSC-52 剪贴板捕获：在会话状态中随 PTY 输出流增量解析并缓存最新剪贴板载荷（或记录已扫描文件偏移量），消除每次 `clipboard` 查询重读整份 `pty_raw.log`。

---

## 3. 验收标准 (Acceptance Criteria)

- [x] `cargo test --workspace`（包含 `jbgit-core`、`jbgit-engine`、`jbgit-tui` 与 `jbgit-pilot`）及 `cargo clippy --workspace --all-targets -- -D warnings` 全部零警告通过。
- [x] `crates/jbgit-tui/src/main.rs` 仅保留 CLI 入口、终端生命周期守卫与事件泵胶水逻辑，不再独占 `coalesce_effects` 或其它核心回合编排规则。
- [x] TUI 集成与行为测试全部通过 `TuiSession` 公开接口驱动（真实覆盖 64 条事件批处理合并、`coalesce_effects`、30ms/120ms/300ms 防抖时钟推进与离屏渲染输出），不再存在散落手写的简化 `dispatch` 循环或直接写内部 LRU 缓存字段的测试。
- [x] `keymap.toml` 支持的任意合法按键表示均可被 `jbgit-pilot` 共享解析器无歧义识别；`jbgit-pilot` 沙箱回归流程（启动会话、发送组合键、读取屏幕与 OSC-52 剪贴板）全部正常工作。

---

## Comments

- 2026-11-22 (agent): 基于架构审查 Candidate 06 创建本 P2 收官工单，打通「接口即测试面」的确定性 `TuiSession` 驱动，并将 `jbgit-pilot` 正式纳入 Workspace 统一治理。
- 2026-11-22 (agent): 已完成重构、测试面收敛与实机 ConPTY 沙箱冒烟验证：
  - 为 `jbgit-tui` 新增 `src/lib.rs` 与确定性单回合深模块 `src/session.rs`（`TuiSession`、`SessionHost`、`ProdHost`、`TestHost`、`coalesce_effects`、`MAX_COALESCE_EVENTS` 及虚拟单调时钟 `AppState::now()`），将 `src/main.rs` 从 464 行收敛至薄 CLI/终端守卫/事件泵壳；
  - 将 `log_tests.rs` 归入 `src/tests/log_tests.rs`，在 `src/tests/mod.rs` 提供统一基于 `TuiSession` + `RecordingEngineBackend` + `TestHost` 的 `TestSession` 驱动与公共测试入口，清除各测试文件手写 `route_terminal_event`/`route_key`/`route_mouse` 循环及直接写 `diff_caches` LRU / 篡改 `menu_hover.due` 的白盒测试，并新增 `src/tests/session_tests.rs` 验证 64 事件批处理、`coalesce_effects`、30ms/120ms/300ms 虚拟时钟防抖与编辑器/剪贴板宿主机隔离；
  - 在 `crates/jbgit-core/src/key_token.rs` 提取唯一共享按键标记解析器，同时服务 `jbgit-tui::keymap::KeyStroke::parse` 与 `jbgit-pilot::keys::encode_key`；将 `tools/jbgit-pilot` 纳入根 Workspace `Cargo.toml`（删除孤立 `tools/jbgit-pilot/Cargo.lock`）；在 `tools/jbgit-pilot/src/{screen,server}.rs` 实现流式增量 `Osc52ClipboardScanner`（零全量重读 `pty_raw.log`），并通过 `jbgit-pilot` ConPTY 沙箱冒烟验证（屏幕渲染、`<F6>`/`<Esc>` 组合键、OSC-52 剪贴板捕获均正常）。
