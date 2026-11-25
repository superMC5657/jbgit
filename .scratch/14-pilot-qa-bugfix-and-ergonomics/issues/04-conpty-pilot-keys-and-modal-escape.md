# 04: `jbgit-pilot` Windows ConPTY 按键编码修复与模态 `Esc` 守卫对齐

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M11 — Pilot QA 缺陷修复与体验加固
Priority: P2
QA-Refs: `P2-07`, `P2-11`, `P2-12`

- **关联文件**：
  - `tools/jbgit-pilot/src/keys.rs`（`KeyTokenKind::Esc` `:47, 143`、`KeyTokenKind::Enter` `:134-142`、C0 控制字符分支 `:144-176`、`encode_ctrl_modified_char` `:181-236`、单测 `:302`）
  - `tools/jbgit-pilot/src/server.rs`（`PilotAction::Keys` 写入循环 `:316-320`、`PilotAction::Stop` `:441`）
  - `crates/jbgit-tui/src/input/keyboard.rs`（`route_reflog_key` `:1380-1381`、`route_push_dialog_key` `:1286`）
  - `crates/jbgit-tui/src/tree_browser.rs`（`route_tree_browser_key` `:673-705`）
  - `crates/jbgit-tui/src/i18n/locales/{zh-CN,en-US}.toml`（`dialog.push_full.hint` `:601`）
  - `crates/jbgit-tui/src/keymap/mod.rs`（`KeyStroke::matches` `:820-873`）

---

## 1. 根因定位（区分 `jbgit-pilot` 与 `jbgit-tui`）

经双边排查，`P2-07`、`P2-11`、`P2-12` 的**主根因均位于 QA 驱动 `tools/jbgit-pilot/src/keys.rs` 与 `server.rs`**，`jbgit-tui` 本身的路由逻辑基本正确（仅存在 2 处 `Esc` 修饰键守卫不一致）：

1. **[`P2-07`] `Ctrl+~` / `Ctrl+\`` 无法打开分支浮窗**：
   - `jbgit-tui`：`default.toml:16-19` 已绑定 `<C-`>` / `<C-~>`，`KeyStroke::matches`（`keymap/mod.rs:847-852`）对非字母 Ctrl 字符正确忽略 `shift`。
   - **`jbgit-pilot` 根因（`keys.rs:149, 161`）**：将 ``'`'`` 和 `'~'` 放入了传统 C0 控制码分支并返回单个 `0x00`（NUL）字节。ConPTY 与 Unix `crossterm` 会将裸 `0x00` 解析成 `Ctrl+Space` 或 `Ctrl+2`，永远不会得到 ``'`'`` / `'~'`。讽刺的是，下方的 `encode_ctrl_modified_char`（`keys.rs:203`）已经正确映射了 ``'`' | '~' => 0xC0``（`VK_OEM_3`）的 Win32 Input Mode 序列，却被第 149 行截胡成了死代码。
2. **[`P2-11`] `Ctrl+Enter` 无法提交 Push 弹窗**：
   - `jbgit-tui`：`route_push_dialog_key`（`keyboard.rs:1286`）已经完整处理了 `KeyCode::Enter if ctrl || alt => vec![Action::PushConfirm]`。
   - **`jbgit-pilot` 根因（`keys.rs:134-142`）**：在所有平台（含 Windows）上无条件把 `Ctrl+Enter` 编码成 Kitty/CSI-u 序列 `\x1b[13;5u`。Windows ConPTY 的 `InputStateMachineEngine` 不识别 CSI `u` 并直接丢弃，而 `crossterm` Windows 后端期望的是 Win32 Input Mode（`VK_RETURN = 13`, `LEFT_CTRL_PRESSED = 8`）。
3. **[`P2-12`] `Esc` 无法关闭 `Reflog` / `TreeBrowserSearch` / `Annotate`**：
   - **主根因在 `jbgit-pilot`（`keys.rs:47, 143` + `server.rs:316-320`）**：
     - `keys.rs:143` 在 Windows 上发送裸 `0x1b`（`b"\x1b"`），进入 ConPTY `StateMachine::Escape` 歧义缓冲态容易被吞掉；
     - `server.rs:316-320` 在 `PilotAction::Keys` 中把多个按键连续写入后才 `flush()` 一次，导致类似 `key Esc 4` / `key Esc q` 被拼成 `\x1b4` / `\x1bq` 解析成 `Alt+4` / `Alt+q`；
     - 实证：`TreeBrowserSearch` 的 `KeyCode::Esc => CloseSearch`（`tree_browser.rs:673-677`）没有任何前置守卫却未能触发，直接证明 `jbgit-tui` 未收到 `Esc` 事件。
   - **次要不一致在 `jbgit-tui`**：
     - `Reflog`（`keyboard.rs:1381`）把 `KeyCode::Esc` 与 `KeyCode::Char('q')` 共用 `if !ctrl && !alt` 守卫；
     - 非搜索态 `TreeBrowser`（`tree_browser.rs:694-705`）在 `CONTROL | ALT` 早退分支丢弃了 `Esc`；
     - 相比之下 `FileHistory`、`Annotate`、`PushDialog`、`Conflict`、`Dialog` 均无条件接受 `KeyCode::Esc`。

---

## 2. 任务清单

- [ ] **修复 `tools/jbgit-pilot/src/keys.rs` Windows ConPTY 按键编码**：
  - `P2-07`（`keys.rs:149, 161`）：从传统 `0x00` C0 分支移除 ``'`'`` 与 `'~'`，使其走 `encode_ctrl_modified_char`（Windows 输出 `VK_OEM_3` Win32 Input Mode，Unix 输出 CSI `u` `\x1b[96;5u` / `\x1b[126;5u`）；
  - `P2-11`（`keys.rs:134-142`）：在 `#[cfg(windows)]` 下将带 `ctrl`（或 `shift`）的 `Enter` 编码为 Win32 Input Mode `\x1b[13;0;13;1;{ctrl_state};1_\x1b[13;0;13;0;{ctrl_state};1_`；在 `#[cfg(not(windows))]` 保留 CSI `u`；
  - `P2-12`（`keys.rs:47, 143`）：在 `#[cfg(windows)]` 下将 `KeyTokenKind::Esc` 编码为 Win32 Input Mode `\x1b[27;0;27;1;{ctrl_state};1_\x1b[27;0;27;0;{ctrl_state};1_`（`VK_ESCAPE = 27`），消除 ConPTY 裸 `0x1b` 解析歧义。
- [ ] **修复 `tools/jbgit-pilot/src/server.rs` 多键刷新与停止序列**：
  - `server.rs:316-320`：在 `PilotAction::Keys` 循环中逐键 `flush()`；在 `#[cfg(not(windows))]` 下若裸 `0x1b` 后还有后续按键，插入约 30ms 间隔防止被误拼为 `Alt+<next>`；
  - `server.rs:441`（`PilotAction::Stop`）：改用 `encode_key("Esc")` 代替裸字节 `b"\x1b"`。
- [ ] **`jbgit-tui` 防御性对齐与提示完善**：
  - `crates/jbgit-tui/src/input/keyboard.rs:1381`：将 `KeyCode::Esc => vec![Action::CloseReflog]` 拆出 `if !ctrl && !alt` 守卫；
  - `crates/jbgit-tui/src/tree_browser.rs:694-703`：在 `CONTROL | ALT` 分支前无条件响应 `KeyCode::Esc => vec![Action::PopModal]`；
  - `crates/jbgit-tui/src/i18n/locales/{zh-CN,en-US}.toml:601`：将 `dialog.push_full.hint` 的 `Ctrl+Enter` 更新为 `Ctrl/Alt+Enter`（与 `dialog.message.hint` 及 `keyboard.rs:1286` 保持一致，方便不支持 CSI `u` 的 Unix 终端用户）。

---

## 3. 测试与验收

- **`jbgit-pilot` 单元测试（`tools/jbgit-pilot/src/keys.rs`）**：
  - 新增 `encode_key("Ctrl+`")`、`encode_key("Ctrl+~")`、`encode_key("Ctrl+Enter")`、`encode_key("Esc")` 的平台分条件断言（`#[cfg(windows)]` 验证 Win32 Input Mode 序列，`#[cfg(not(windows))]` 验证 CSI `u` / `\x1b`）。
- **`jbgit-tui` 单元测试**：
  - `keymap/mod.rs`：断言 `<C-`>` / `<C-~>` 匹配带 `CONTROL` 及 `CONTROL | SHIFT` 的事件；
  - `tag_and_push_tests.rs`：断言 `Ctrl+Enter` 与 `Alt+Enter` 均触发 `Action::PushConfirm`；
  - 断言 `Esc`（含附带修饰键场景）稳定关闭 `Reflog`、`TreeBrowserSearch`、`TreeBrowser` 与 `Annotate`。
- **Pilot ConPTY 端到端复验**：
  - `full` 场景下执行 `key "Ctrl+~"`、`key 'Ctrl+`'` 均打开分支浮窗；Push 弹窗执行 `key Ctrl+Enter` 触发推送；`Reflog` / `TreeBrowserSearch` / `Annotate` 执行 `key Esc` 全部立即关闭。
