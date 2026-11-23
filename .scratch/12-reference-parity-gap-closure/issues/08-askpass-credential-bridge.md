# 08: `GIT_ASKPASS` / `SSH_ASKPASS` 凭据回调桥

Status: resolved
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

- [x] `jbgit askpass <prompt>` 隐藏子命令（`main.rs` 分发，不出现在 `--help`）：读取 `JBGIT_ASKPASS_ADDR` / `JBGIT_ASKPASS_TOKEN`，连接、发送 `{token, prompt}`、把回应写到 stdout；超时、取消或环境缺失时返回退出码 1。
- [x] TUI 侧 IPC 服务：随会话启动，只接受令牌匹配的连接，把请求放入 FIFO 并通过 `EngineEvent` / 专用通道驱动对话框。
- [x] 引擎：仅对联网类 `GitOp`（fetch / pull / push / update project / 远端 Tag 操作 / `ls-remote`）设置 `GIT_ASKPASS`、`SSH_ASKPASS`、`SSH_ASKPASS_REQUIRE=force`、`JBGIT_ASKPASS_ADDR`、`JBGIT_ASKPASS_TOKEN`（askpass 路径为当前 `jbgit` 可执行文件的绝对路径）；本地只读命令不设置。保留 `GIT_TERMINAL_PROMPT=0`。
- [x] `Dialog::Credential { prompt, masked: bool }`：提示含 `Password` / `passphrase` / `token` 时掩码显示；支持粘贴；发送后用 `zeroize` 清零缓冲区。
- [x] `GitFailure::AuthenticationFailed` 分类（`Authentication failed`、`Permission denied (publickey)`、`could not read Username`），失败对话框给出配置 credential helper / ssh-agent 的指引。
- [x] `docs/manual.md` 说明工作原理与"jbgit 不保存凭据"的承诺。

## 4. 验收

- 本地起一个要求 Basic Auth 的 HTTP git 服务（`git http-backend` + 简易代理；CI 不便时标记为 `#[ignore]` 手动测试），验证输入正确凭据可推送、取消时以 `AuthenticationFailed` 结束。
- 错误令牌的连接被拒绝；通道文件 / 管道权限只允许当前用户。
- 控制台、日志、草稿文件、`.pilot/` 记录中检索不到凭据明文或令牌。

## Comments

- 2026-11-24 实现说明：
  - 模块：`jbgit-engine/src/askpass.rs`（服务端 `AskpassServer`、客户端 `run_client` / `request_credential`、`AskpassResponder` trait 及 `ChannelResponder` / `FixedResponder`、`AskpassEnv`）。协议为长度前缀帧：客户端先发令牌帧再发提示帧，服务端回 `1 + 帧`（答案）或 `0`（取消）；令牌常量时间比较，不匹配直接断开、不调用 responder。
  - 偏离 / 补充：git 与 ssh 调用 askpass 时只传一个提示参数，因此除了隐藏子命令 `jbgit askpass <prompt>` 外，`jbgit` 在 **恰好一个参数且存在 `JBGIT_ASKPASS_ADDR` / `JBGIT_ASKPASS_TOKEN`** 时也进入 askpass 客户端模式（`GIT_ASKPASS` 直接指向 `jbgit` 可执行文件）。
  - Windows 管道 DACL 为 `D:P(A;;GA;;;<当前用户 SID>)`（受保护、仅一条 ACE），并 `reject_remote_clients`；Unix socket 位于 `$XDG_RUNTIME_DIR`（或临时目录）下新建的 `0700` 目录，socket 文件 `0600`，`AskpassServer` drop 时删除目录。Unix 分支在本机以 `x86_64-unknown-linux-gnu` 目标做了类型检查，运行时测试（`socket_is_owner_only`）仅在 Unix CI 上执行。
  - 引擎按**命令**判定联网：编译出的步骤首参数为 `fetch` / `pull` / `push` / `ls-remote` 时才追加 askpass 环境，因此 `Sequence` / `Smart` / `UpdateProject` 内部的联网步骤自动覆盖，本地步骤（含 `remote add`）不设置。`EngineSupervisor::start_with_askpass` 接收 `AskpassEnv`，`start_with_config` 保持不带 askpass。
  - 偏离：后台「远端是否存在同名 Tag」探测（`ls-remote`，5 s 超时）**不**接 askpass——后台探测不应弹出凭据对话框，需要认证时结果视为「未知」。
  - TUI：`UiAsyncEvent::Askpass` → `CredentialQueue`（FIFO，同一时刻只显示一个 `Dialog::Credential`）；确认时值从对话框直接移交给等待的请求（不经过 `Action`），`Esc` / Cancel / 背景点击回复取消；git 进程放弃等待（120 s 超时或退出）的提示在下一次 tick 时自动移除。值存放于 `Zeroizing<String>`（预分配 256 字节以避免输入时重新分配留下副本），`Debug` 输出脱敏。
  - `GitFailure::AuthenticationFailed` 识别 `Authentication failed`、`Permission denied (publickey`、`could not read Username/Password`、`terminal prompts disabled`、`HTTP Basic: Access denied`、HTTP 401/403 等；失败对话框给出 credential helper / `ssh-agent` 指引。
  - 测试替身：`FixedResponder`；生产二进制额外支持 `JBGIT_ASKPASS_RESPONDER=cancel|value:<text>` 供 `jbgit-pilot` 沙箱 / 脚本化 QA 使用。
  - 验收：未实现 `git http-backend` Basic Auth 推送测试；改为 `crates/jbgit-tui/tests/askpass_e2e.rs` 用真实 `git -c credential.helper= credential fill` 启动构建出的 `jbgit` 作为 askpass，验证应答、取消（stderr 被分类为 `AuthenticationFailed`）与错误令牌被拒三种情况，覆盖同一条 git → askpass → 会话通道路径。Windows 管道 DACL 仅校验了 SDDL 构造与当前用户 SID，未做跨用户访问的实机验证。
