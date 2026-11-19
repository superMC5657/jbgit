# 01: 实现 `GitBranchesPopup` 分支浮窗（`nucleo` 模糊速查、级联操作子菜单与顶栏分支挂件）

Status: ready-for-agent
Blocked by: 00-architecture-and-git-engine/issues/01-workspace-tui-runtime-and-hit-test.md, 00-architecture-and-git-engine/issues/02-hybrid-git-engine-and-watcher.md, 01-visual-system-darcula-and-widgets/issues/01-darcula-theme-14-color-palette-and-icons.md
Spec: [../spec.md](../spec.md)
Milestone: M5 — 分支浮窗、右键菜单与高阶操作

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/ui/branch/GitBranchPopupActions.java`
  - `reference/plugins/git4idea/backend/src/ui/branch/GitRemoteBranchCheckoutUtil.kt`
  - `reference/plugins/git4idea/shared/resources/intellij.vcs.git.shared.xml`

---

## 1. 背景与目标

复刻 IntelliJ IDEA 使用频率最高的全局入口之一 —— 顶栏当前分支胶囊挂件（`GitBranchWidget`）与 **`GitBranchesPopup` 分支速查浮窗**（`Ctrl+Shift+\`` / 点击顶栏分支名呼出），集成 `nucleo-matcher` 零延迟模糊检索、Reflog 近期切换分支记忆、`↑↓` 同步状态与二级级联分支操作子菜单。

---

## 2. 核心实现任务清单

- [ ] **顶栏当前分支状态挂件 (`BranchStatusBarWidget`)**：
  - 实时显示当前分支名（或 Detached HEAD 短哈希 / Ongoing 状态如 `Rebasing 2/5`）及相对于上游分支的 `↑outgoing ↓incoming` 计数，注册 `HitTarget::BranchWidgetPill` 点击即呼出分支浮窗。
- [ ] **`GitBranchesPopup` 主列表与 Reflog `Recent` 解析**：
  - 解析 `.git/logs/HEAD` 最近 50 条记录提取最近检出过的本地分支（最多展示前 5 个），构建：`Ongoing Actions`（若在变基/合并中） -> `+ New Branch…` / `Checkout Tag or Revision…` -> `Recent` -> `Local Branches` -> `Remote Branches` -> `Tags`（可选）。
  - 集成 `nucleo-matcher`：打开浮窗直接键入任意字符即时模糊过滤全部分支，并在渲染时将命中的字符索引以 `#548AF7` 加粗高亮。
  - 在分支条目右侧展示星标 `★`（支持点击切换）、跟踪上游灰字提示（如 `origin/main`）与 `↑N ↓M`。
- [ ] **级联二级分支动作子菜单（对标 `GitBranchPopupActions`）**：
  - 选中任意分支按 `Enter` / `→` 或鼠标点击弹出右侧对齐的二级动作面板：
    - 本地分支：`Checkout`、`New Branch from 'X'…`、`Checkout and Rebase onto Current`、`Compare with Current`（跳转 Log 视图展示双向差异）、`Show Diff with Working Tree`、`Rebase Current onto 'X'`、`Merge 'X' into Current`、`Update`、`Push…`、`Rename…`、`Delete`。
    - 远程分支：**智能 `Checkout`**（本地不存在同名分支时自动新建并绑定 `--track`；若已存在同名分支且偏离则弹窗提供安全选项，对标 `GitRemoteBranchCheckoutUtil`）、`New Branch from 'origin/X'…`、`Merge into Current`、`Rebase Current onto 'origin/X'`、`Delete Remote Branch…`。
  - **未合并分支删除安全护栏**：删除尚未合并进 `HEAD` 或上游的分支时，弹窗明确警告未合并提交数并提供 `[Force Delete (-D)]` 与取消选项；删除成功后在底栏通知提供 `[Restore Branch]`（基于原 SHA 一键恢复刚删掉的分支，复刻 IDEA `GitRestoreSavedChanges` 贴心挽救体验！）。

---

## 3. 验收标准 (Acceptance Criteria)

- [ ] 按快捷键打开浮窗后直接输入 `f/dl` 可在 < 1ms 内过滤定位到 `feature/dag-layout` 并高亮匹配字符。
- [ ] 对 `origin/feature-new` 执行 `Checkout` 自动在本地创建 `feature-new` 并配置好上游跟踪关系。
- [ ] 删除分支后点击底栏通知的 `[Restore]` 能按原 Commit Oid 瞬间无损恢复该分支。

---

## Comments

- 2026-11-18 (agent): Implemented. Popup model/render/keys in `crates/jbgit-tui/src/branches_popup.rs` (Ongoing row → New Branch / Checkout Tag or Revision → reflog `Recent` (`parse_recent_checkouts`, last 50 entries, ≤5) → Local (folder groups) → Remote → Tags when searching; `nucleo` indices highlighted bold `#548AF7`; ★ toggle, upstream + ↑↓, `[↻ Fetch] [⚙]`). Opens via `Ctrl+\`` / `Ctrl+Shift+\`` / header pill click / `Alt+B` outside the Log tab (Log keeps Spec 03's `Alt+B` pane toggle). Branch submenus in `crates/jbgit-tui/src/menus/branch.rs`; smart remote checkout and unmerged-delete guard + `[Restore Branch]` notification in `app_git.rs`. Engine-level acceptance in `crates/jbgit-engine/tests/git_ops_integration.rs`, UI acceptance in `crates/jbgit-tui/src/spec05_tests.rs`.
