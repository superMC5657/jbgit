# Spec 03: Git Log 三屏联动工作台与多维筛选器规约

- **目录**：`.scratch/03-vcs-log-three-pane-linkage/`
- **下属 Issues**：
  - [`01-branches-dashboard-left-pane.md`](./issues/01-branches-dashboard-left-pane.md)
  - [`02-vcs-log-table-and-filter-bar.md`](./issues/02-vcs-log-table-and-filter-bar.md)
  - [`03-commit-changes-details-and-diff-linkage.md`](./issues/03-commit-changes-details-and-diff-linkage.md)
- **上游参考源码**：
  - `reference/plugins/git4idea/backend/src/ui/branch/dashboard/BranchesInGitLogUiFactoryProvider.kt`（左屏与主框架装配、焦点遍历策略、分支联动模式）
  - `reference/plugins/git4idea/backend/src/ui/branch/dashboard/BranchesDashboardTreeModel.kt`（分支目录树分组、Favorites、Incoming/Outgoing）
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/frame/MainFrame.java`（中屏表格、右屏变更浏览器与提交详情上下分屏、Diff 预览器）
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/frame/CommitDetailsLoader.kt`（异步防抖详情加载）
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/highlighters/`（`CurrentBranchHighlighter`、`MergeCommitsHighlighter`）

---

## 1. 三屏整体布局与响应式折叠拓扑

对标 `BranchesVcsLogUi` + `MainFrame`，`[2 Log]` 标签页由 **左、中、右三屏** 以及可选内嵌/全屏 **Diff 预览窗** 构成：

```text
┌───────────────────┬───────────────────────────────────────────────┬─────────────────────────────┐
│  Left Pane (20%)  │               Center Pane (50%)               │      Right Pane (30%)       │
│                   ├───────────────────────────────────────────────┤                             │
│ Branches          │ Filter Bar: [🔍 Search] [Branch▾] [User▾] [⚙] │ Commit Changes Browser (62%)│
│ Dashboard Tree    ├───────────────────────────────────────────────┤ (Dir/Flat Tree + +/- Stats) │
│                   │                                               ├─────────────────────────────┤
│ • HEAD (main)     │ Virtualized Graph + Commit Table              │ Commit Details Panel (38%)  │
│ • Local/...       │ [Graph Tracks] [Pills + Subject] [Author][Age]│ (Hash, GPG, Parents, Refs,  │
│ • Remote/origin/..│                                               │  Full Message Body)         │
│ • Tags/...        │                                               │ (可切换为内联 Diff Preview)  │
└───────────────────┴───────────────────────────────────────────────┴─────────────────────────────┘
```

### 1.1 焦点环（Focus Cycle）与快速面板导航
对标 `BranchesInGitLogUiFactoryProvider.kt` 第 198-205 行 `ComponentsListFocusTraversalPolicy`：
- `Tab` / `Shift+Tab` 循环顺序：`Left Branches Tree` -> `Center Graph Commit Table` -> `Right Commit Changes Tree` -> `Right Commit Details / Diff Preview`。
- 快捷直达：`Ctrl+L` 或 `/` 直接聚焦中屏顶部 `Text/Regex Filter` 搜索框；`Alt+B` 折叠/展开左侧分支面板（折叠时左边缘保留 2 列竖向条带 `[»]` 提示，对标 `ExpandStripeButton`）。
- 终端窄屏自适应（宽度 < 110 列时）：自动折叠左侧 `Branches` 面板（可通过 `Alt+B` 浮层呼出），保证中屏提交图与右屏变更详情的可读性。

---

## 2. 左屏：Branches Dashboard 分支导航树 (`01-branches-dashboard-left-pane`)

对标 `git4idea/ui/branch/dashboard`：

### 2.1 树节点层级模型
```text
▼ HEAD (main)                      ★  ↑2 ↓1
▼ Local (8)
  ▼ feature
      ★ dag-layout                 ↑1
        partial-stage
    ★ main                         ↑2 ↓1
      hotfix-crlf
▼ Remote (14)
  ▼ origin
      ★ main
        feature/dag-layout
        release/v1.0
▶ Tags (26)                           (受设置 Show Tags 开关控制，默认展示)
```
- **斜杠目录折叠（Group by Directory，默认开启）**：将形如 `feature/auth/oauth2` 的分支名自动按 `/` 构建可展开/收起的虚拟文件夹节点（可一键切换为扁平列表）。
- **星标收藏置顶（Favorites）**：`main`/`master` 默认带 `★`；在任意分支节点按 `Space`（星标区点击）或菜单 `Mark as Favorite` 切换星标，同一分组内星标分支排在普通分支之前（对标 `BranchTreeNodeComparator`）。
- **同步偏离徽标（Incoming / Outgoing Indicators）**：通过 `git2::Repository::graph_ahead_behind(local_oid, upstream_oid)` 异步计算：
  - `↑N`（绿色 `#629755`）：本地有 `N` 个待推送（Outgoing）提交；
  - `↓M`（蓝色 `#548AF7`）：上游远程跟踪分支有 `M` 个待拉取（Incoming）提交。
- **内联快速筛选（Inline Speed-Search）**：焦点在左屏树时直接键入字母，顶部浮现搜索条实时过滤匹配分支并自动展开命中目录路径，`Esc` 清空过滤。

### 2.2 左屏选中 → 中屏联动双模式
严格复刻 `BranchesInGitLogUiFactoryProvider.kt` 第 220-264 行 `SelectionHandler` 的两种交互语义：
1. **显式触发分支过滤（`Enter` 或 鼠标双击节点）**：
   - 将选中节点（若选中文件夹 `feature` 则包含其下所有子分支！）写入中屏 `BranchFilter`，中屏立即重构 `VisibleGraph` 仅展示可达提交；支持 `Ctrl+Click` 或多选多个分支做并集对比过滤！再次在根节点按 `Esc` 或点击 `[Clear Branch Filter]` 恢复全部分支。
2. **定位跳转提交（`J` / 工具栏定位图标 `[⌖]` 或开启 `Navigate Log to Branch on Selection` 选项）**：
   - **不改变**中屏当前过滤条件，直接将中屏提交表格滚动并居中高亮（对标 `vcs.log.table.show.centered = true`）该分支/标签指向的目标提交行！

---

## 3. 中屏：多维筛选栏与虚拟化提交图表格 (`02-vcs-log-table-and-filter-bar`)

### 3.1 顶部多维筛选栏 (`VcsLogFilterBar`)
包含 5 个组合式过滤器槽位与 1 个视图展示配置菜单 `[⚙ Presentation]`：
1. **文本 / 哈希搜索框 (`Ctrl+L` / `/`)**：
   - 支持匹配提交 Subject/Body 或完整/短 SHA-1 前缀；
   - 右侧附带两个内联开关徽标：`[.*]`（正则匹配 `EnableFilterByRegex`）与 `[Cc]`（区分大小写 `MatchCase`）。
2. **`Branch` 筛选器下拉框**：显示当前生效的分支集合（如 `Branch: main + feat/*`），支持输入表达式排除分支（如 `-dependabot/*`）。
3. **`User` 筛选器下拉框**：支持快速选择 `Me`（读取 `git config user.name / user.email`）或仓库历史贡献者列表模糊多选。
4. **`Date` 筛选器下拉框**：预设 `Today`、`Yesterday`、`Last 7 Days`、`Last 30 Days` 及自定义起止日期 `YYYY-MM-DD`。
5. **`Paths` 路径筛选器**：仅展示触及特定目录或文件集合的提交历史（与 `--full-history --simplify-merges` 语义对齐）。
6. **`[⚙ Presentation]` 显示设置下拉菜单**（对标 `Vcs.Log.PresentationSettings`）：
   - `[x] Compact References View`（紧凑引用胶囊）
   - `[x] Show Tag Names`（展开显示标签名）
   - `[ ] Show Long Edges`（显示跨越 ≥30 行的长边）
   - `[x] IntelliSort (Bek)`（智能拓扑时间排序）
   - `[ ] First Parent Only`（仅看第一父主线）
   - `[x] Highlight: Current Branch` / `[x] Highlight: Merge Commits`

### 3.2 虚拟化提交图表格 (`GraphCommitTable`)
- **严格视口切片（Viewport Virtualization）**：无论可见图有 100 行还是 500,000 行，每帧仅向 `PrintElementGenerator` 与提交元数据缓存请求 `scroll_offset .. scroll_offset + viewport_height` 区间的行！
- **列构成与自适应弹性宽度**：
  1. **Graph + Refs + Subject 主列（弹性自适应剩余全部宽度）**：先绘制 `2 * graph_cols` 宽的彩色连续轨道，紧跟 1 空格间距、排好序的 `PillBadgeBar`，再绘制单行提交摘要 Subject。
  2. **Author 列（固定 14-18 列，窄屏可隐藏）**：作者姓名截断对齐。
  3. **Date 列（固定 12-16 列）**：支持切换相对时间（`2h ago`、`Yesterday`）与紧凑绝对时间（`2026-11-18 14:20`，支持切换 Author Date / Commit Date）。
  4. **Hash 列（可选开启，7-8 列）**：短哈希灰字。
- **行级高亮器（对标 `VcsLogHighlighter`）**：
  - `CurrentBranchHighlighter`：通过 `PermanentGraph::contained_in_branch(HEAD)` 位集（`FixedBitSet`，构建耗时 < 5ms）判断，当前分支已包含的历史行赋予微亮底色 `#252930`，未合入的其他分支行使用标准 `#1E1F22`，一眼看清哪些提交已在当前分支、哪些尚未合入！
  - `MergeCommitsHighlighter`：父提交数 ≥ 2 的合并提交摘要文字使用柔和灰 `#6F737A`，突出实质业务提交。
- **图拓扑快捷键导航（对标 `intellij.platform.vcs.log.impl.xml` 第 185-190 行）**：
  - `Right` (`→`)：`GoToParentRow` —— 直接沿着图轨道跳转到当前提交的父提交行（多父提交时默认跳第一父，按数字或弹窗跳第二父）！
  - `Left` (`←`)：`GoToChildRow` —— 沿着图轨道向上跳转到子提交行！
  - `Ctrl+G`：`GoToHashOrRef` —— 弹出输入框输入任意 SHA、分支名、`HEAD~3` 瞬间居中定位，并推入导航历史栈（`Alt+Left` / `Alt+Right` 前进后退，对标 `MainFrame.myHistory`）。
- **多行连续/离散选择**：支持 `Shift+Up/Down` 或鼠标拖拽选择连续提交区间！当选中多个提交时，右屏自动聚合计算所选提交集合的合并净变更树（对标 `VcsLogAsyncChangesTreeModel`）！

---

## 4. 右屏：双重分屏（变更文件树 + 提交详情 + Diff 联动）(`03-commit-changes-details-and-diff-linkage`)

对标 `MainFrame.java` 第 99-190 行的 `myDetailsSplitter` 与 `myDiffPreview`：

### 4.1 右上分屏：提交变更树 (`CommitChangesBrowser`)
- **头部工具栏**：显示变更文件统计（如 `Changes (5 files: +128 -34)`）、目录树/扁平列表切换 `[⊞ Group by Dir]`、展开/收起全部节点、`[👁 Diff Preview]` 开关。
- **单链目录压缩（Compact Middle Packages）**：若 `src/main/rust/graph` 中间目录仅有一个子目录，自动折叠显示为单行 `▼ src/main/rust/graph`，避免深层缩进吞噬水平空间。
- **文件节点渲染**：
  - 状态标识 + 着色文件名：`M` (`#6897BB`)、`A` (`#629755`)、`D` (`#808080`)、`R` (`#70AEFF`，附灰字 `← old_name.rs`)；
  - 右端对齐 `+added -deleted` 行数微统计。
- **Merge 提交双亲切换**：当选中一个 Merge 提交时，默认展示相对于主父提交 `parent[0]` 的净改动，并提供快捷切换查看相对于 `parent[1]` 的冲突解决改动（对标 `ShowChangesFromParentsAction`）。

### 4.2 右下分屏：提交元数据与详情面板 (`CommitDetailsPanel`)
支持独立上下滚动，展示：
1. **完整 40 位 Commit SHA**（点击复制）+ **GPG / SSH 签名验证徽标**（`✓ Verified` 绿色胶囊或 `⚠ Unverified`）。
2. **Author & Committer**：若作者与提交者不同（如 Cherry-pick / Rebase / GitHub Merge），双行分别列出姓名、邮箱与精确时间；相同时合并单行展示。
3. **Clickable Parents**：列出所有父提交短哈希链接（如 `Parents: a8f3d21  9c01e4b`），鼠标点击或按键直接在中屏提交图中跳转至该父提交（对标 `MainFrame.java` 第 100-103 行）！
4. **Contained in Branches / Tags**：异步计算包含该提交的分支与标签列表（对标 `PermanentGraph.getContainingBranches`），解决“这个 bug 修复到底进没进 `release/v1.2` 分支”的高频工程追问！
5. **完整 Commit Message**：保留完整多段落正文、列表缩进及 `Co-authored-by:` / `Signed-off-by:` / `#123` Issue 高亮。

### 4.3 联动 Diff 预览面板 (`FrameDiffPreview`)
提供两种无缝预览形态（快捷键 `Enter` / `Ctrl+D` / 工具栏切换）：
1. **内嵌分屏预览（Inline Split Diff）**：可在右侧下方替换详情区，或在最右侧展开第四栏 / 底部通栏（对标 `MoveDiffPreviewToRight` / `MoveDiffPreviewToBottom`），随着用户在 `CommitChangesBrowser` 中上下移动文件光标，实时流式刷新当前文件语法高亮 Diff（支持 Unified 单栏与 Side-by-Side 双栏对比、行内词级 `ByWord` 差异高亮、可调上下文行数 `3 / 10 / Full File`）。
2. **全屏沉浸式 Diff 模态窗（Zoom Diff，`Ctrl+Enter` / `F4`）**：一键将当前文件 Diff 放大至全屏阅读，支持 `[` / `]` 在同一提交的上一个/下一个变更文件间无缝翻页，`Esc` 原位返回三屏日志台。

---

## 5. 三屏异步联动数据流与防抖契约

```text
[左屏选中分支并回车]
      │ FilterAction::SetBranches(vec!["feat/ui"])
      ▼
[jgit-engine 后台构建 VisibleGraph (≤15ms)]
      │ EngineEvent::VisiblePackUpdated { pack, epoch }
      ▼
[中屏 GraphCommitTable 重绘 & 选中首行提交 C0]
      │
      ├──► (0ms 立即响应) 若 LRU 缓存已命中 C0 详情，右屏 0 延迟同步更新
      └──► (30ms 防抖调度, epoch+1) 发送 TaskRequest::LoadCommitDetails { oids, epoch }
                  │
                  ▼
           [git2 Worker 读取 Commit Tree Diff + Containing Branches]
                  │ EngineEvent::CommitDetailsLoaded { details, changes_tree, epoch }
                  ▼
           [校验 epoch == latest_epoch -> 更新右上 ChangesTree & 右下 DetailsPanel & DiffPreview]
```
- **核心体验保障**：当用户按住 `↓` 键以每秒 30 行的速度飞速滚过提交图时，中屏滚动 **0 卡顿**，过期 `epoch` 的右屏 Diff 计算在后台自动让路，松手停下的瞬间（≤ 30ms）右屏树、详情与代码 Diff 精准落位！
