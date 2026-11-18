# 03: 实现多行提交信息编辑器、Amend 修正模式、Sign-off/Hooks 选项及提交流闭环

Status: ready-for-agent
Blocked by: 01, 00-architecture-and-git-engine/issues/02-hybrid-git-engine-and-watcher.md
Spec: [../spec.md](../spec.md)
Milestone: M4 — 变更暂存工作台与局部暂存

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/index/ui/GitStageCommitPanel.kt`
  - `reference/platform/vcs-impl/src/com/intellij/vcs/commit/AmendCommitHandlerImpl.kt`
  - `reference/plugins/git4idea/backend/src/checkin/GitSkipHooksCommitHandlerFactory.kt`

---

## 1. 背景与目标

实现 `[1 Commit]` 左下分屏的 **Commit Panel**，提供媲美 IDE 体验的多行提交信息编辑器（含规范长度标尺与历史记录回填）、无损切换草稿的 `Amend` 修正模式、`Sign-off` / `Run Hooks` / `Author` 选项，以及 `Commit` 与 `Commit and Push…` 完整闭环。

---

## 2. 核心实现任务清单

- [ ] **多行提交信息编辑器 (`CommitMessageEditor`)**：
  - 支持多行光标移动、自动软换行、UTF-8/中文/Emoji 宽度对齐、Bracketed Paste 粘贴、词级删除（`Ctrl+W` / `Alt+Backspace`）与多步 `Undo/Redo`。
  - 第 1 行末尾显示动态列宽标尺（`≤50` 常规灰、`51..72` 柔和琥珀色、`>72` 警告橙色）。
  - 支持按 `Ctrl+E` 一键唤起外部 `$EDITOR` 编辑复杂长文后自动同步回填。
  - 实现 **Commit Message History 浮窗 (`Ctrl+M` / `[🕘]`)**：持久化最近 20 条历史提交说明与未提交草稿，支持上下检索一键填入。
- [ ] **非模态 `Amend` 修正工作流（对标 `NonModalAmendCommitHandler`）**：
  - 勾选 `[x] Amend`（`Alt+M`）时：先暂存用户当前手写草稿 `saved_draft`，异步读取 `HEAD` 提交完整说明填入编辑器，并在上方 `GitStageTree` 挂载只读参考节点 `▶ Head Commit to Amend (<short_sha>)` 展示上次提交已含文件；取消勾选 `[ ] Amend` 时原样恢复 `saved_draft`！
- [ ] **提交选项与自适应双通道执行**：
  - 选项开关：`[ ] Amend`、`[ ] Sign-off`（自动检测避免重复追加 `Signed-off-by:`）、`[x] Run Hooks`（关闭传 `--no-verify`）、可选自定义 `Author`。
  - 预检拦截：存在未解决 `Merge Conflicts` 时阻止提交并弹窗定位冲突；`Commit Message` 为空时高亮输入框边框提示；`Staged` 为空且非 `Amend` 时弹出确认询问是否 `Stage All Tracked and Commit`。
  - 执行策略：检测 `.git/hooks/pre-commit`、`commit-msg` 可执行状态及 `git config commit.gpgsign`；涉及 Hooks 或 GPG/SSH 签名时走 `GitCliExecutor` 流式展示 Hook 检查进度条（失败时直接在弹窗与 Console 展示 lint/test 报错详情！），纯常规无钩子提交可走极速路径。
  - 提交成功后清空编辑框、刷新状态树与提交图，并在底栏弹出带 `[Undo Commit]` 快捷动作的成功通知条。
  - 实现 `[Commit and Push… (Ctrl+Alt+K)]` 联动触发推送流。

---

## 3. 验收标准 (Acceptance Criteria)

- [ ] 勾选 `Amend` 能自动加载 `HEAD` 消息，取消勾选能找回先前输入的草稿文字不丢失。
- [ ] 当仓库配置了返回非零退出码的 `.git/hooks/pre-commit` 脚本时，勾选 `Run Hooks` 提交会拦截失败并清晰展示脚本输出且保留已写好的 Commit Message 不丢失；取消勾选 `Run Hooks` 则可通过 `--no-verify` 成功提交。
- [ ] 在空仓库（Unborn HEAD）首次提交及常规分支提交、Amend 提交场景下均通过集成测试验证。

---

## Comments
