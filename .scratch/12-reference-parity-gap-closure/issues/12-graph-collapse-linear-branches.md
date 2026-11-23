# 12: 提交图线性分支折叠 / 展开

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M9 — reference 缺口补齐
Priority: P2

- **参考源码**：
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/collapsing/CollapsedGraph.java`（及同目录其他文件）
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/facade/LinearGraphController.kt`
  - `reference/platform/vcs-log/graph-api/src/com/intellij/vcs/log/graph/actions/GraphAction.kt`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/actions/{CollapseGraphAction,ExpandGraphAction,CollapseOrExpandGraphAction}.java`
- **关联文件**：
  - `crates/jbgit-core/src/graph/{permanent,visible,print_elements,edges_in_row}.rs`（`GraphViewOptions`、`VisibleLinearGraph`）
  - `crates/jbgit-tui/src/log_view/{presentation,workbench}.rs`、`crates/jbgit-tui/src/graph/`

---

## 1. 背景与目标

在多分支并行的仓库中，IDEA 允许把"线性分支"（无分叉 / 合并的提交链）折叠为一条虚线边，只保留分叉点与合并点，快速看清拓扑。`jbgit` 的图谱算法已与 vcs-log 对齐，但缺少 `CollapsedGraph` 这一投影层。

## 2. 任务清单

- [x] 在 `jbgit-core/src/graph/` 新增 `collapse.rs`：基于永久图识别线性片段（入度 = 出度 = 1 的连续节点，且不含带 ref 的提交、HEAD 及当前选中提交），维护 `collapsed: HashSet<SegmentId>`；`VisibleGraph::build` 接受折叠集，被折叠片段以 `GraphEdgeType::Dotted` 的单条虚边连接两端。**永久图不变**，折叠只影响可见投影与行号映射。
- [x] `GraphViewOptions` 增加 `collapse: CollapseMode { None, All, Custom }`；折叠 / 展开后保持当前选中提交可见（若被折叠，自动展开所在片段）。
- [x] 交互：点击虚边或节点上的折叠箭头（复用 Spec 02 Issue 03 的命中测试）切换单个片段；Presentation 菜单 "Collapse All Linear Branches" / "Expand All"；键位 `zc` / `zo` / `zM` / `zR`（vim 预设）与 `Ctrl-Shift-Minus/Plus`（jetbrains 预设）。
- [x] 筛选激活时与 `first_parent_only` 模式下禁用折叠（对齐 IDEA 行为），菜单项显示原因。
- [x] Cell 与 Vector 两种渲染后端都要绘制虚边与折叠标记。

## 3. 测试

- 纯算法单元测试：简单链、菱形合并、带 ref 的中间节点不可折叠、章鱼合并。
- 折叠后行号映射与 `perm_id ↔ visible_row` 双向一致；选中提交始终可见。
- 50k 提交基准：全部折叠 / 展开的投影重建 < 50ms（沿用 Spec 10 的基准方法）。

## Comments

- 2026-11-24 实现说明：
  - `jbgit-core::graph::collapse`：`LinearSegments::compute(linear, pinned)` 在 `O(n)` 内识别线性片段（中间节点入度 = 出度 = 1、父节点已加载、非 ref / HEAD；至少 2 个中间节点；两端已直接相连的片段不折叠以免与原边重复），`pinned_nodes` 由全部 ref 目标与 HEAD 生成。`SegmentId` 取片段最上方被隐藏提交的 oid，快照重载后仍稳定。`CollapseState { mode: CollapseMode { None, All, Custom }, ids }`：`All` 下 `ids` 为保持展开的例外、`Custom` 下为折叠集合，因此"全部折叠"会自动覆盖重载后新出现的片段。
  - `GraphViewOptions.collapse: CollapseMode`；`VisibleGraph::build_collapsed(.., Option<CollapseInput>)`（`build` 委托给它），新增 `GraphEdgeType::Collapsed`（渲染为虚线）与 `VisibleGraph.collapse_applied`。筛选或 `first_parent_only` 生效时跳过折叠。并行片段（菱形 / 章鱼合并的多臂）共用一条折叠边。永久图不变。
  - `GraphPrintElement::Node.collapsed_below` 作为折叠标记：Cell 后端绘制空心 `○`（HEAD 保留 HEAD 颜色），Vector 后端绘制空心圆环；`GraphHitKind::Node.collapsed_below` 让点击标记即展开。字符网格中相邻行间的虚边被节点覆盖，因此标记是主要的可视/可点提示；点击折叠虚边同样展开。
  - TUI：`LogGraphState.collapse` + 同步重投影（`rebuild_collapsed_now`，仅在缓存为当前未筛选投影时执行，否则交给常规重建）；`CachedLogGraph/CachedGraphBase.segments` 随永久图构建，异步 `LogGraphJob` 携带折叠状态。选中提交保持可见：全部折叠保留其所在片段；`zc` 折叠所在片段时光标移到片段上端；跳转（Go to Revision、父提交链接、导航前进 / 后退）与重载后落入新片段时自动展开。
  - 命令：`KeyAction::{CollapseLinearBranch, ExpandLinearBranch, CollapseAllLinearBranches, ExpandAllLinearBranches}` + `catalog::graph_*` 可用性（筛选 / First Parent Only 时给出原因）；键位 default / jetbrains `<C-S-->` / `<C-S-=>`（及 `<C-S-+>`），vim `z c` / `z o` / `z M` / `z R`；Presentation 菜单 `Collapse / Expand Linear Branches`（不显示快捷键列，以免级联菜单超出父菜单左侧空间），提交菜单 `Collapse / Expand Linear Branch`。中英 i18n、手册第六章。
  - 测试：核心（简单链、单提交不成段、菱形单臂折叠且父序保留、ref 拆分片段、章鱼合并共边、两端直连不可折叠、筛选 / first-parent 跳过、状态转换、行号双向一致、50k 提交全部折叠 / 展开 < 50ms —— release 下整测 20ms）；Vector 圆环像素测试；`TuiSession`：全部折叠 / 展开与标记、选中片段保留、vim z 系列、点击标记 / 折叠边展开、禁用原因、跳转自动展开、重载后保持 All 模式。
