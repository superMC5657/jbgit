# 07: Commit 模板、`MERGE_MSG` 自动加载与 Push/Update 健壮性加固

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M10 — reference 深度对齐与正确性加固
Priority: P1
Blocked by: 01

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/repo/GitCommitTemplateTracker.kt`、`checkin/GitMergeCommitMessageReader.kt`、`checkin/GitUserNameNotDefinedDialog.kt`
  - `reference/platform/vcs-impl/src/com/intellij/vcs/commit/message/{BodyLimitInspection.kt,ReformatCommitMessageAction.java}`
  - `reference/plugins/git4idea/backend/src/push/{GitPushOperation.java,GitPushSupport.java,GitPushTargetPanel.java,GitPushDivergenceDetector.kt}`
  - `reference/plugins/git4idea/backend/src/update/{GitUpdateProcess.java,GitRebaseOverMergeProblem.java,FixTrackedBranchDialog.kt,GitSubmoduleUpdater.kt}`
- **关联文件**：
  - `crates/jbgit-engine/src/{stage.rs,ops.rs,details/outgoing.rs}`
  - `crates/jbgit-tui/src/stage_view/{workbench.rs,commit_editor.rs}`、`crates/jbgit-tui/src/log_view/push_dialog.rs`
  - `crates/jbgit-tui/src/app/git_workflows.rs`、`crates/jbgit-tui/src/views/{commit.rs,push_dialog.rs}`

---

## 1. 背景与目标

本工单把 Commit → Push → Update Project 这条高频主链路上的“体验断层”全部接平：
- Commit 侧：加载 `commit.template` 与各类 sequencer 预生成消息（`MERGE_MSG`/`SQUASH_MSG`）、支持 `--cleanup=strip` / `--allow-empty` / 正文 72 列折行与 Author 补全、缺少 `user.name`/`user.email` 时直接弹窗引导配置，以及让 **Commit & Push** 直接衔接完整 Push 对话框；
- Push/Update 侧：Push 弹窗补齐 `--no-verify`、Detached HEAD 推送、内联“定义远端”与精确 `--force-with-lease=<ref>:<oid>`；Push 被拒重试套上 `GitOp::Smart` 并预警 Rebase over Merge；Update Project 在上游丢失时引导重选追踪分支并支持顺带更新 Submodule。

## 2. 任务清单

- [x] **Commit 模板、Sequencer 消息与编辑器打磨**：
  - 启动/切仓/进入 `Merge`/`CherryPick`/`Revert` 状态时自动探测 `<git_dir>/MERGE_MSG`、`SQUASH_MSG`（优先级高于草稿，若编辑器为空或刚产生冲突则预填，并按 `core.commentChar` 处理注释行）；空编辑器在配置了 `commit.template` 时加载模板内容；
  - CLI 提交路径补 `--cleanup=strip`（`git2` 路径同步剥离 `#` / `core.commentChar` 注释行），避免模板或 `$EDITOR` 注释被直接提交；
  - Commit 选项区新增 `--allow-empty` 开关与 Author 输入框的历史作者自动补全（来自仓库近期提交）；
  - Commit 编辑器增加正文行宽 >72 字符高亮警告与一键 Reformat（按 72 列安全折行正文段落，不破坏代码块/列表/trailer）；
  - 提交前若 `precheck.user_name` 或 `user_email` 缺失，弹出 `Configure Git Identity` 对话框（可选写入本地或全局 `git config`）后继续提交；
  - `Undo Last Commit` 前若检测到 `HEAD` 为合并提交或已推送至 upstream，弹出二次确认提示。
- [x] **Commit & Push 与 Push 对话框加固**：
  - Commit 面板触发 **Commit & Push** 时，在提交成功后直接打开 `PushDialog`（或先让用户在 PushDialog 确认目标与选项），替代原来的简单 Yes/No 确认框；
  - `GitOp::Push` 新增 `no_verify: bool`（`--no-verify`）与 `expected_remote_oid: Option<Oid>`（用于精确 `--force-with-lease=<ref>:<oid>`，对齐 `GitPushDivergenceDetector`）；
  - 解除 Detached HEAD 打开 `PushDialog` 的硬禁止（允许选择目标远端与分支推送当前 `HEAD` OID）；
  - 当仓库无任何 remote 时，Push 弹窗目标区提供内联 `[+ Define Remote…]` 按钮直接拉起 Add Remote 流程；
  - 尊重 `remote.<name>.push` refspec（若配置了自定义 refspec 则作为默认目标展示）。
- [x] **Push Rejected 与 Update Project 健壮性**：
  - `Push Rejected` 对话框的「Merge & Retry」/「Rebase & Retry」将内部 `Fetch → Merge/Rebase` 包裹在 `GitOp::Smart` 中（工作区有未提交改动时自动 stash/恢复，不再直接失败），并在执行 Rebase 重试前检测待变基区间是否包含合并提交，若有则提示 `Rebase over Merge commits` 警告；
  - `Update Project`：当当前分支无 upstream 或追踪分支已在远端消失（`[gone]`）时，弹出追踪分支选择器而非直接报错；选项弹窗增加「同时更新已初始化子模块（`Update Submodules`）」复选框（仅在 `has_submodules` 时可见）。

## 3. 测试

- **单元测试**：`core.commentChar` 剥离与正文 72 列 Reformat 算法测试；`--force-with-lease=<ref>:<oid>` 与 `--no-verify` 参数编译测试；`Rebase over Merge` 检测测试。
- **引擎集成测试**：配置 `commit.template` → 预检读取模板；冲突合并产生 `MERGE_MSG` → 自动加载；无 `user.name` 仓库配置身份后提交成功；脏工作区下触发 Push Rejected → Smart Rebase & Retry 成功推上并恢复工作区改动。
- **TUI 会话测试**：Detached HEAD 打开 Push 对话框；Commit & Push 走通 PushDialog 提交流程。
