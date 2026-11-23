# 08: `GIT_ASKPASS` / `SSH_ASKPASS` 凭据回调桥

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M9 — reference 缺口补齐
Priority: P1
Blocked by: 07

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/commands/{GitHttpAuthenticator.java,GitHttpAuthService.kt,GitHttpAuthServiceImpl.java}`
  - `reference/plugins/git4idea/backend/src/remote/GitHttpAuthDataProvider.java`
  - `reference/plugins/git4idea/rt/src/git4idea/`（外部辅助进程与 IDE 通信的方式）
- **关联文件**：
  - `crates/jbgit-engine/src/cli.rs`（约 `:55-56`、`:186-192` 统一设置 `GIT_TERMINAL_PROMPT=0`）
  - `crates/jbgit-tui/src/main.rs`（子命令分发）、`crates/jbgit-tui/src/dialogs.rs`（需新增掩码输入）

---

## 1. 背景与目标

所有 git 子进程强制 `GIT_TERMINAL_PROMPT=0`，因此当没有配置 credential helper 或 ssh-agent 时，HTTPS 推拉与带口令的 SSH key 会直接失败。IDEA 通过 askpass 辅助进程把提示转发到 IDE 对话框。本工单实现同等能力。

## 2. 已定设计（2026-11-23 维护者拍板：做 askpass 回调桥）

1. **IPC 通道**：仅当前用户可访问的本地通道 —— Windows 命名管道（`\\.\pipe\jbgit-askpass-<pid>-<random>`，设置仅当前用户 SID 的 DACL）/ Unix domain socket（放在 `$XDG_RUNTIME_DIR` 或 `0700` 临时目录下，socket 文件 `0600`）。**不使用 TCP 回环端口。** 会话结束或 `RepoSession::unbind`（Issue 09）时关闭并删除。
2. **鉴权**：每个 TUI 会话生成 32 字节随机令牌，经环境变量 `JBGIT_ASKPASS_TOKEN` 传给 git 子进程；askpass 子命令连接时出示，令牌不匹配立即断开。通道地址经 `JBGIT_ASKPASS_ADDR` 传递。
3. **控制台脱敏**：控制台回放的环境变量与命令行采用白名单，`JBGIT_ASKPASS_*` 永不显示；凭据内容只在内存中流转。
4. **持久化**：jbgit 自身不存储任何凭据。若用户配置了 credential helper，git 在认证成功后会自行调用 helper 的 `store`，jbgit 不干预。
5. **并发**：多个 git 进程同时请求时，TUI 按 FIFO 一次显示一个对话框；用户取消 → askpass 以退出码 1 结束。单次请求 120s 超时。
6. **测试替身**：提供 `AskpassResponder` trait，生产实现为 TUI 对话框，测试 / `jbgit-pilot` 沙箱使用返回固定值或固定取消的确定性适配器。

## 3. 任务清单

- [ ] `jbgit askpass <prompt>` 隐藏子命令（`main.rs` 分发，不出现在 `--help`）：读取 `JBGIT_ASKPASS_ADDR` / `JBGIT_ASKPASS_TOKEN`，连接、发送 `{token, prompt}`、把回应写到 stdout；超时、取消或环境缺失时返回退出码 1。
- [ ] TUI 侧 IPC 服务：随会话启动，只接受令牌匹配的连接，把请求放入 FIFO 并通过 `EngineEvent` / 专用通道驱动对话框。
- [ ] 引擎：仅对联网类 `GitOp`（fetch / pull / push / update project / 远端 Tag 操作 / `ls-remote`）设置 `GIT_ASKPASS`、`SSH_ASKPASS`、`SSH_ASKPASS_REQUIRE=force`、`JBGIT_ASKPASS_ADDR`、`JBGIT_ASKPASS_TOKEN`（askpass 路径为当前 `jbgit` 可执行文件的绝对路径）；本地只读命令不设置。保留 `GIT_TERMINAL_PROMPT=0`。
- [ ] `Dialog::Credential { prompt, masked: bool }`：提示含 `Password` / `passphrase` / `token` 时掩码显示；支持粘贴；发送后用 `zeroize` 清零缓冲区。
- [ ] `GitFailure::AuthenticationFailed` 分类（`Authentication failed`、`Permission denied (publickey)`、`could not read Username`），失败对话框给出配置 credential helper / ssh-agent 的指引。
- [ ] `docs/manual.md` 说明工作原理与"jbgit 不保存凭据"的承诺。

## 4. 验收

- 本地起一个要求 Basic Auth 的 HTTP git 服务（`git http-backend` + 简易代理；CI 不便时标记为 `#[ignore]` 手动测试），验证输入正确凭据可推送、取消时以 `AuthenticationFailed` 结束。
- 错误令牌的连接被拒绝；通道文件 / 管道权限只允许当前用户。
- 控制台、日志、草稿文件、`.pilot/` 记录中检索不到凭据明文或令牌。
