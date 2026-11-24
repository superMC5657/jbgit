# 09: 真正 3-Way 自动合并、Result 内联编辑与多文件冲突队列视图

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M10 — reference 深度对齐与正确性加固
Priority: P2
Blocked by: 02

- **参考源码**：
  - `reference/platform/vcs-impl/src/com/intellij/openapi/vcs/merge/MultipleFileMergeDialog.kt`
  - `reference/plugins/git4idea/backend/src/merge/{GitMergeUtil.java,GitMergeProvider.java}`、`conflicts/GitConflictsPanel.kt`
  - `reference/plugins/git4idea/backend/src/merge/GitConflictResolver.java`、`GitMergeCommittingConflictResolver.java`
- **关联文件**：
  - `crates/jbgit-core/src/conflict.rs`
  - `crates/jbgit-engine/src/pool/snapshot.rs`（`load_conflict_document_sync`）、`crates/jbgit-engine/src/ops.rs`
  - `crates/jbgit-tui/src/{app/modal_stack.rs,app/git_workflows.rs,views/conflict_merge.rs}`

---

## 1. 背景与目标

Issue 02 修复了冲突方向与 CRLF/Delete-Modify 正确性后，冲突解决器仍停留在“解析磁盘 `<<<<<<<` 标记 + 枚举选边 + 单文件手动跳转”的初级形态，与 IDEA 标志性的三方合并体验有明显差距：
1. 未加载 index `:1`（共同祖先 Base），无法识别并一键自动应用非冲突改动，也未展示 diff3 Base 内容；
2. 中间 Result 栏只读，想微调一行必须退回 `$EDITOR`，且缺少 `Neither`（双向丢弃）与单块撤销；
3. 多文件冲突时没有统一队列视图，无法一眼看到所有冲突文件在 Yours/Theirs 的状态、批量处理、重置误解文件（`git checkout -m`），或在最后一个文件解决后自动提示继续 Rebase/Merge。

## 2. 任务清单

- [x] **基于 `:1/:2/:3` 的真正 3-Way 模型与自动合并（`jbgit-core/src/conflict.rs`）**：
  - `load_conflict_document_sync` 从 Git index 完整读取 `:1`（Base）、`:2`（Ours）、`:3`（Theirs）三方 blob（兼容仅有一侧/无 Base 的 Add/Add 场景，按 Issue 02 的 `is_reverse_conflict_state` 对齐方向）；
  - 在 `jbgit-core` 基于 `imara-diff` 实现纯函数 3-way 块归并，区分：`Unchanged`、`LeftOnly`、`RightOnly`（非冲突单侧改动）、`BothSame`、`Conflict`；
  - 冲突合并视图提供 **「Apply Non-Conflicting Changes」** 快捷键与按钮（一键采纳全部 `LeftOnly` / `RightOnly` / `BothSame` 块），并支持按 `b` 展开/折叠当前冲突块的 **Base 祖先对照区**。
- [x] **Result 栏精细编辑能力**：
  - `ConflictChoice` 新增 `Neither`（丢弃双方改动）与 `Custom(Vec<String>)`（用户内联编辑后的内容）；
  - 支持单块重置回 `Unresolved`（`u` 键），支持对当前冲突块按 `e` 进入轻量内联多行编辑模式修改 Result 文本（保留语法高亮与水平滚动）；
  - 为三栏冲突视图接入可选语法高亮与块内词级差异提示。
- [x] **多文件冲突队列视图（`ModalLayer::ConflictQueue`）**：
  - 点击 Ongoing 横幅的 `[Resolve Conflicts]` 或快捷键触发时，若存在 ≥ 2 个未解决冲突文件，打开多文件冲突队列模态（单文件时可直接进合并器或按偏好打开队列）：
    - 表格列：`Path`、`Yours (<branch>)` 状态、`Theirs (<branch>)` 状态、`Status`（Unresolved / Resolved）；
    - 支持单选/多选批量 `Accept Yours` / `Accept Theirs`、`Merge…` 打开三栏合并器、`Open External Mergetool`、以及对已误解文件执行 **Revert Resolved (`git checkout -m -- <path>`)**（新增 `GitOp::UnresolveConflict`）；
    - 在三栏合并器中 `Save & Stage` 完成后自动前进到队列中的下一个未解决文件；当全部冲突文件均已解决时，弹出确认提示一键执行 `Ongoing::Continue`（或切 Commit 面板提交）。

## 3. 测试

- **单元测试**：3-way 块归并纯函数测试（纯左侧改、纯右侧改、双方相同改、真冲突、无 Base 的 Add/Add、Custom 编辑与重置为 Unresolved）。
- **引擎集成测试**：`GitOp::UnresolveConflict`（`git checkout -m`）将已暂存的合并文件重新恢复为冲突态。
- **TUI 会话测试**：构造 2 个冲突文件的仓库 → 打开 ConflictQueue → 第 1 个文件用「Apply Non-Conflicting」+ 选边保存 → 自动跳第 2 个文件 → 解决后弹出 Continue 提示。
