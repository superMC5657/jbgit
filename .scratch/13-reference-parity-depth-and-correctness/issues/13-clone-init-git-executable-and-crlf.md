# 13: Clone / Init 仓库生命周期、`[git] executable` 配置与 Windows CRLF 预警

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M10 — reference 深度对齐与正确性加固
Priority: P2

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/checkout/GitCloneUtils.kt`、`ui/GitCloneDialogComponent.kt`、`commands/GitShallowCloneOptions.kt`
  - `reference/plugins/git4idea/backend/src/actions/GitInit.java`、`repo/GitRepositoryInitializerImpl.java`
  - `reference/plugins/git4idea/backend/src/config/GitExecutableDetector.java`
  - `reference/plugins/git4idea/backend/src/crlf/{GitCrlfProblemsDetector.java,GitCrlfDialog.java,GitCrlfUtil.java}`
- **关联文件**：
  - `crates/jbgit-core/src/git_ops/ops.rs`
  - `crates/jbgit-engine/src/{cli.rs,ops.rs,probe.rs,stage.rs}`
  - `crates/jbgit-tui/src/{main.rs,session.rs,repo_switcher.rs,dialogs.rs,config/jbgit_config.rs,stage_view/workbench.rs}`

---

## 1. 背景与目标

Spec 12 落地了进程内仓库切换器（`RepoSession::rebind`），现已具备承载仓库创建与克隆生命周期的基础。同时，用户反馈在非标准安装环境下无法指定 `git` 二进制路径，Windows 下也缺少 `core.autocrlf` 提交前 CRLF 风险提醒。本工单统一补齐这三项环境与生命周期能力（2026-11-24 维护者拍板全部纳入 M10）。

## 2. 任务清单

- [ ] **Initialize & Clone Repository**：
  - 当用户在**非 Git 目录**启动 `jbgit` 时，不再直接报错退出，而是展示轻量引导模态，提供三项入口：① `Initialize Git Repository`（在当前目录执行 `git init` 并直接进入工作台）；② `Clone Repository…`；③ `Open Recent / Switch Repository…`（复用 `RepoSwitcher`）；
  - 在已打开仓库的 `RepoSwitcher` 与 `CommandCatalog` 中同样提供 `Clone Repository…` 入口；
  - `CloneDialog` 字段：远程 URL（粘贴自动推导默认目录名）、目标目录、可选 `--depth <N>`（浅克隆）、可选 `--recurse-submodules`；执行过程复用 `askpass` 凭据桥并流式展示 `git clone --progress` 进度，克隆成功后经 `RepoSession::rebind` 自动切入新仓库。
- [ ] **可配置 `[git] executable` 与扩展探测**：
  - `jbgit.toml` 新增 `[git] executable = ""`（空串表示自动探测）；
  - `engine/cli.rs::find_git_executable` 按优先级解析：`[git] executable`（若配置且无效则给出清晰错误提示）→ `PATH` → 平台常见安装目录兜底（Windows：`Program Files\Git\cmd\git.exe`、`Program Files\Git\bin\git.exe`、`%LOCALAPPDATA%\Programs\Git\cmd\git.exe`；Unix/macOS：`/usr/local/bin/git`、`/opt/homebrew/bin/git`、`/usr/bin/git`）；
  - 在帮助/关于或控制台启动摘要中显示实际解析到的 `git` 路径与版本号。
- [ ] **Windows CRLF 提交前检测与修复引导**：
  - 在 `jbgit-engine/src/stage.rs` 实现 `detect_crlf_commit_problem_sync`（对齐 `GitCrlfProblemsDetector`）：仅当 `core.autocrlf` 未设为 `true`/`input` 时，检查本次待提交的已暂存文本文件（跳过二进制、LFS、`git check-attr text crlf` 已显式指定策略的文件）是否包含 `\r\n`；
  - 提交前命中风险时弹出 `CrlfWarning` 确认对话框，提供三项操作：① **Fix & Commit**（执行 `git config core.autocrlf true` 后提交）；② **Commit As Is**；③ **Don't warn again**（写入 `[commit] warn_crlf = false` 或每仓偏好）。

## 3. 测试

- **单元测试**：URL 推导仓库目录名（HTTPS / SSH / `.git` 后缀 / 尾斜杠）；`find_git_executable` 显式路径优先与无效路径错误测试；CRLF 检测跳过二进制与 `.gitattributes` `text=auto`/`eol=crlf` 的纯函数测试。
- **引擎集成测试**：在空临时目录执行 `git init` 后 `RepoLocation::discover` 成功；从本地 bare 仓库执行 `clone --depth 1` 并校验浅克隆状态；构造 `core.autocrlf=false` + 暂存 CRLF 文件触发预警，执行 Fix 后 `core.autocrlf == true`。
- **TUI 会话测试**：在非 Git 目录启动进入引导界面选择 Init 并正常渲染主工作台。
