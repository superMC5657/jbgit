# 08: FileHistory 模态内操作补全、分支筛选与 Annotate 深度增强

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M10 — reference 深度对齐与正确性加固
Priority: P1

- **参考源码**：
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/history/{FileHistoryPanel.java,FileHistoryFilterUi.kt,FileHistorySpeedSearch.kt,FileHistoryFilterer.kt}`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/actions/history/{ShowAllAffectedFromHistoryAction.kt,GetVersionFromHistoryAction.java,OpenRepositoryVersionFromHistoryAction.java,CreatePatchFromHistoryActionProvider.java}`
  - `reference/plugins/git4idea/backend/src/annotate/{GitAnnotationProvider.java,GitFileAnnotation.java}`、`actions/GitToggleAnnotationOptionsActionProvider.java`
- **关联文件**：
  - `crates/jbgit-core/src/details.rs`（`FileHistoryKey`、`BlameOptions`、`BlameCommit`）
  - `crates/jbgit-engine/src/details/{file_history.rs,blame.rs}`
  - `crates/jbgit-tui/src/{diff_cache.rs,input/menus.rs,input/keyboard.rs,log_view/file_history.rs,log_view/annotate.rs,views/file_history.rs,views/annotate.rs,menus/log.rs}`

---

## 1. 背景与目标

`FileHistory` 与 `Annotate` 在 Spec 09 / 12 已具备核心骨架，但在深度使用中仍有明显断层：
- `FileHistory` 模态目前**没有任何右键菜单**（无法在历史内直接恢复版本、查看该提交全部影响文件、查看文件全文、导出 Patch 或复制 Hash），写死单一起点 `HEAD`（无法切“所有分支”），写死 500 条上限无分页，且缺少模态内 `/` 搜索；
- `Annotate` 把 `-M`（文件内移动）与 `-C`（跨文件复制）绑在一个布尔开关上（大仓非常慢），未解析 `committer-time`，无法自定义 Gutter 列/按作者着色，底部仅展示单行 summary 而无法看完整提交说明，且每次打开/回退都重跑 `git blame` 无缓存。

## 2. 任务清单

- [x] **FileHistory 模态内右键菜单与动作补齐**：
  - 在 `input/menus.rs` 接入 `HitTarget::FileHistoryRow` 右键菜单与 `.` 键盘菜单，补齐 `Vcs.FileHistory.ContextMenu` 核心 6 项动作：
    1. **Get Version**（二次确认后执行 `GitOp::CheckoutPaths`，将此文件恢复到所选版本）；
    2. **Show All Affected Files**（展开/弹窗查看该提交变更的全部文件列表及 Diff，不退出 FileHistory）；
    3. **Open Repository Version**（直接以全文件只读语法高亮查看该版本完整内容，复用 TreeBrowser 预览组件）；
    4. **Create / Copy Patch**（单文件补丁复制到剪贴板或导出）；
    5. **Copy Revision Hash**；
    6. **Toggle Commit Details**（切换显示所选历史条目的完整提交正文、作者/提交者邮箱与时间戳）。
- [x] **FileHistory 分支筛选、分页与内联搜索**：
  - `FileHistoryKey` 新增 `scope: HistoryBranchScope { CurrentHead, AllBranches, Branch(String) }` 与 `limit: usize`（初始 500，列表底端显示 `[Load More (+500)]` 行，按 `Enter` 或点击续加载；`AllBranches` 时自动停用 Git 不兼容的 `--follow` 并在工具栏提示）；
  - 模态内支持 `/` 模糊搜索过滤历史条目（按提交消息 / 作者 / 短哈希），支持 `Space` 标记任意两条不相邻历史版本进行对比；
  - 目录历史（"Show Directory History"）保存进入前的 Log 筛选快照，退出或按返回快捷键时可无损恢复主 Log 筛选（不再永久冲掉用户原筛选）。
- [x] **Annotate 细粒度选项、视图定制与 LRU 缓存**：
  - `BlameOptions` 将 `detect_moves: bool` 升级为 `MoveDetection { None, WithinFile /* -M */, AcrossFiles /* -M -C */ }`，并新增 `prefer_commit_date: bool`（解析 `committer-time` / `committer-tz`）；
  - `AnnotateState` 支持：
    - 独立开关 Gutter 列（Hash / Author / Date）与着色模式（`Age` 年龄热力图 / `Author` 按作者分配色板 / `None`）；
    - **Annotate This Revision**（在当前行所归属的提交处重新打开 Annotate，与已有 `p` Previous Revision 互补，共享 `nav_back` 回退栈）；
    - 按 `i` / 点击详情条弹出该行提交的**完整提交消息与全部影响文件**预览（异步加载 `CommitChanges`）；
  - 将 `BlameBundle` 纳入 `DiffCacheStore`（按 `BlameKey` LRU 缓存，默认 16 条，仓库快照更新时自动失效），避免 `p` 回退栈往返时重复执行 `git blame`。

## 3. 测试

- **单元测试**：`BlameOptions` 三档移动检测参数编译与 `committer-time` 解析测试；`FileHistoryKey` 分支范围与 `--follow` 互斥测试；`DiffCacheStore` blame 缓存命中与失效测试。
- **引擎集成测试**：带重命名及多分支修改的仓库，`AllBranches` 能列出非当前分支修改记录；超过 `limit` 时分页续加载无重复条目。
- **TUI 会话测试**：FileHistory 模态内打开右键菜单执行 `Copy Revision Hash` / `Open Repository Version` / `/` 搜索；Annotate 切换 `Age ↔ Author` 着色与 `WithinFile` 模式，`p` 回退后再次进入命中缓存。
