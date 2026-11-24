# 14: GPG 签名与 SSH Host-Key 交互、Bisect 控制与 Submodule/Worktree/Ignore 增强

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M10 — reference 深度对齐与正确性加固
Priority: P2

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/config/gpg/{GitGpgConfigDialog.kt,GitGpgConfigUtils.kt}`、`ui/GitTagDialog.java`
  - `reference/plugins/git4idea/backend/src/commands/GitHandlerAuthenticationManager.java`（SSH host-key 确认）
  - `reference/plugins/git4idea/backend/src/workingTrees/{GitWorktreeIncludeFileService.kt,GitWorktreeSetupScriptRunner.kt}`
  - `reference/plugins/git4idea/backend/src/ignore/actions/{GitIgnoreFileActionGroup.kt,GitExcludeActions.kt}`
  - `reference/plugins/git4idea/backend/src/index/ui/GitStageDiffUtil.kt`（`:131-172` submodule gitlink 暂存）
- **关联文件**：
  - `crates/jbgit-core/src/git_ops/ops.rs`、`crates/jbgit-engine/src/{askpass.rs,ops.rs,stage.rs,probe.rs}`
  - `crates/jbgit-tui/src/{dialogs.rs,worktrees_view.rs,stage_view/workbench.rs,menus/stage.rs,menus/branch.rs,views/banner.rs}`

---

## 1. 背景与目标

按 2026-11-24 维护者拍板，将三组体量可控、实用度高的进阶能力（GPG/SSH 交互、Bisect 面板控制、以及 Submodule/Worktree/`.gitignore` 细节增强）打包落地；`In-Memory Rebase` 已明确裁定 `wontfix`（保持原生 CLI rebase 兼容 Hooks/GPG）。

## 2. 任务清单

- [ ] **GPG 签名交互与 SSH Host-Key 指纹确认**：
  - `CommitRequest` 新增 `gpg_sign: Option<bool>`（`None` 跟随 `commit.gpgsign`，`Some(true)` 传 `-S`，`Some(false)` 传 `--no-gpg-sign`），Commit 面板选项区提供三态/开关显示（默认跟随仓库配置）；
  - `GitOp::CreateTag` 新增 `sign: bool`（`-s`），Create Tag 对话框增加「Sign tag (`-s`)」复选框；
  - 扩展 `askpass.rs`：识别 SSH 首次连接主机密钥确认提示（`Are you sure you want to continue connecting (yes/no/[fingerprint])?`），将其标记为**非掩码确认对话框**完整展示指纹原文（引导输入 `yes`/`no` 而非密码掩码）；同时在 `GitFailure` 中识别 GPG/pinentry 签名失败错误并给出排查提示（`gpg-agent` / `pinentry` 配置建议）。
- [ ] **Bisect 交互式工作流控制**：
  - 新增纯声明式 `GitOp::{BisectStart { bad: Option<String>, good: Vec<String> }, BisectMark { good: bool }, BisectSkip, BisectReset}`；
  - Log 提交菜单增加「Start Bisect Here (Bad)」/「Mark Good」入口；
  - 当仓库处于 `RepositoryState::Bisect` 时，顶部状态横幅（`views/banner.rs`）展示 `[Good]` / `[Bad]` / `[Skip]` / `[Reset]` 可点击按钮与对应快捷键（`CommandCatalog` 统一注册），状态提示剩余估算步数。
- [ ] **细节增强三件套（Submodule / Worktree / `.gitignore`）**：
  - **Submodule gitlink 暂存**：允许在 Stage 工作台对 Submodule 条目执行**整条暂存（`git add <submodule>`）、取消暂存（`git restore --staged <submodule>`）与放弃指针变更**（仍禁止对其内部做文本行级暂存），使提交流程能正常提交子模块指针更新；
  - **Worktree `.worktreeinclude` 与远程分支创建**：`AddWorktree` 支持直接从远程分支菜单创建与可选 `--force`；若仓库根目录存在 `.worktreeinclude`（gitignore 语法匹配待复制的被忽略文件如 `.env`），在 `git worktree add` 成功后按匹配规则复制文件到新 worktree，并支持 `[worktree] setup_command` 可选创建后钩子提示运行；
  - **灵活 `.gitignore`**：Stage 菜单「Add to Ignore…」支持选择规则粒度（精确路径 `/a/b.log`、同扩展名 `*.log`、父目录 `/a/`）与目标文件（仓库根 `.gitignore`、最近嵌套 `.gitignore` 或 `.git/info/exclude`），并增加「Open `.gitignore` / `exclude` in `$EDITOR`」快捷动作。

## 3. 测试

- **单元测试**：`is_ssh_host_key_prompt` 识别测试；`Bisect*` / `CreateTag { sign: true }` / `CommitRequest { gpg_sign }` 参数编译测试；`.worktreeinclude` 路径匹配与 `.gitignore` 规则生成测试。
- **引擎集成测试**：
  - 5 提交线性仓库走完 `BisectStart` → `BisectMark(good/bad)` → 定位故障提交 → `BisectReset`；
  - 含子模块的临时仓库：更新子模块指向新提交 → 暂存/取消暂存 gitlink → 成功提交子模块指针更新；
  - 含 `.worktreeinclude` 的仓库创建新 worktree → 匹配的忽略文件被自动复制到新 worktree。
- **TUI 会话测试**：进入 Bisect 状态验证横幅按钮与快捷键联动；Stage 树右键未追踪 `.log` 文件验证按扩展名忽略写入 `.gitignore`。
