# 02: 实现通用右键/键盘上下文菜单引擎、视口边缘自动翻转与助记符系统

Status: ready-for-agent
Blocked by: 00-architecture-and-git-engine/issues/01-workspace-tui-runtime-and-hit-test.md, 01-visual-system-darcula-and-widgets/issues/01-darcula-theme-14-color-palette-and-icons.md
Spec: [../spec.md](../spec.md)
Milestone: M5 — 分支浮窗、右键菜单与高阶操作

- **参考源码**：
  - `reference/plugins/git4idea/backend/resources/intellij.vcs.git.backend.xml` (`Git.Log.ContextMenu`, `Git.Stage.Tree.Menu`)
  - `reference/platform/vcs-log/impl/resources/intellij.platform.vcs.log.impl.xml` (`Vcs.Log.ContextMenu`)

---

## 1. 背景与目标

构建全应用通用的 **桌面级悬浮上下文菜单框架（`ContextMenuOverlay`）** 与 **模态确认/输入对话框组件库**，打通 Log 提交行、分支 Pill 胶囊、左侧分支树节点、暂存状态树节点及 Diff 代码块五大热区的 **鼠标右键即点即弹** 与 **纯键盘快捷键等价呼出**。

---

## 2. 核心实现任务清单

- [ ] **多级级联 `ContextMenuOverlay` 渲染与布局引擎**：
  - 使用 `ratatui::widgets::Clear` 擦除底层背景后绘制 `#313438` 卡片、圆角细边框 `╭─╮`、分隔横线 `├─┤` 与右下 1 格暗调投影。
  - **视口边缘碰撞自动翻转（Auto-Flip）**：计算光标锚点 `(anchor_x, anchor_y)` 与终端宽高 `(W, H)`，右溢时向左展开，下溢时向上展开；二级子菜单右溢时自动向父菜单左侧镜像展开，超长菜单在内部启用上下滚动指示箭头 `▴`/`▾`。
  - **交互控制**：支持鼠标悬停自动高亮并延迟 150ms 展开二级子菜单、方向键 `↑↓←→` 导航（自动跳过分割线）、首字母下划线助记符一键触发、置灰禁用项展示及底栏禁用原因说明、`Esc` 或外部点击逐层关闭。
- [ ] **五大场景上下文菜单绑定落地**：
  1. **Log 提交表格行菜单**（完整覆盖 [Spec 05 第 3.1 节](../spec.md#31-提交日志行右键菜单对标-gitlogcontextmenu--vcslogcontextmenu) 全部动作项，自动识别单选 vs 多选提交上下文切换可用状态）；
  2. **Log 行内特定 Pill 胶囊菜单**（右键直接点在某枚 `▐⎇ feat/x▌` 胶囊上时，直接弹出针对 `feat/x` 分支的专属操作菜单！）；
  3. **左屏 Branches Dashboard 节点菜单**（Checkout / Merge / Rebase / Push / Update / New Branch / Rename / Delete）；
  4. **Stage 暂存树文件/目录菜单**（Stage / Unstage / Stage Without Content / Rollback / Stash Selected / Add to `.gitignore` / Delete / Copy Path / Show Diff）；
  5. **Diff 视图代码块右键菜单**（Stage Hunk or Lines / Unstage / Rollback / Copy Patch / Toggle Unified vs Side-by-Side）。
- [ ] **通用轻量模态对话框组件库 (`ModalDialog`)**：
  - 单行文本输入弹窗（用于 `New Branch`、`New Tag`、`Rename Branch`，含 Git 分支名合法性实时校验器 `git check-ref-format` 规则！）；
  - 单选组对话框（用于 `GitResetDialog` 的 `Soft / Mixed / Hard / Keep` 四选一）；
  - 危险操作红色高亮二次确认框（用于 `Reset --hard`、`Rollback Discard`、`Drop Commit`）。

---

## 3. 验收标准 (Acceptance Criteria)

- [ ] 在终端最底行、最右列边缘处点击鼠标右键，弹出的菜单及二级子菜单 100% 完整落在屏幕可视范围内，零字符截断。
- [ ] 在未选中的第 8 行提交上直接点击鼠标右键，第 8 行先立即高亮成为选中行，同时弹出针对第 8 行提交的菜单；若先多选第 3~6 行再在第 5 行上右键，则保留 3~6 行多选态并弹出多提交菜单（如 `Squash Commits` 激活）。
- [ ] 新建分支弹窗输入非法名称（如含空格、`..`、以 `-` 开头或与现有分支重名）时实时显示红色内联校验错误并禁用确认按钮。

---

## Comments

- 2026-11-18 (agent): Implemented. `ContextMenuItem` gained icons, separators and disabled reasons (shown in the footer); renderer in `views/overlays.rs` (rounded border, `├─┤` separators, darkened 1-cell shadow, underlined mnemonics, `▴/▾` scrolling, right/bottom auto-flip incl. submenus mirrored left). 150 ms hover-to-open/close (`input_menus.rs::flush_menu_hover`), `Shift+F10` / `Menu` / `.` / `m` keyboard trigger anchored on the focused row (`input_menus.rs`). Menus for Log rows (full §3.1), ref pills, Branches tree, Stage tree (§3.2) and diff views under `crates/jgit-tui/src/menus/`. Dialog library in `dialogs.rs` + `views/dialogs.rs` (check-ref-format validation with disabled OK, radio Reset dialog, red confirmations, message editor, rebase todo table).
