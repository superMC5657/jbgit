# 12: 提交图线性分支折叠 / 展开

Status: ready-for-agent
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

- [ ] 在 `jbgit-core/src/graph/` 新增 `collapse.rs`：基于永久图识别线性片段（入度 = 出度 = 1 的连续节点，且不含带 ref 的提交、HEAD 及当前选中提交），维护 `collapsed: HashSet<SegmentId>`；`VisibleGraph::build` 接受折叠集，被折叠片段以 `GraphEdgeType::Dotted` 的单条虚边连接两端。**永久图不变**，折叠只影响可见投影与行号映射。
- [ ] `GraphViewOptions` 增加 `collapse: CollapseMode { None, All, Custom }`；折叠 / 展开后保持当前选中提交可见（若被折叠，自动展开所在片段）。
- [ ] 交互：点击虚边或节点上的折叠箭头（复用 Spec 02 Issue 03 的命中测试）切换单个片段；Presentation 菜单 "Collapse All Linear Branches" / "Expand All"；键位 `zc` / `zo` / `zM` / `zR`（vim 预设）与 `Ctrl-Shift-Minus/Plus`（jetbrains 预设）。
- [ ] 筛选激活时与 `first_parent_only` 模式下禁用折叠（对齐 IDEA 行为），菜单项显示原因。
- [ ] Cell 与 Vector 两种渲染后端都要绘制虚边与折叠标记。

## 3. 测试

- 纯算法单元测试：简单链、菱形合并、带 ref 的中间节点不可折叠、章鱼合并。
- 折叠后行号映射与 `perm_id ↔ visible_row` 双向一致；选中提交始终可见。
- 50k 提交基准：全部折叠 / 展开的投影重建 < 50ms（沿用 Spec 10 的基准方法）。
