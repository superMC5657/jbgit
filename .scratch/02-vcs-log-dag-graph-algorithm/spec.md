# Spec 02: 提交图排版算法 — DFS 连续轨道与非交叉 DAG 提交树

- **目录**：`.scratch/02-vcs-log-dag-graph-algorithm/`
- **下属 Issues**：
  - [`01-permanent-graph-and-dfs-track-layout.md`](./issues/01-permanent-graph-and-dfs-track-layout.md)
  - [`02-non-crossing-print-elements-and-bek-sort.md`](./issues/02-non-crossing-print-elements-and-bek-sort.md)
  - [`03-tui-graph-cell-rasterizer-and-hit-testing.md`](./issues/03-tui-graph-cell-rasterizer-and-hit-testing.md)
  - [`04-kitty-sixel-vector-graph-backend.md`](./issues/04-kitty-sixel-vector-graph-backend.md)（可选高清矢量后端）
- **上游参考源码**：
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/permanent/PermanentLinearGraphBuilder.java`
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/permanent/GraphLayoutBuilder.kt`
  - `reference/plugins/git4idea/backend/src/log/GitRefManager.kt` (`GitBranchLayoutComparator`)
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/graph/HeadCommitsComparator.java`
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/print/EdgesInRowGenerator.java`
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/print/GraphElementComparatorByLayoutIndex.java`
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/print/PrintElementGeneratorImpl.kt`
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/impl/facade/PrintElementPresentationManagerImpl.kt`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/paint/PaintParameters.java`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/paint/SimpleGraphCellPainter.kt`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/paint/HeadNodePainter.kt`
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/linearBek/LinearBekGraphBuilder.java`

---

## 1. 为什么传统终端 Git 图容易乱？IDEA `vcs-log` 的本质突破

常见的 CLI `git log --graph` 采用贪心单遍扫描：遇到 merge 节点就向右扩张新列，合流后再向左坍缩，导致主干分支反复左右蛇形扭曲、长跨度分支横穿数十列制造大量交叉线。

IntelliJ IDEA `platform/vcs-log/graph` 通过 **两阶段解耦架构** 彻底解决了这一问题：
1. **全局静态拓扑轨道规划（`PermanentGraph` + `GraphLayoutBuilder`）**：先按分支战略优先级排序所有图头（`origin/main` 最高优），再做迭代式 DFS 为每个节点赋予单调递增的永久轨道序号 `layout_index: u32` —— **同一条连续历史路径拥有完全相同的 `layout_index`**，主干永远锁定 `layout_index = 1`（第 0 列垂直贯通）。
2. **局部按需非交叉行投影（`EdgesInRowGenerator` + `GraphElementComparatorByLayoutIndex`）**：仅对当前视口可见行计算穿过该行的活跃边集合，并用基于 `max(layout_index(up), layout_index(down))` 的严格全序比较器对行内节点与边排序，从而在数学上保证：**同一轨道在相邻行中列号连续稳定，不同 `layout_index` 轨道保持左右相对次序不变，最大限度消除无谓交叉**。

---

## 2. 核心数据结构设计 (`jgit-core::graph`)

```rust
/// 永久图中的密集节点下标 (0 .. nodes_count)，0 为最新提交
pub type PermNodeId = u32;
/// 可见图（经过分支过滤/搜索/折叠后）的行下标 (0 .. visible_count)
pub type VisibleRowIdx = u32;
/// 连续轨道编号（从 1 开始单调递增，1 代表最左侧主干轨道）
pub type LayoutIndex = u32;

/// 紧凑 CSR 结构的静态永久图（对标 PermanentLinearGraphImpl）
pub struct PermanentLinearGraph {
    pub nodes_count: u32,
    /// 每个节点对应的 Git Oid 与提交时间戳
    pub commits: Vec<CommitMetaLite>,
    /// Oid -> PermNodeId 哈希加速表
    pub oid_to_node: rustc_hash::FxHashMap<git2::Oid, PermNodeId>,
    /// CSR 向下边（父节点方向）偏移与目标数组；负值代表尚未加载的边界父提交
    pub down_offsets: Vec<u32>,
    pub down_targets: Vec<i32>,
    /// CSR 向上边（子节点方向）偏移与目标数组
    pub up_offsets: Vec<u32>,
    pub up_targets: Vec<PermNodeId>,
}

/// DFS 连续轨道布局结果（对标 GraphLayoutImpl）
pub struct PermanentGraphLayout {
    /// 长度 = nodes_count，记录每个节点的连续轨道编号 (>= 1)
    pub layout_index: Vec<LayoutIndex>,
    /// 按战略重要性排好序的主导分支头节点列表
    pub head_node_ids: Vec<PermNodeId>,
    /// 记录每个节点归属的主导头节点下标，用于稳定计算轨道颜色
    pub head_of_node: Vec<PermNodeId>,
}
```

---

## 3. 阶段一：头节点优先级与 DFS 连续轨道分配

### 3.1 头节点比较器（对标 `HeadCommitsComparator` + `GitBranchLayoutComparator`）

收集两类候选起点：`branches_node_ids`（所有本地分支、远程分支、标签指向的节点）与 `graph_heads`（入度为 0、即 `up_targets` 为空的拓扑顶端节点），去重后按以下全序排序：

1. **有引用的头节点排在无引用（游离悬空）头节点之前**；
2. 若均有引用，取各节点上优先级最高的引用，按 **`GitBranchLayoutComparator`** 比较权重（权重越小越靠左）：
   ```text
   0: origin/main | origin/master   (ORIGIN_MASTER — 最高优，锁定最左直轨 layout_index = 1)
   1: 其他远程分支 origin/*          (REMOTE_BRANCH)
   2: main | master                 (MASTER)
   3: 其他本地分支                   (LOCAL_BRANCH)
   4: 标签                          (TAG)
   5: 当前检出分支                   (CURRENT_BRANCH)
   6: HEAD                          (HEAD)
   7: 其他引用                       (OTHER)
   ```
   > **源码级关键洞察**（参见 `GitRefManager.kt` 第 219-230 行）：为什么 `ORIGIN_MASTER` / `MASTER` 排在 `CURRENT_BRANCH` 前面？因为这样无论用户当前检出到哪个短期 `feature/*` 分支，`main` 主干始终稳居最左侧第 0 列笔直向下，而 `feature/*` 分支总是优雅地从主干右侧分叉与汇合，绝不会因 `git checkout` 导致整张提交图剧烈翻转！
3. 同权重下按 Git 引用名字典序比较；若仍相同，按 `PermNodeId` 升序比较。

### 3.2 迭代式无栈溢出 DFS 轨道分配（对标 `GraphLayoutBuilder.kt` 第 68-104 行）

严禁使用递归 DFS（10 万级线性提交会触发线程栈溢出），必须使用显式栈实现与 `Dfs.walk` 完全等价的后序递增逻辑：

```rust
pub fn build_graph_layout(
    graph: &PermanentLinearGraph,
    sorted_heads: &[PermNodeId],
) -> PermanentGraphLayout {
    let n = graph.nodes_count as usize;
    let mut layout_index = vec![0u32; n];
    let mut head_of_node = vec![0u32; n];
    let mut important_heads = Vec::new();
    let mut current_layout_index: LayoutIndex = 1;
    let mut stack: Vec<PermNodeId> = Vec::with_capacity(256);

    for &head in sorted_heads {
        if layout_index[head as usize] != 0 {
            continue;
        }
        important_heads.push(head);
        stack.push(head);

        while let Some(&curr) = stack.last() {
            let curr_idx = curr as usize;
            let first_visit = layout_index[curr_idx] == 0;
            if first_visit {
                layout_index[curr_idx] = current_layout_index;
                head_of_node[curr_idx] = head;
            }

            // 按父节点顺序（第一父提交 First-Parent 优先！）寻找首个尚未分配 layout_index 的父节点
            let next_unvisited_parent = graph
                .down_nodes(curr)
                .iter()
                .copied()
                .filter(|&p| p >= 0)
                .map(|p| p as PermNodeId)
                .find(|&p| layout_index[p as usize] == 0);

            match next_unvisited_parent {
                Some(child) => {
                    stack.push(child);
                }
                None => {
                    // 当前连续链条走到底（遇到根提交或撞上已被更高优分支占领的公共祖先）
                    // 若本次是首次给链尾节点赋号，则递增轨道号，供回溯后的次级父分支（Merge 第二父节点）使用新轨道号！
                    if layout_index[curr_idx] == current_layout_index {
                        current_layout_index += 1;
                    }
                    stack.pop();
                }
            }
        }
    }

    PermanentGraphLayout {
        layout_index,
        head_node_ids: important_heads,
        head_of_node,
    }
}
```
- **时间复杂度**：每个节点和每条边最多被扫描常数次，严格 **$O(V + E)$**。
- **First-Parent 保直特性**：由于 `down_nodes(curr)` 中 `parent[0]`（Merge 提交的目标主分支父节点）排在 `parent[1..]`（被合入的特性分支父节点）之前，DFS 总是沿着 `parent[0]` 保持**同一个 `current_layout_index` 笔直向下**，而 `parent[1]` 在回溯时自动获得 `current_layout_index + 1` 向右侧展开！

---

## 4. 阶段二：排序控制器与可见图（Bek / FirstParent / LinearBek / Filtered）

对标 `PermanentGraphImpl.createFilteredController`（第 56-90 行），支持 4 种视图模式：

1. **`SortType::Topo` (`Normal`)**：标准拓扑逆序（子提交恒在父提交上方）。
2. **`SortType::IntelliSort` (`BekSorter`)**：在严格保持父子拓扑约束的前提下，利用优先队列按提交时间戳交错排布不相关的并发分支，避免久远未合并分支沉底或置顶失真。
3. **`FirstParent` 模式**：仅沿各可见分支头节点的 `parent[0]` 投影主线演进，隐藏合入分支内部细碎提交。
4. **`LinearBek` 模式（可选高级折叠）**：对标 `LinearBekGraphBuilder`，将无冲突的局部短生命周期合并分支线性化折叠，并以虚线箭头（`GraphEdgeType::DottedArrowUp/Down`）标识折叠关系，点击箭头可原位展开/收起。
5. **`FilteredController`（多维筛选子图）**：当按关键词/作者/路径过滤出匹配提交子集 `matching_nodes` 时，保留匹配节点并压缩中间未匹配路径为连接边或虚线边，且复用永久图的 `layout_index` 保证筛选前后分支颜色与相对左右位置一致。

---

## 5. 阶段三：非交叉行元素生成与长边折叠

### 5.1 双向分块缓存活跃边扫描器（对标 `EdgesInRowGenerator.java`）

给定可见行 `r`，需要找出所有满足 `edge.up <= r && r < edge.down`（或连接 `r`）的跨行活跃边。若每次从第 0 行扫描是 $O(N)$，无法支撑 10 万行即时跳转。因此完全复刻 `EdgesInRowGenerator` 的双缓存步进机制：

- 常量：`BLOCK_SIZE = 40`，`WALK_SIZE = 1000`，LRU 块缓存容量 `CACHE_SIZE = 20`。
- **向下扫描步 `one_down_step(row)`**：以 `up_anchor = (r / 40) * 40` 为锚点（若未命中缓存则从 `max(0, up_anchor - 1000)` 步进构建），每向下一行：
  - 加入 `row` 发出的 `NORMAL_DOWN` 边；
  - 移除抵达 `row + 1` 的 `NORMAL_UP` 边。
- **向上扫描步 `one_up_step(row)`**：以 `down_anchor = up_anchor + 40` 为锚点（从 `min(last, down_anchor + 1000)` 向上步进），捕获跨越超过 1000 行、但下端点就在附近的超长边！
- 合并两个集合：`active_edges(r) = neighbor_u(r) ∪ neighbor_d(r)`。单行查询均摊复杂度 **$O(\text{活跃分支宽度})$**。

### 5.2 长边折叠与箭头截断（对标 `PrintElementGeneratorImpl.kt` 第 214-245、277-281 行）

为防止一条跨越 5000 行的古老分支一直占据屏幕宝贵列宽：
- **默认折叠模式（`show_long_edges = false`）**：
  - `LONG_EDGE_SIZE = 30`，`VISIBLE_PART_SIZE = 1`。
  - 当一条正常边跨度 `down - up >= 30` 时，仅在距离上端点 `≤ 1` 行和距离下端点 `≤ 1` 行处可见，并在截断处生成带向下/向上箭头的 `TerminalEdgePrintElement`（TUI 渲染为带分支色的 `▾` / `▴` 短桩，点击该箭头可直接跳转到对端提交！），中间 28+ 行完全释放该列宽度！
- **展开长边模式（`show_long_edges = true`）**：
  - `VERY_LONG_EDGE_SIZE = 1000`，`VERY_LONG_EDGE_PART_SIZE = 250`。

### 5.3 非交叉全序比较器（对标 `GraphElementComparatorByLayoutIndex.java`）

对于第 `r` 行的所有可见元素（包含该行唯一的 `GraphNode(r)` 及所有穿过该行的可见 `GraphEdge`），按以下严格全序排序得到每个元素在该行的横向槽位下标 `position: usize`（`0, 1, 2, ...`）：

```rust
fn compare_edge_and_node(
    edge: NormalEdge, // up < down
    node_row: VisibleRowIdx,
    layout_idx_of: impl Fn(VisibleRowIdx) -> LayoutIndex,
) -> Ordering {
    let edge_li = layout_idx_of(edge.up).max(layout_idx_of(edge.down));
    let node_li = layout_idx_of(node_row);
    if edge_li != node_li {
        edge_li.cmp(&node_li)
    } else {
        edge.up.cmp(&node_row)
    }
}

fn compare_elements(
    a: &GraphElement,
    b: &GraphElement,
    layout_idx_of: impl Fn(VisibleRowIdx) -> LayoutIndex,
) -> Ordering {
    match (a, b) {
        (GraphElement::Node(_), GraphElement::Node(_)) => Ordering::Equal,
        (GraphElement::Edge(e), GraphElement::Node(n)) => compare_edge_and_node(*e, *n, layout_idx_of),
        (GraphElement::Node(n), GraphElement::Edge(e)) => compare_edge_and_node(*e, *n, layout_idx_of).reverse(),
        (GraphElement::Edge(e1), GraphElement::Edge(e2)) => {
            if e1.up == e2.up {
                if e1.down < e2.down {
                    compare_edge_and_node(*e2, e1.down, layout_idx_of).reverse()
                } else {
                    compare_edge_and_node(*e1, e2.down, layout_idx_of)
                }
            } else if e1.up < e2.up {
                compare_edge_and_node(*e1, e2.up, layout_idx_of)
            } else {
                compare_edge_and_node(*e2, e1.up, layout_idx_of).reverse()
            }
        }
    }
}
```

### 5.4 分叉与合并边的颜色归属铁律（对标 `PrintElementPresentationManagerImpl.kt` 第 50-58 行）

对于任意连接上端点 `up` 与下端点 `down` 的边 `e`：
```rust
let up_li = layout_index(up_perm_id);
let down_li = layout_index(down_perm_id);
let owner_perm_id = if up_li >= down_li { up_perm_id } else { down_perm_id };
let edge_color = color_getter.node_color(owner_perm_id);
```
- **效果验证**：
  - **Branch Fork（分叉）**：子提交在 `feature`（`up_li = 3`），父提交在 `main`（`down_li = 1`） -> `up_li >= down_li` -> 斜向分叉线取 **`feature` 的颜色**！
  - **Branch Merge（合入主干）**：Merge 提交在 `main`（`up_li = 1`），第二父提交在 `feature`（`down_li = 3`） -> `down_li > up_li` -> 斜向合入线同样取 **`feature` 的颜色**！
  - 主干 `main` 的颜色自始至终只染第 0 列的垂直主轴，视觉边界干净利落！

---

## 6. 阶段四（双后端抽象与默认字符后端）：`GraphRenderBackend` 与字符矩阵光栅化器 (`CellGridGraphBackend`)

为了同时兼顾 **100% 终端普适性** 与 **现代图形终端的 GUI 级抗锯齿矢量表现力**，`jgit-tui` 将 `jgit-core` 输出的抽象几何图元（`GraphPrintElement`）与具体终端绘制解耦为统一的可插拔接口 `GraphRenderBackend`：

```rust
pub struct GraphViewportSlice<'a> {
    /// 当前视口可见行的图元切片（长度 = area.height）
    pub rows: &'a [RowGraphSlice<'a>],
    /// 分配给图形轨道区的终端矩形区域（宽 = 2 * max_tracks 列，高 = visible_rows 行）
    pub area: ratatui::layout::Rect,
    /// 当前帧是否存在悬浮在 area 之上的 Layer 2+ 弹窗/右键菜单遮挡区域
    pub occluding_rects: &'a [ratatui::layout::Rect],
}

pub struct RowGraphSlice<'a> {
    pub visible_row: VisibleRowIdx,
    pub row_bg: ratatui::style::Color,
    pub elements: &'a [GraphPrintElement],
}

pub trait GraphRenderBackend {
    /// 渲染当前视口所有可见行的分支轨道图，并向 HitTestRegistry 注册节点与箭头热区
    fn render_viewport(
        &mut self,
        slice: &GraphViewportSlice<'_>,
        buf: &mut ratatui::buffer::Buffer,
        ctx: &mut RenderContext<'_>,
    );
}
```

系统内置两套实现（运行期可热切换或自动降级）：
1. **`CellGridGraphBackend`（默认主线，`03-tui-graph-cell-rasterizer-and-hit-testing`）**：纯字符微网格光栅化，兼容一切 SSH、tmux、Windows Terminal 与经典终端；
2. **`PixelVectorGraphBackend`（进阶可选扩展，`04-kitty-sixel-vector-graph-backend`，见第 7 节）**：基于 `tiny-skia` + Kitty / Sixel / iTerm2 终端图形协议直接复刻 `SimpleGraphCellPainter.kt` 亚像素抗锯齿矢量曲线与双环靶心圆点。

以下 6.1 ~ 6.4 规约默认主线 **`CellGridGraphBackend`**，将 `PrintElementGenerator` 输出的向量几何语义（`NodePrintElement` 在列 `pos`；`EdgePrintElement` 从本行 `from_pos` 连往相邻行 `to_pos`，方向 `UP | DOWN`，线型 `SOLID | DASHED`）无损光栅化到终端单行高度 `ratatui::buffer::Buffer`：

### 6.1 槽位列宽与微网格模型
- 每个轨道槽位 `pos` 占用 **2 个终端字符列**：
  - **偶数列 `2 * pos`（轨道主轴列）**：放置提交圆点 `●` / `◎`、垂直干线 `│` / `┆`、弯折角 `╭` `╮` `╰` `╯`、三通/四通交汇 `├` `┤` `┼`、长边终端箭头 `▾` `▴`。
  - **奇数列 `2 * pos + 1`（槽间桥接列）**：放置横向跨列连线 `─` / `╌` 或留空 `' '`。
- 图区域总列宽自适应当前视口（结合 `PrintElementGeneratorImpl.recommendedWidth` 加权标准差公式 `round(avg + std_dev)` 设置平滑最小列宽，防止滚动时右侧提交文字频繁左右抖动！）。

### 6.2 节点与边字符映射表

| 元素类型 | 场景条件 | Unicode 字符 | 着色与叠加优先级 |
| :--- | :--- | :---: | :--- |
| **普通提交节点** | 非 HEAD 普通提交 (`FILL`) | `●` (`U+25CF`) | 节点所属轨道色（最高绘制优先级，覆盖底层边） |
| **Merge 提交节点** | 父提交数 ≥ 2 | `●` 或 `◉` (`U+25C9`) | 节点所属轨道色 |
| **HEAD 提交节点** | 当前检出提交 (`OUTLINE_AND_FILL`) | `◎` (`U+25CE`) / `◈` | `#E1C731` 金色高亮或当前轨道加粗高亮 |
| **垂直贯通边** | `from == to`, `SOLID` | `│` (`U+2502`) | 该边所属轨道色 |
| **虚线过滤/折叠边** | `DASHED` | `┆` (`U+2506`) | 该边所属轨道色 |
| **长边截断箭头** | `TerminalEdge(DOWN / UP)` | `▾` / `▴` | 该边所属轨道色（注册 `HitTarget::GraphPrintElement` 支持点击跳转） |
| **向右分叉/合入** | 本行 `pos` 连往下一行 `to > pos` | 起点 `╭` / `├`，中间 `─`，目标上方 `╮` | 严格使用 `max(up_li, down_li)` 分支色逐格着色！ |
| **向左归并（Merge）**| 本行 `pos` 连往上一行 `to < pos` | 下方收口 `╯`，中间 `─`，主干交汇 `├` | 横线与弯角 `╯` 染子分支色，主干交汇 `├` 保持主干色！ |

### 6.3 字符位掩码合成器（Bitmask Glyph Composer）

每个字符格维护四向连接位掩码 `ConnMask { up: Option<Color>, down: Option<Color>, left: Option<Color>, right: Option<Color>, style: LineStyle }`：
1. 先按 `PrintElement` 顺序向本行各 `2 * pos` 及跨越的 `2 * k + 1` 单元格写入 `up / down / left / right` 连接及对应颜色；
2. 查 16 项查找表将 `(up.is_some(), down.is_some(), left.is_some(), right.is_some())` 精确合成为圆角盒线字符：
   - `(1, 1, 0, 0) -> '│'`
   - `(0, 0, 1, 1) -> '─'`
   - `(0, 1, 0, 1) -> '╭'`
   - `(0, 1, 1, 0) -> '╮'`
   - `(1, 0, 0, 1) -> '╰'`
   - `(1, 0, 1, 0) -> '╯'`
   - `(1, 1, 0, 1) -> '├'`
   - `(1, 1, 1, 0) -> '┤'`
   - `(0, 1, 1, 1) -> '┬'`
   - `(1, 0, 1, 1) -> '┴'`
   - `(1, 1, 1, 1) -> '┼'`
3. **单元格颜色仲裁规则**：当垂直主轨与横向分支线在同一格交汇为 `├` 或 `┼` 时，优先保留垂直贯通轨（`up == down`）的颜色，保证主干垂直视觉连贯不断裂；而紧邻右侧的奇数列 `─` 与弯头 `╮`/`╯` 显示分支自身颜色，在单字符精度下完美呈现多色分叉与汇合！

### 6.4 渲染效果示意

```text
Col: 0 1 2 3 4 5
Row0 ◎           ▐◆ main▌ release: v1.0.0
Row1 ├─╮         Merge pull request #42 from feat/ui
Row2 │ ●         ▐⎇ feat/ui▌ style: polish darcula pill colors
Row3 │ ├─╮       Merge branch 'feat/parser' into feat/ui
Row4 │ │ ●       feat(parser): support regex filter
Row5 │ ├─╯       feat(ui): add three-pane splitter
Row6 ├─╯         fix: memory index buffer race
Row7 ●           chore: init repository
```

---

## 7. 阶段四（进阶可选后端）：像素级抗锯齿矢量图形后端 (`PixelVectorGraphBackend`)

- **关联 Issue**：[`04-kitty-sixel-vector-graph-backend.md`](./issues/04-kitty-sixel-vector-graph-backend.md)
- **直接对标源码**：`PaintParameters.java`、`SimpleGraphCellPainter.kt`、`HeadNodePainter.kt`

在支持现代终端内联位图协议（**Kitty Graphics Protocol**、**Sixel**、**iTerm2 Inline Images**，如 WezTerm、Kitty、Ghostty、Windows Terminal 1.22+、iTerm2）的环境中，`PixelVectorGraphBackend` 使用纯 Rust CPU 2D 矢量光栅库 **`tiny-skia`** 1:1 还原 Swing `Graphics2D` 的抗锯齿斜线、任意交叉双色叠加与双环 HEAD 靶心节点，抹平 TUI 与 GUI 最后 10% 的视觉差距。

### 7.1 终端单元格像素几何校准 (`CellPixelMetrics`)

启动及收到 `Event::Resize` 时，通过 `crossterm::terminal::window_size()`（底层 `TIOCGWINSZ` / `CSI 14 t` / `CSI 16 t`）读取窗口总像素与行列数，计算精确单格像素尺寸：
```rust
pub struct CellPixelMetrics {
    pub cell_px_w: u16, // 例如 9px ~ 12px
    pub cell_px_h: u16, // 例如 20px ~ 26px (对应 IDEA PaintParameters.ROW_HEIGHT = 22)
}
```
- **自动降级护栏**：若终端返回 `width == 0 || height == 0`、处于未开启 `allow-passthrough` 的 `tmux`/`screen` 会话中，或图形协议握手超时（> 50ms），无感回退至 `CellGridGraphBackend`，绝不输出乱码控制序列。

### 7.2 1:1 移植 `PaintParameters` + `SimpleGraphCellPainter` + `HeadNodePainter`

以单行实际像素高度 $H = \text{cell\_px\_h}$、单槽像素宽度 $W_{\text{elem}} = 2 \times \text{cell\_px\_w}$ 建立坐标系，严格对齐 IDEA 比例公式（`PaintParameters.scaleWithRowHeight(val, H) = val * H / 22.0`）：

1. **线宽与圆点尺寸参数**：
   - 普通轨道线宽：`line_thickness = (1.5 * H / 22.0).max(1.0)`（圆头端点 `LineCap::Round`, `LineJoin::Round`）；
   - 悬停/选中加粗线宽：`selected_thickness = (2.5 * H / 22.0).max(line_thickness + 1.5)`；
   - 普通提交圆点半径：`circle_radius = 4.0 * H / 22.0`；
   - 第 `pos` 轨道中心横坐标：$x_0(\text{pos}) = \text{pos} \cdot W_{\text{elem}} + \lfloor W_{\text{elem}} / 2 \rfloor$，行中心纵坐标：$y_0 = \lfloor H / 2 \rfloor$。
2. **双倍跨行无缝对接斜线（对标 `SimpleGraphCellPainter.kt` 第 165-169 行）**：
   - 当绘制跨列斜线（`from != to`）时，若仅画到行底边界 $y = H$ 会出现折角；IDEA 的精妙做法是在裁剪矩形 `[0, row_top .. row_top + H)` 内，直接画从 $(x_1, y_{\text{center}})$ 到相邻行中心 **$(x_2, y_{\text{center}} \pm H)$** 的双倍长线段！
   - 这样第 $r$ 行的 `DOWN` 斜线与第 $r+1$ 行的 `UP` 斜线恰好在行交界处的中点 $(\frac{x_1+x_2}{2}, r \cdot H + H)$ 以完全相同的斜率无缝拼接，且**两条异色轨道交叉时各自保持完整真实色彩，彻底突破字符格单前景色限制**！
3. **虚线边与截断箭头（对标 `SimpleGraphCellPainter.kt` 第 137-144、219-229 行）**：
   - 虚线步长按斜边长度自适应计算整数段 `StrokeDash`，确保跨行交界处虚线相位连续；
   - 长边截断箭头旋转角参数完全复用 `ARROW_ANGLE_COS2 = 0.7`、`ARROW_LENGTH = 0.3 * H`。
4. **HEAD 三层同心圆靶心节点（对标 `HeadNodePainter.kt` 第 47-60 行）**：
   - 非选中态绘制三层同心圆：外环实心 `node_color`（外径 $2 \cdot (r + \Delta)$，$\Delta = 2.0 \cdot H / 22.0$） -> 中环实心镂空 `row_bg`（直径收缩 $2\Delta$） -> 内核实心圆点 `node_color`（直径再收缩 $2\Delta$）；选中态填充实心加粗大圆。

### 7.3 整条视口瓦片合成（Single Viewport Strip）与浮层遮挡安全

1. **整条视口条带一次提交（消除 Sixel 6px 接缝与协议开销）**：
   - 不按单行零碎发送 40 张小图，而是将当前可见视口图形列整块区域 `Pixmap::new(area.width * cell_px_w, area.height * cell_px_h)` 作为单张条带合成：
   - 第 `i` 行先填充其精确 `row_bg`（`#1E1F22` / `#252930` / `#2E436E`），再设置行裁剪框 `ClipMask` 绘制该行 `PrintElement`（这保证了即使在不支持 Alpha 透明通道的 Sixel 终端上背景也与右侧表格选区严丝合缝！）。
   - 以 `(visible_start_row, selection_hash, hover_element, area, theme_id)` 作为缓存键：光标仅在右侧详情面板滚动或未改变图状态时直接复用上一帧编码缓存，单帧 `tiny-skia` 重绘耗时 **< 0.5ms**。
2. **Z-Order 浮窗与右键菜单防穿透机制**：
   - **Kitty Unicode Placeholders 优先**：在支持 Kitty `U=1`（`U+10EEEE` 占位符）的终端（Kitty / Ghostty / WezTerm 新版）中，图像像素附着于 `Buffer` 字符格层级，上层 `ContextMenu` 或 `BranchesPopup` 执行 `widgets::Clear` 时天然擦除被遮挡区域的图像像素！
   - **Sixel / iTerm2 通用安全兜底**：在位图固定浮于文本之上的协议下，一旦检测到 `slice.occluding_rects` 与图形区 `area` 存在非空交集（例如右键菜单或分支浮窗正好盖住了左侧提交图），该帧自动对相交矩形做像素裁剪挖空或瞬时切回 `CellGridGraphBackend` 渲染底层字符，**100% 杜绝位图穿透遮挡上层右键菜单**！

---

## 8. 算法不变量与自动化验证要求

1. **拓扑正确性不变量**：对任意边 `(u -> v)`（`u` 为子提交，`v` 为父提交），在 `Normal` 与 `Bek` 排序下恒有 `visible_row(u) < visible_row(v)`。
2. **主干零偏移不变量**：若仓库存在 `origin/main`（或 `main`），沿着其 `first-parent` 链条向下的所有节点恒满足 `layout_index == 1` 且行内槽位 `position == 0`。
3. **轨道连续性不变量**：对任意无分支插队的度为 2 线性链节点序列 $c_1 \to c_2 \to \dots \to c_k$，恒有 `position(c_1) == position(c_2) == ... == position(c_k)`（绝不无故左右摇摆）。
4. **分叉色归属不变量**：主干与特性分支之间的所有斜向/横向连接边，其 `color_id` 恒等于特性分支的 `color_id`。
