# 03: 实现右侧双重分屏（提交变更树 + 提交详情元数据面板 + 联动语法高亮 Diff 预览）

Status: ready-for-agent
Blocked by: 01, 02
Spec: [../spec.md](../spec.md)
Milestone: M3 — Git Log 三屏联动工作台

- **参考源码**：
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/frame/MainFrame.java`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/frame/VcsLogChangesBrowser.kt`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/details/commit/CommitDetailsPanel.kt`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/frame/CommitDetailsLoader.kt`

---

## 1. 背景与目标

完成 `[2 Log]` 三屏联动的最后一环：右屏上下双重分屏（上方 **Commit Changes Browser** 变更文件树 + 下方 **Commit Details** 富元数据面板），以及支持内嵌分屏与全屏放大的语法高亮 **Diff Preview**，实现与中屏提交选择、多提交聚合差异计算及父提交跳转的无缝闭环。

---

## 2. 核心实现任务清单

- [ ] **异步防抖详情与变更树加载器 (`CommitDetailsLoader`)**：
  - 在 `jbgit-engine` 实现单提交（对比 `parent[0]`；若为根提交则对比空树；若为 Merge 提交支持切换对比 `parent[0]` 或 `parent[1]`）与**多选提交集合**（按时间序合并净 Tree Diff）的快速变更列表及增删行数统计。
  - 内置容量 256 的 LRU 内存缓存：已浏览过的提交再次选中时 0ms 瞬时渲染；快速按键划过时通过 30ms 防抖 + `epoch` 丢弃中间态任务。
- [ ] **右上分屏：`CommitChangesBrowser` 变更树**：
  - 支持目录层级模式（含单链目录折叠 `src/core/graph`）与扁平文件列表模式切换。
  - 按 Darcula 文件状态色渲染 `M` / `A` / `D` / `R`（含重命名来源提示 `← old.rs`）与右对齐 `+N -M` 行统计。
- [ ] **右下分屏：`CommitDetailsPanel` 提交详情视图**：
  - 展示完整 40 位 SHA、GPG/SSH 签名状态徽标、Author/Committer 信息、**可点击/可聚焦跳转的父提交短哈希链接**（点击直接让中屏跳往该父提交！）、异步计算的 **所属分支与标签列表（Contained in Branches/Tags）** 以及完整多行 Commit Message 正文。
- [ ] **语法高亮 Diff 预览器 (`DiffPreviewPane`)**：
  - 集成 `syntect`（Darcula 语法高亮主题）+ `imara-diff` 行级与行内词级（`ByWord`：`#37664B` 新增词 / `#6E383E` 删除词）双重着色。
  - 支持 **Unified 单栏** 与 **Side-by-Side 左右双栏** 切换、上下文行数调节（`3 / 10 / Full`）、二进制文件/超大文件安全提示。
  - 支持内嵌分屏跟随预览（光标在 `CommitChangesBrowser` 上下移动时实时切换文件 Diff）及 `Enter` / `F4` 全屏放大阅读模式（`[` / `]` 跨文件翻页）。

---

## 3. 验收标准 (Acceptance Criteria)

- [ ] 在中屏按住 `j`/`k` 高速连续滚动 100 行不掉帧，停下后 30ms 内右屏变更树与详情面板准确同步至当前选中提交。
- [ ] 在中屏用 `Shift+Down` 连续选中 5 个提交，右上变更树自动展示这 5 个提交合并后的净变更文件集合。
- [ ] 在右下详情面板点击某父提交哈希，中屏立即居中跳转到该父提交行。
- [ ] Diff 预览对修改行能清晰呈现行底色与行内修改单词的两层对比度高亮。

---

## Comments

- 2026-11-18 (agent): Implemented. Engine loaders in `crates/jbgit-engine/src/details.rs` (single commit vs parent[N], merge parent switch, chronological zip net diff across multi-commit selections, rename detection, `imara-diff` line diffs + stats, ByWord ranges, `git log --format=%G?` GPG/SSH signature verification); 256-entry LRU + 30 ms debounce + epochs in `crates/jbgit-tui/src/app.rs`; changes tree / details / inline (Details/Right/Bottom dock) + F4 zoomed diff with Darcula `syntect` syntax highlighting in `crates/jbgit-tui/src/syntax.rs` and `views/log_details.rs`. Full SHA copy uses OSC 52 and depends on terminal support.
