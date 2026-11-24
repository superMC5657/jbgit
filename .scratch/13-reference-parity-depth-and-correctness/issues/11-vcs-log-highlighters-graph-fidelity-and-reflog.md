# 11: VCS Log 高亮器、LinearBek / 图谱保真、No-Merges、表内速查与 Reflog 视图

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M10 — reference 深度对齐与正确性加固
Priority: P2
Blocked by: 03

- **参考源码**：
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/highlighters/{VcsLogCommitsHighlighter.java,CurrentBranchHighlighter.java}`
  - `reference/plugins/git4idea/backend/src/branch/{CherryPickedCommitsHighlighter.kt,DeepComparator.kt}`
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/linearBek/{LinearBekController.kt,LinearBekGraph.java,LinearBekGraphBuilder.java}`
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/facade/sort/bek/{BekSorter.java,BekBranchCreator.java,BekBranchMerger.java}`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/filter/{NoMergesFilterAction.kt,BranchFilterModel.kt}`、`table/VcsLogSpeedSearch.java`、`actions/GoToParentOrChildAction.kt`
- **关联文件**：
  - `crates/jbgit-core/src/graph/{visible.rs,collapse.rs,permanent.rs}`、`crates/jbgit-core/src/filter.rs`
  - `crates/jbgit-engine/src/details/`（新增 `cherry_picked.rs`、`reflog.rs`）、`crates/jbgit-engine/src/supervisor.rs`
  - `crates/jbgit-tui/src/log_view/{workbench.rs,filter_bar.rs,presentation.rs}`、`crates/jbgit-tui/src/views/{log.rs,log_details/commit_details.rs}`

---

## 1. 背景与目标

`jbgit` 的 VCS Log 已实现永久图、非交叉投影与四维筛选，但距离 IntelliJ `vcs-log` 的日常辨识度与信息密度还差最后几块招牌拼图：`My Commits` 与 `Already Cherry-Picked` 高亮器、`LinearBek` 折叠合并提交、`No-Merges` 筛选、表内不隐藏行的 Speed Search、多父/多子跳转选择器、列显隐定制以及 **Reflog** 恢复救命视图。

## 2. 任务清单

- [x] **高亮器补齐与自抑制**：
  - **My Commits**：加粗/高亮由当前 Git 用户（`RepoSnapshot::{user_name, user_email}`）提交的行；当仓库仅单一作者或已启用 `UserFilter(@me)` 时自动抑制，可由展示菜单开关；
  - **Already Cherry-Picked**：当按单分支筛选或对比分支时，异步在独立 `RequestSlot` 运行 `git cherry <current_head> <branch>`，将 patch-id 已存在于当前分支的提交（`- <oid>`）以暗淡样式标识；
  - **Current Branch 高亮自抑制**：当 Branch 筛选已限定为纯 `HEAD` 或当前分支本身时自动抑制整表底色染色（对齐 `CurrentBranchHighlighter.java:49-56`）。
- [x] **图谱保真度与 `LinearBek`**：
  - 修复 `FirstParent` 与 `BranchFilter` 叠加时的起始节点范围（仅从可见分支头开始沿第一父节点回溯，避免侧支标签泄漏）；
  - 实现 `LinearBek` 视图模式（`GraphViewOptions::linear_bek`）：将合并边折叠为从合入分支尾端指向合并第一父节点的虚边，支持单击/回车展开单个合并节点与全局折叠/展开；
  - 放宽线性折叠限制：在仅激活 `BranchFilter` 时允许折叠，且悬停已展开可折叠段时高亮整段；
  - `Go to Parent` / `Go to Child` 在目标数 ≥ 2 时弹出编号候选菜单（`hash "subject" author date`）而非硬跳第 0 个。
- [x] **筛选、表格与详情体验增强**：
  - 新增 `No-Merges`（`--no-merges`）过滤开关及芯片；
  - 增强 `Branch` 筛选芯片：支持直接输入 `a..b` 范围语法、内置 `Favorites` 快捷过滤组、以及各筛选字段最近 10 条历史记录；
  - 增强 `Hash` 筛选：支持空格分隔多个哈希同时命中；
  - Log 表格支持 **表内 Speed Search**（不隐藏非匹配提交，仅高亮并在匹配行间 `n`/`N` 跳行）；
  - `LogPresentation` 支持独立开关 `Author` 与 `Date` 列、支持 `Align Labels`（Ref Pill 居左或居右对齐）、以及 Details 面板的「最大化提交信息」弹窗和正文内 7–40 位 commit hash 可点击跳转。
- [x] **Reflog 视图（`ModalLayer::Reflog`）**：
  - 引擎新增可取消只读查询 `load_reflog_sync`（`git reflog show --format=... -n <limit> [HEAD|<branch>]`），支持分页；
  - TUI 新增 Reflog 视图（入口：Log 菜单 / 分支菜单 / `CommandCatalog`），展示 `HEAD@{n}`、操作类型（`commit` / `rebase` / `reset` / `checkout`）、短哈希、时间与消息，右侧联动 Diff，支持对任意 reflog 条目一键执行 **Checkout / Create Branch / Cherry-Pick / Reset to Here**（误操作 `reset --hard` 后的救命闭环）。

## 3. 测试

- **单元测试**：`LinearBek` 图变换与展开还原测试；`FirstParent` + BranchFilter 不泄漏侧支测试；`git cherry` 与 `git reflog` 解析测试；多哈希解析与 `a..b` 范围解析测试。
- **引擎集成测试**：在临时仓库 cherry-pick 某提交到 `main` 后，`load_cherry_picked_sync` 准确标记源分支提交为已拣选；执行 `reset --hard HEAD~1` 后 `load_reflog_sync` 能找回被丢弃的提交 OID。
- **TUI 会话测试**：打开 Reflog 选中历史条目并创建恢复分支；在合并提交上触发 `Go to Parent` 弹出双父选择框并跳转到第 2 父提交。
