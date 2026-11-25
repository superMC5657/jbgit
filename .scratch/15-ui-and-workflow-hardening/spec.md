# Spec 15: 工作流正确性与 UI 交互加固（Phase 10）

- **目录**：`.scratch/15-ui-and-workflow-hardening/`
- **QA 规程**：[`docs/agents/ai-exploratory-testing.md`](../../docs/agents/ai-exploratory-testing.md)
- **下属 Issues**（按“功能正确性 → 交互收敛 → 窄屏布局 → Hover 一致性”拆分为 4 个正交原子工单）：
  - [`01-smart-merge-blocked-files-and-stash-restore-failure.md`](./issues/01-smart-merge-blocked-files-and-stash-restore-failure.md)（**P2** · Smart Merge 2 空格缩进被阻挡文件名解析、`stash pop` 非冲突恢复失败分类）
  - [`02-commit-initial-selection-and-unified-ctrl-a.md`](./issues/02-commit-initial-selection-and-unified-ctrl-a.md)（**P2** · `[1] 提交` 首次载入默认选中文件行、`PushDialog` 与其余内嵌弹窗文本行统一 `Ctrl+a` 全选）
  - [`03-modal-narrow-layout-overflow-and-borders.md`](./issues/03-modal-narrow-layout-overflow-and-borders.md)（**P2** · 冲突解决弹窗窄屏适配、两分支对比栏宽度预算、`Alt+T` 边缘缝隙、Annotate/FileHistory 叠层底边框对齐、FileHistory 80 列工具栏与上下文菜单宽度修正）
  - [`04-interactive-hover-feedback-consistency.md`](./issues/04-interactive-hover-feedback-consistency.md)（**P3** · 冲突解决器底部按钮、Log 筛选芯片/`⚙` 与顶栏分支胶囊鼠标 Hover 反馈补齐）

---

## 1. 工单与代码面全景表

| 工单 | 优先级 | 涉及 Crate | 核心定位文件 |
| :--- | :---: | :--- | :--- |
| [`01` Smart Merge 文件解析与 Stash 恢复分类](./issues/01-smart-merge-blocked-files-and-stash-restore-failure.md) | **P2** | `jbgit-core`, `jbgit-engine`, `jbgit-tui` | `crates/jbgit-core/src/git_ops/outcome.rs:118-154`, `crates/jbgit-engine/src/ops.rs:2684-2695`, `crates/jbgit-tui/src/app/git_workflows.rs:2756-2871` |
| [`02` Commit 初始选中与弹窗 `Ctrl+a` 统一](./issues/02-commit-initial-selection-and-unified-ctrl-a.md) | **P2** | `jbgit-tui` | `crates/jbgit-tui/src/stage_view/workbench.rs:642-677`, `crates/jbgit-tui/src/log_view/push_dialog.rs:24-51`, `crates/jbgit-tui/src/input/keyboard.rs:1271-1292`, `crates/jbgit-tui/src/dialogs.rs:2506-2715` |
| [`03` 弹窗窄屏布局、边框与菜单宽度修正](./issues/03-modal-narrow-layout-overflow-and-borders.md) | **P2** | `jbgit-tui` | `crates/jbgit-tui/src/views/conflict_merge.rs`, `views/log.rs`, `tree_browser.rs`, `views/annotate.rs`, `views/file_history.rs`, `views/overlays/context_menu.rs` |
| [`04` 交互控件 Hover 视觉反馈一致性](./issues/04-interactive-hover-feedback-consistency.md) | **P3** | `jbgit-tui` | `crates/jbgit-tui/src/views/conflict_merge.rs:755-776`, `views/log.rs:342-378, 633-755`, `views/header.rs:168-196` |

---

## 2. 执行顺序与收口基线

4 个工单代码面正交，无强制串行依赖，按优先级分两批推进：

```text
Batch 1 (核心正确性与交互一致):
  01 Smart Merge 被阻挡文件解析与 Stash 恢复失败分类
  02 Commit 初始文件选中与弹窗文本 Ctrl+a 全选统一

Batch 2 (窄屏布局与 Hover 打磨):
  03 弹窗窄屏布局/溢出/边框对齐与菜单宽度修正
  04 冲突按钮 / 筛选芯片 / 顶栏分支胶囊 Hover 反馈补齐
```

---

## 3. 验收总则

- 每个工单须配套回归单元测试或 `TestBackend` 渲染测试（修复前必挂）；
- 全量通过：
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace --locked`
- 对每个工单涉及的场景使用 `jbgit-pilot` 执行定向实机复验，并将验证记录回写至对应 Issue 的 `## Comments`。
