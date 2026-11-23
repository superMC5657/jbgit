# 11: 多仓库（multi-root）模型取舍

Status: wontfix
Type: grilling
Spec: [../spec.md](../spec.md)
Milestone: M9 — reference 缺口补齐
Priority: P2
Blocked by: 09

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/repo/GitRepositoryManager.java`
  - `reference/plugins/git4idea/backend/src/branch/`（`GitBranchWorker`、多仓库同步分支操作）
  - `DvcsSyncSettings`（"Execute branch operations on all roots"）
- **关联文件**：`crates/jbgit-tui/src/session.rs`、`crates/jbgit-engine/src/supervisor.rs`、`crates/jbgit-core/src/repo.rs`

---

## 1. 问题

reference 几乎所有操作都假设可能有多个仓库根（同步分支操作、合并 Log、按仓库分组的变更树）。`jbgit` 是单仓库架构。在 Issue 09 提供"快速切换"后，是否还需要真正的多根模型？

## 2. 需要回答的问题

1. **目标用户场景**：monorepo + 子模块？多个独立服务仓库同时开发？这类用户在终端中是否更倾向于多开 `jbgit` 实例（终端分屏 / tmux）？
2. **最小可行形态**：
   - **A. 单活动仓库 + 快速切换**（Issue 09 即可达成）：成本最低，与现有架构一致。
   - **B. 只读聚合视图**：一个"仓库总览"页列出各仓库的分支、ahead/behind、脏状态，选择后切换为活动仓库。
   - **C. IDEA 式同步多根**：多 `EngineSupervisor` 并行、合并 Log（按仓库着色）、分支操作可同步到所有仓库、Stage 树按仓库分组。成本极高，会触及 `AppState` 的几乎所有工作台。
3. **若选 C**：`RepoSnapshot` 是否变为 `Vec`？`GitOp` 是否需要 `repo: RepoId`？Console 如何区分来源？合并 Log 的图谱如何布局？
4. **子模块**：是否作为"多根"的一个特例处理（父仓库 Stage 树中进入子模块）？

## 3. 产出

在本文件追加 `## Answer`，记录选定方案与理由；若选 B 或 C，拆出新的 Spec（`13-…`）及其工单，并在 `docs/adr/` 记录 ADR。

## Answer

**决定（2026-11-23，维护者拍板）：选 A — 单活动仓库 + 快速切换。** 本工单以 `wontfix` 关闭，不再追加多根实现。

- 多仓库诉求由 Issue 09 的 `RepoSwitcher`（最近仓库 / linked worktrees / 子模块）满足；同时查看多个仓库的场景交给终端多开或分屏。
- 架构约束随之确定：`RepoSnapshot`、`GitOp`、Console 保持单仓库语义，**不**引入 `RepoId`；新功能不得假设同时存在多个活动仓库。
- 子模块不作为"多根"处理：在父仓库中只读标注（Issue 14），需要操作时通过切换器进入子模块。
- 若未来出现真实需求（如 monorepo + 大量子模块的批量操作），重新开工单从方案 B（只读总览）评估，而不是直接跳到 C。
