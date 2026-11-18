# 02: 实现 `EdgesInRowGenerator`、`GraphElementComparatorByLayoutIndex` 非交叉行元素生成与 Bek 排序

Status: ready-for-agent
Blocked by: 01
Spec: [../spec.md](../spec.md)
Milestone: M2 — 非交叉 DAG 图与 Pill 徽标

- **参考源码**：
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/print/EdgesInRowGenerator.java`
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/print/GraphElementComparatorByLayoutIndex.java`
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/print/PrintElementGeneratorImpl.kt`
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/facade/PrintElementPresentationManagerImpl.kt`
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/facade/sort/bek/BekSorter.java`

---

## 1. 背景与目标

在 `PermanentGraphLayout` 之上构建完整的 `VisibleGraph` 管线：支持拓扑/IntelliSort(Bek)/FirstParent/过滤子图投影，实现双向分块缓存的 `EdgesInRowGenerator` 与严格非交叉的 `GraphElementComparatorByLayoutIndex` 行槽位生成器，并支持长边自动折叠与精确分叉边颜色归属。

---

## 2. 核心实现任务清单

- [ ] **可见图控制器 (`VisibleGraphController`)**：
  - 实现 `BaseController(SortType::Topo)` 与 `BekSorter(SortType::IntelliSort)`（在满足拓扑偏序前提下按提交时间戳交错排布并发分支）。
  - 实现 `FirstParentController`（仅保留第一父路径）与 `BranchFilter / MatchingCommits FilteredController`（对分支过滤或搜索命中集构建压缩可见子图并维护 `VisibleRowIdx <-> PermNodeId` 双向映射）。
- [ ] **双向分块缓存活跃边扫描器 (`EdgesInRowGenerator`)**：
  - 完整移植 `EdgesInRowGenerator.java`：`BLOCK_SIZE = 40`，`WALK_SIZE = 1000`，维护上下双向 LRU 块快照缓存 `cache_nu` 与 `cache_nd`，实现 `one_down_step` 与 `one_up_step`，确保在 10 万行任意位置随机拖动滚动条时单行活跃边查询耗时 < 50μs。
- [ ] **非交叉行元素比较器 (`GraphElementComparatorByLayoutIndex`)**：
  - 逐分支精确移植 `GraphElementComparatorByLayoutIndex.java`，通过 `max(layout_index(up), layout_index(down))` 与 `up - node_row` 平局决胜规则，保证其满足 Rust `Ord` 所需的严格弱序（反对称性、传递性、自反性）。
- [ ] **`PrintElementGenerator` 与长边折叠**：
  - 实现 `show_long_edges: bool` 切换：关闭时 `LONG_EDGE_SIZE = 30, VISIBLE_PART_SIZE = 1`；开启时 `VERY_LONG_EDGE_SIZE = 1000, VERY_LONG_EDGE_PART_SIZE = 250`。
  - 对被截断的长边在上下保留端生成带 `has_arrow = true` 的 `EdgePrintElement` / `TerminalEdgePrintElement`（记录对端目标 `PermNodeId` 以备点击跳转）。
  - 实现 `recommended_width()` 加权方差算法（`SAMPLE_SIZE = 20000, K = 0.1`），为提交表格提供稳定不抖动的推荐轨道列宽。
- [ ] **边颜色归属计算 (`PrintElementPresentationManager`)**：
  - 严格实现 `if up_layout_idx >= down_layout_idx { color(up) } else { color(down) }`，确保所有分叉与合入斜线恒归属子分支颜色。

---

## 3. 边界情况与不变量

- **严格全序验证（反 Panic 保证）**：Rust 的 `slice::sort_by` 若遇到违反传递性的比较器可能触发异常排序结果甚至 panic！必须编写穷举三元组 `(a, b, c)` 测试验证 `compare_elements` 在同起点多边、同终点多边及特殊虚线边下的严格传递性。
- **跨度 > 1000 行的超长边**：验证在 `show_long_edges = true` 时结合向上/向下扫描或全局长边补充索引不会在中间段丢失可见长边。

---

## 4. 验收标准 (Acceptance Criteria)

- [ ] 单元测试覆盖：经典分叉合流、双分支交错合流（Criss-cross Merge）、章鱼合并、跨 50 行长边截断箭头生成（第 `up+1` 行生成 `DOWN` 箭头，第 `down-1` 行生成 `UP` 箭头，中间行隐藏该边释放列槽位）。
- [ ] 属性测试验证：对任意随机 DAG 的全部可见行，验证比较器满足全序传递律 `a < b && b < c => a < c`。
- [ ] 颜色断言：验证特性分支从 `main` 分出再合回 `main` 的上下两条连接边，`color_id` 均等于特性分支而非 `main`。

---

## Comments
