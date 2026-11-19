# Spec 00: 系统总体架构、事件循环与混合 Git 引擎

- **目录**：`.scratch/00-architecture-and-git-engine/`
- **下属 Issues**：
  - [`01-workspace-tui-runtime-and-hit-test.md`](./issues/01-workspace-tui-runtime-and-hit-test.md)
  - [`02-hybrid-git-engine-and-watcher.md`](./issues/02-hybrid-git-engine-and-watcher.md)
- **上游参考源码**：
  - `reference/plugins/git4idea/backend/src/commands/`（Git 进程执行、环境变量隔离、锁控制）
  - `reference/plugins/git4idea/backend/resources/intellij.vcs.git.backend.xml`（`git.use.no.optional.locks`、`git.use.pathspec.from.file`）
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/BaseSingleTaskController.kt`（单飞异步计算与过期请求丢弃模型）

---

## 1. 设计目标与架构原则

1. **主线程零阻塞（60 FPS 恒定帧率）**：终端 UI 主循环（`ratatui::Terminal::draw` + 事件分发）严禁直接调用任何磁盘 I/O、`git2::Repository` 锁或 `git` 子进程；单帧渲染耗时目标 **< 4ms**（10 万级提交仓库下同样生效）。
2. **单向数据流（Elm / TEA 变体）+ 版本号取消机制**：借鉴 IDEA `BaseSingleTaskController` 与 `CommitDetailsLoader`，每次高频光标移动（如快速按住 `j`/`k` 浏览提交图）生成的异步 Diff/详情加载请求携带单调递增 `request_epoch: u64`，旧请求在后台自动中断或到达 UI 时被静默丢弃。
3. **键盘与鼠标一等公民平权（Full Mouse & Keyboard Parity）**：支持全键盘 Vim/IDEA 双风格导航的同时，在每一帧渲染阶段构建 **空间矩形命中注册表（`HitTestRegistry`）**，精确处理左键选中、双击展开/检出、滚轮局部滚动、拖拽分屏条（`Splitter`）、悬停高亮（轨道箭头/文件快捷暂存图标）以及**鼠标右键弹出上下文菜单**。
4. **`git2` 优先 + `git` CLI 兜底混合引擎**：兼顾 `libgit2` 的内存级操控速度与系统原生 `git` 可执行文件对 SSH Agent、Credential Manager、GPG/SSH 签名、Git Hooks 及复杂交互式变基的 100% 兼容性。

---

## 2. Workspace 与 Crate 职责划分

| Crate | 核心职责 | 严禁引入的依赖 |
| :--- | :--- | :--- |
| `jbgit-core` | 纯内存领域模型：DAG 提交图结构、DFS 轨道与非交叉布局算法、引用分组与排序器、三文档行级局部暂存补丁计算、模糊匹配与筛选数据结构 | 严禁依赖 `ratatui`、`crossterm`、文件系统监听或终端状态，确保纯函数式基准测试（`criterion`）与属性测试（`proptest`）毫秒级运行 |
| `jbgit-engine` | 封装 `git2`（线程局部 `Repository` 句柄池）与系统 `git` CLI 异步执行器、`notify` 防抖文件监听器、后台任务队列与 `GitConsoleEntry` 命令审计流 | 严禁依赖 `ratatui`、`crossterm` |
| `jbgit-tui` | 终端生命周期接管（Alternate Screen、Raw Mode、Bracketed Paste、Mouse Capture）、Darcula 主题系统、可拖拽双重/三重分屏布局、自研高保真控件与浮窗/右键菜单栈 | 不直接调用底层 `git2` 原始类型，全部消费 `jbgit-core` 与 `jbgit-engine` 暴露的快照与命令协议 |

### 核心第三方依赖选型基线

| 领域 | Crate 选型 | 选型理由 |
| :--- | :--- | :--- |
| TUI 渲染与事件 | `ratatui` + `crossterm` | 社区标准，直接操作 `ratatui::buffer::Buffer` 可逐 Cell 控制前景/背景色与字符组合 |
| Git 核心库 | `git2`（默认关闭无需的联网特性以加速编译，远程走 CLI） | 极速遍历 `Revwalk`、直接读取 ODB Blob/Tree、支持 `Index::add_frombuffer` 内存级局部暂存 |
| 异步运行时 | `tokio` + `crossbeam-channel` / `tokio::sync::mpsc` | 统一驱动终端事件流 (`EventStream`)、子进程异步管道读取、防抖定时器与线程池通信 |
| 行内/词级 Diff | `imara-diff` 或 `similar` | 高性能 Myers / Histogram 差分算法，支撑 `GitStageLineStatusTracker` 与词级（ByWord）高亮 |
| 语法高亮 | `syntect`（内置 Darcula `.tmTheme`） | 为右侧 Diff 预览窗提供与 IDEA Darcula 一致的源码词法着色 |
| 模糊速查 | `nucleo-matcher` | 极速模糊匹配评分与高亮索引提取，用于 Branches 浮窗 Speed-Search 与提交检索 |
| 文件监听 | `notify` + `notify-debouncer-mini` | 监听 `.git/` 与工作区变更，自动刷新暂存树与分支图 |
| Unicode 宽度 | `unicode-width` + `unicode-segmentation` | 精确处理 CJK 双宽字符、Emoji 与 Nerd Font 私有区图标列宽对齐 |

---

## 3. 事件循环与单向状态机架构

```
┌──────────────────────┐    Crossterm Event     ┌───────────────────────────┐
│  Crossterm Terminal  │ ─────────────────────► │   Input Router & HitTest  │
│ (Key/Mouse/Resize)   │                        │  (Modal Stack -> Focused) │
└──────────▲───────────┘                        └─────────────┬─────────────┘
           │                                                  │ Action
           │ Buffer Diff Draw                                 ▼
┌──────────┴───────────┐                        ┌───────────────────────────┐
│   Render Pipeline    │ ◄──── AppState ─────── │   State Reducer (Sync)    │
│ + HitTest Collector  │                        │  Updates UI State + Emits │
└──────────────────────┘                        └─────────────┬─────────────┘
                                                              │ Effect / TaskRequest (epoch)
                                                              ▼
┌──────────────────────┐   EngineEvent (epoch)  ┌───────────────────────────┐
│  notify Debouncer    │ ─────────────────────► │  jbgit-engine Supervisor  │
│ (.git & Worktree)    │                        │ ┌───────────┬───────────┐ │
└──────────────────────┘                        │ │git2 Worker│ CLI Runner│ │
                                                │ │ ThreadPool│ (Async)   │ │
                                                │ └───────────┴───────────┘ │
                                                └───────────────────────────┘
```

### 3.1 模态层栈（Overlay & Focus Z-Order）

模仿 IntelliJ Swing `IdeGlassPane` / `JBPopup` 层级管理，输入事件（键盘与鼠标）按 **自顶向下（Top-Down Z-Order）** 拦截：

1. **`Layer 4: Critical Modal Dialog`**（如 Reset Hard 风险确认框、删除未合并分支确认框、交互式文本输入框）—— 独占键盘焦点；点击遮罩外部关闭或告警。
2. **`Layer 3: Context Menu Stack`**（右键菜单及级联二级子菜单）—— 拦截方向键、助记符字母键、鼠标移动悬停与点击；左/右键点击菜单外区域先关闭菜单并根据策略决定是否透传。
3. **`Layer 2: Floating Popup`**（如 `GitBranchesPopup` 分支浮窗、提交历史选择浮窗、快捷键速查面板 `?`）—— 内置 Speed-Search 输入框自动捕获字符输入。
4. **`Layer 1: Main ToolWindow Workspace`** —— 顶部全局状态栏（仓库名、当前分支挂件、同步按钮、Ongoing 状态横幅）+ 工具窗口标签栏（`[1] Commit` | `[2] Log` | `[3] Stash` | `[4] Console`）+ 当前激活标签页的多分屏工作台。

### 3.2 空间矩形命中测试系统（`HitTestRegistry`）

由于 `ratatui` 采用即时模式（Immediate Mode）渲染，为了支持丝滑的鼠标交互（包括右键精确选中某一行提交、某一个分支 Pill、某一个 Diff Hunk 按钮或拖拽某条 1 字符宽的分屏线），每一帧 `render` 时同步填充 `HitTestRegistry`：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitTarget {
    /// 顶栏或工具窗口切换标签
    ToolTab(WorkspaceTab),
    /// 顶栏当前分支挂件（点击弹出 Branches Popup）
    BranchWidgetPill,
    /// 可拖拽分屏分割线（支持鼠标按住拖拽调整比例，双击恢复默认比例）
    Splitter { id: SplitterId, axis: SplitAxis },
    /// Log 左屏：分支面板节点
    BranchTreeNode { pane: PaneId, node_id: BranchTreeNodeId },
    /// Log 中屏：提交图中的特定节点或折叠箭头（参考 SimpleGraphCellPainter.getElementUnderCursor）
    GraphPrintElement { visible_row: usize, element_idx: usize },
    /// Log 中屏：某行提交上的特定 Pill 引用徽标
    CommitRefPill { visible_row: usize, ref_name: String },
    /// Log 中屏：提交表格行
    CommitTableRow { visible_row: usize, commit_oid: Oid },
    /// 右屏或左屏：变更文件树节点（附带右侧快捷悬停动作按钮区域，如 Stage `+` / Unstage `-`）
    ChangesTreeNode { tree_id: TreeId, node_id: ChangeNodeId, on_hover_action_icon: bool },
    /// Diff 视图行及局部操作热区（Stage Hunk / Unstage Hunk / Rollback Hunk 按钮与单行边栏）
    DiffLine { hunk_idx: usize, line_idx: usize, gutter_hit: bool },
    /// Diff Hunk 头部内联操作链接（参考 GitStageLineStatusTracker.createStageLinksPanel）
    DiffHunkAction { hunk_idx: usize, op: HunkOp },
    /// 上下文菜单或浮窗条目
    MenuItem { menu_depth: u8, item_idx: usize },
}

pub struct HitRegion {
    pub rect: ratatui::layout::Rect,
    pub z_index: u8,
    pub target: HitTarget,
}
```

**命中规则**：当收到 `MouseEvent { kind, column, row, modifiers }` 时，按 `z_index` 降序、后注册优先（同层子元素覆盖父容器）查找包含 `(column, row)` 的第一个 `HitRegion`：
- **`MouseEventKind::Down(MouseButton::Right)`**：
  1. 若命中 `CommitTableRow` / `BranchTreeNode` / `ChangesTreeNode` / `DiffLine` 且该条目尚未在当前多选集合中，**先将当前焦点与选中态切换至该条目**（与 IDEA `PopupHandler` 行为严格一致）；
  2. 根据 `HitTarget` 构建对应的上下文菜单模型，并在 `(column, row)` 处推入 `Layer 3: Context Menu`。

---

## 4. 混合 Git 引擎规范 (`jbgit-engine`)

### 4.1 读写通道职责边界矩阵

对标 `git4idea`，我们将所有 Git 操作划分为 **通道 A（`git2` 原生内存/磁盘直读直写）** 与 **通道 B（`git` CLI 外部进程调用）**：

| 操作类别 | 具体功能 | 执行通道 | 核心技术实现与原因 |
| :--- | :--- | :--- | :--- |
| **图与历史读取** | 全量/增量提交拓扑加载、Refs 枚举、`ahead/behind` 计算 | **通道 A (`git2`)** | `Repository::revwalk()` + ODB 头部直接解析，无需进程启动开销；10 万提交拓扑扫描 < 120ms |
| **工作区与暂存状态** | Staged / Unstaged / Untracked / Ignored / Conflicted 状态树 | **通道 A (`git2`)** | `Repository::statuses()` 开启 `include_untracked`、`renames_head_to_index`、`renames_index_to_workdir` |
| **Diff 与三方内容加载** | 提交间 Diff、本地三文档（`HEAD`/`Index`/`Worktree`）加载 | **通道 A (`git2`)** | 直接从 ODB 读取 Blob 字节流送入 `imara-diff` 计算行级/词级块，零临时文件 |
| **整文件/局部行级暂存** | Stage / Unstage 文件、**Hunk 级与单行级局部 Stage / Unstage** | **通道 A (`git2`)** | 通过三文档合并器在内存中生成新 `staged_bytes`，调用 `git2::Index::add_frombuffer(&entry, &staged_bytes)` + `index.write()`，**绝不污染工作区文件且速度 < 2ms** |
| **轻量引用操作** | 新建/重命名/删除本地分支、新建/删除轻量或附注标签 | **通道 A (`git2`)** | `repo.branch()`、`repo.tag()`，附带未合并分支安全性预检（`graph_descendant_of`） |
| **提交创建与修正** | 普通 Commit、Amend Commit | **自适应双通道** | 若未配置 GPG/SSH 签名（`commit.gpgsign != true`）且无可执行 `pre-commit`/`commit-msg` 钩子或用户显式勾选 `Skip Hooks`，可走 `git2` 极速提交；**否则一律走 `git commit` CLI**，确保 Hooks 输出与 GPG Pinentry 完整生效 |
| **检出与重置** | Checkout 分支/提交、Reset (`Soft`/`Mixed`/`Hard`/`Keep`)、Uncommit | **通道 B (`git` CLI) 为主** | `git checkout` / `git switch` / `git reset --keep`（`--keep` 模式完美对齐 IDEA 默认安全重置行为，防止覆盖未提交本地修改）及 LFS/Submodule 过滤器支持 |
| **重写与合并工作流** | Merge、Rebase（含 Interactive: Reword/Squash/Fixup/Drop）、Cherry-Pick、Revert、Stash | **通道 B (`git` CLI)** | 100% 复用 Git 官方 Sequencer 状态机（`.git/rebase-merge`、`MERGE_HEAD`、`CHERRY_PICK_HEAD`），通过环境变量注入非交互 `GIT_SEQUENCE_EDITOR` / `GIT_EDITOR` 实现无感 Reword/Squash/Drop |
| **远程网络同步** | Fetch、Pull（Merge/Rebase）、Push（含 `--force-with-lease`、Push Up To Commit） | **通道 B (`git` CLI)** | 天然继承用户的 OpenSSH Agent、Windows Git Credential Manager、HTTPS 代理、WSL/SSH Config，零配置打通企业私有仓库认证 |

### 4.2 `git` CLI 执行器规范与锁安全

参照 `reference/plugins/git4idea/backend/resources/intellij.vcs.git.backend.xml` 第 734 行（`git.use.no.optional.locks`）与第 797 行（`git.use.pathspec.from.file`）：

1. **环境变量标准注入**：
   - 只读辅助命令一律携带 `GIT_OPTIONAL_LOCKS=0`，防止后台状态探测与外部终端的 `git rebase` 争抢 `.git/index.lock`。
   - 禁用分页器与颜色转义：`GIT_PAGER=cat`、`LC_ALL=C.UTF-8`（解析机器输出时）、`GIT_TERMINAL_PROMPT=0`（避免后台进程挂起等待 stdin 密码；若遇凭据缺失则捕获错误并在 TUI 弹窗提示）。
   - 非交互式变基编辑器：当用户在 TUI 中执行 `Reword` / `Squash` / `Fixup` / `Drop` 时，jbgit 将自身可执行文件以内部子命令模式（或临时无副作用脚本）设为 `GIT_SEQUENCE_EDITOR` 与 `GIT_EDITOR`，精准改写 `git-rebase-todo` 后立即返回，与 IDEA `GitInteractiveRebaseEditorHandler` 机制一致。
2. **大批量路径参数传递**：
   - 当暂存/回滚选中超过 32 个文件时（尤其在 Windows 命令行长度限制下），使用 `--pathspec-from-file=- --pathspec-file-nul` 通过 `stdin` 传递 NUL 分隔路径列表。
3. **Git Console 实时审计日志**：
   - 每一次 `GitCliExecutor` 执行均向 `AppState.console_entries` 推送结构化记录：
     ```rust
     pub struct GitConsoleEntry {
         pub id: u64,
         pub started_at: DateTime<Local>,
         pub duration_ms: Option<u64>,
         pub command_line: String, // 例如: "[14:22:05] git fetch --prune origin"
         pub stdout_lines: Vec<String>,
         pub stderr_lines: Vec<String>,
         pub exit_code: Option<i32>,
     }
     ```
   - 用户切换到 `[4] Console` 标签页即可查看带时间戳、耗时与语法着色的全部底层 Git 命令历史（高保真复刻 IDEA Git ToolWindow 的 `Console` 标签页）。

### 4.3 多线程 `git2::Repository` 句柄池与文件监听

1. **线程安全**：`git2::Repository` 是 `Send` 但非 `Sync`。`jbgit-engine` 维护 `rayon` / 固定工作线程池，每个工作线程通过 `thread_local!` 惰性持有只读 `git2::Repository` 实例，避免每次查询重新打开 `.git/config` 与 packfile 索引。
2. **细粒度防抖监听（`RepoWatcher`）**：
   - 监听 `.git/HEAD`、`.git/refs/`、`.git/packed-refs`、`.git/MERGE_HEAD`、`.git/REBASE_HEAD`、`.git/CHERRY_PICK_HEAD` -> 触发 **`RefAndGraphRefresh`**（100ms 防抖）。
   - 监听 `.git/index` -> 触发 **`StageStatusRefresh`**（80ms 防抖）。
   - 监听工作区文件（按 `.gitignore` 预过滤，排除 `.git/objects` 与 `target/`、`node_modules/` 等大型忽略目录） -> 触发 **`WorktreeStatusRefresh`**（250ms 防抖）。
   - 当 `GitCliExecutor` 正在执行写操作期间（对标 `GitStagePanel.kt` 第 232 行 `if (workflow.isExecuting) hasPendingUpdates = true`），挂起中间态 UI 刷新，待命令结束一次性合并刷新，杜绝界面闪烁。

---

## 5. 性能与规模指标（SLO）

| 指标项 | 基准仓库规模 | 目标耗时上限 |
| :--- | :--- | :--- |
| 首屏冷启动并渲染前 200 行提交图与 Pill | 50,000 commits / 300 branches | **≤ 150 ms** |
| 全量 `PermanentGraph` 构建 + DFS 轨道分配 | 100,000 commits / 500 branches | **≤ 350 ms**（后台渐进加载，不阻塞首屏） |
| 提交图上下滚动单帧渲染（含轨道 + Pill） | 任意规模（视口虚拟化） | **≤ 2.5 ms / frame** |
| 切换选中提交后加载右侧变更树与首文件 Diff | 单次提交改动 ≤ 100 文件 | **≤ 25 ms** |
| 单行 / Hunk 局部暂存（`Index::add_frombuffer`） | 5,000 行源码文件 | **≤ 10 ms** |
