# 02: 实现三文档行状态追踪器 (`GitStageLineStatusTracker`) 与 Hunk/单行内存级局部暂存

Status: ready-for-agent
Blocked by: 01
Spec: [../spec.md](../spec.md)
Milestone: M4 — 变更暂存工作台与局部暂存

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/index/GitStageLineStatusTracker.kt` (`BlockMerger`, `StagedRange`, `runBulkStage`, `runBulkUnstage`, `runBulkRollback`)

---

## 1. 背景与目标

在 `jbgit-core::stage` 与 `jbgit-engine` 中完整落地 IDEA `GitStageLineStatusTracker` 的三文档双差分块合并算法与 `git2::Index::add_frombuffer` 纯内存写入机制，并在右侧交互式 Diff 面板中提供 **Hunk 级** 与 **单行 / 多行可视选区级（Visual Line Selection）** 的局部 Stage、Unstage 与 Rollback 能力，彻底杜绝手工拼 patch 容易失败或误改工作区文件的问题。

---

## 2. 核心实现任务清单

- [ ] **无损行切片文档与 `BlockMerger` 实现 (`jbgit-core::stage`)**：
  - 实现 `SlicedDocument`：将字节数组按行切分，精确记录每行的内容切片及行尾换行符类型（`\n`、`\r\n` 或文件尾无换行 `None`），确保重组回字节流时与原文件 100% 字节级一致（Byte-for-byte identical）。
  - 基于 `imara-diff` 计算 `staged_blocks = diff(vcs_doc, staged_doc)` 与 `unstaged_blocks = diff(staged_doc, local_doc)`。
  - 移植 `BlockMerger`（`GitStageLineStatusTracker.kt` 第 922-1018 行），输出统一坐标系的 `Vec<StagedRange>`。
- [ ] **内存级局部缓冲区合成与 `git2::Index` 落盘**：
  - **Stage Hunk / Stage Selected Lines**：将选中区间内的未暂存新增行并入 `staged_doc`、被选中删除行从 `staged_doc` 剔除、未选中变更保持 `staged_doc` 原状，合成 `new_staged_bytes`，调用 `git2::Index::add_frombuffer` + `index.write()`（保留可执行权限位 `0o100755` / `0o100644`）。
  - **Unstage Hunk / Unstage Selected Lines**：在 `vcs_doc ↔ staged_doc` 间逆向回退选中行至 `vcs_doc` 内容，同样经 `add_frombuffer` 更新 Index（若新增文件全部行被取消暂存且 `vcs_doc` 不存在，则自动降级为 Untracked 或从 Index 移除条目）。
  - **Rollback Unstaged Hunk / Lines**：将工作区 `local_doc` 中选中的未暂存块恢复为 `staged_doc` 对应文本并原子写回工作区文件，同时压入本地内存 `RollbackUndoStack`（支持 `u` / `Ctrl+Z` 撤销最近一次局部回滚防误删！）。
- [ ] **交互式 `StageDiffWorkbench` TUI 组件**：
  - 在每个 Hunk 标题行右侧渲染内联动作胶囊：`[+ Stage (s)]`、`[- Unstage (u)]`、`[↺ Rollback (r)]`（均注册 `HitTarget::DiffHunkAction` 支持鼠标直接点击！）。
  - 支持 `]c` / `[c`（或 `n` / `p`）在各个变更 Hunk 间快速跳跃，支持 `v` 开启逐行多选模式（高亮选中行范围，仅对选中行执行 `s` / `u` / `r`）。
  - 渲染左侧三态边栏指示条，并在混合变更块支持按 `i` 弹出 `HEAD / Staged / Worktree` 三方迷你对照浮窗。

---

## 3. 边界情况与不变量

- **CRLF / LF 混合换行符与无尾换行文件**：对不含末尾 `\n` 的文件暂存前几行后，文件末尾依然不得凭空多出或丢失换行符。
- **尚未加入 Index 的全新 Untracked 文件局部暂存**：若用户在 `Untracked` 新文件上仅选中其中 5 行执行局部 Stage，`vcs_doc` 与初始 `staged_doc` 均视为空文档，`add_frombuffer` 应在 Index 中新建仅含这 5 行的条目，而工作区保留剩余行作为未暂存新增！
- **可执行脚本权限保持**：对 `100755` 文件执行局部 Stage/Unstage 后，Index Entry 的 `mode` 必须保持 `0o100755`。

---

## 4. 验收标准 (Acceptance Criteria)

- [ ] 属性测试（`proptest`）：随机生成含多处修改、插入、删除及 CRLF 换行的 `vcs / staged / local` 三份文本，随机挑选任意子集行执行 Stage 再全部 Stage，最终 `staged_bytes == local_bytes`；执行全部 Unstage 后 `staged_bytes == vcs_bytes`，且全过程 `local_doc` 未发生意外变更。
- [ ] 集成测试：验证在一个含 3 个 Hunk 的修改文件中，单独暂存第 2 个 Hunk 中的其中 2 行后，运行原生 `git diff --cached` 与 `git diff` 输出完全符合预期且无损坏。

---

## Comments
