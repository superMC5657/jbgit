# 02: 实现中部虚拟化提交图表格与多维筛选栏（文本/正则、分支、作者、日期、路径及高亮器）

Status: ready-for-agent
Blocked by: 00-architecture-and-git-engine/issues/02-hybrid-git-engine-and-watcher.md, 01-visual-system-darcula-and-widgets/issues/02-ref-pill-badges-and-grouping.md, 02-vcs-log-dag-graph-algorithm/issues/03-tui-graph-cell-rasterizer-and-hit-testing.md
Spec: [../spec.md](../spec.md)
Milestone: M3 — Git Log 三屏联动工作台

- **参考源码**：
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/table/VcsLogGraphTable.java`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/filter/`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/highlighters/CurrentBranchHighlighter.java`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/highlighters/MergeCommitsHighlighter.java`

---

## 1. 背景与目标

打造 `[2 Log]` 工作台的核心中枢 —— 顶部多维组合筛选栏（`VcsLogFilterBar`）与 60 FPS 视口虚拟化提交图表格（`GraphCommitTable`），无缝拼装 14 色连续非交叉轨道、Pill 引用胶囊、提交摘要、作者与时间戳，并支持拓扑父子跳转、多选区间及浏览历史前进后退。

---

## 2. 核心实现任务清单

- [ ] **多维组合筛选引擎 (`LogFilterSpec`)**：
  - 支持 5 类筛选条件交集组合：
    1. **Text / Hash 搜索**（`Ctrl+L` 或 `/` 聚焦，支持 `[.*]` 正则模式与 `[Cc]` 大小写敏感切换，同时匹配短/长 SHA 前缀与 Commit Message）；
    2. **Branch 过滤**（接收左屏选择或手动输入包含/排除规则 `+main -dependabot/*`）；
    3. **User 过滤**（内置 `@me` 当前 Git 用户快捷项及历史作者多选列表）；
    4. **Date 过滤**（预设时间窗与起止日期范围）；
    5. **Path 过滤**（按目录或文件路径集筛选触及提交）。
  - 实现 `[⚙ Presentation]` 下拉弹出菜单：切换 `Compact References`、`Show Tag Names`、`Show Long Edges`、`IntelliSort (Bek)`、`First Parent Only`、`Show Hash Column`。
- [ ] **虚拟化 `GraphCommitTable` 高性能渲染**：
  - 严格只计算与渲染当前可见窗口行区间 `scroll_top .. scroll_top + rows`，集成平滑滚动、半页/整页翻页（`Ctrl+D/U`、`PgUp/PgDn`）、首尾直达（`g` / `G`）与居中定位（`vcs.log.table.show.centered`）。
  - 集成行级视觉高亮器：
    - `CurrentBranchHighlighter`：对 `HEAD` 可达提交行填充 `#252930` 微亮底色；
    - `MergeCommitsHighlighter`：对 Merge 提交标题应用 `#6F737A` 柔和灰前景色；
    - 搜索关键词命中高亮：当文本过滤器激活时，在提交标题中高亮匹配的字串片段。
- [ ] **图拓扑导航与多选/历史栈**：
  - 实现 `Right` (`→`) 跳转父提交行（`GoToParentRow`）、`Left` (`←`) 跳转子提交行（`GoToChildRow`）。
  - 实现 `Ctrl+G`（`GoToHashOrRef`）弹窗输入任意 SHA / 分支 / 标签 / `HEAD~N` 瞬间居中跳转，并维护浏览历史栈支持 `Alt+Left` / `Alt+Right` 前进后退。
  - 支持 `Shift+Up/Down` 与 `Shift+Click` 连续多行选中，向下游右屏发出选中提交 Oid 列表（附 30ms 防抖与 `epoch` 版本号）。

---

## 3. 验收标准 (Acceptance Criteria)

- [ ] 在 ≥ 50,000 提交的真实仓库（如本项目的上游参考仓库或 Linux/Rust 级仓库）中连续滚动帧率恒定，单帧渲染耗时 ≤ 3ms。
- [ ] 开启文本正则过滤或路径过滤后，非匹配提交被隐藏，剩余匹配提交依然保持正确的相对轨道颜色与虚线拓扑连接。
- [ ] `→` / `←` 键能顺着提交图跨越中间无关分支提交精准跳到直接父提交与子提交。

---

## Comments
