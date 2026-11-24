# jbgit 操作与配置手册

本文档详细说明 **jbgit** 的交互操作方式、各视图快捷键以及多层配置文件体系。如果只需要快速安装与启动，请先参阅项目根目录的 [README.md](../README.md)。

---

## 目录

- [一、交互设计概览](#一交互设计概览)
- [二、快捷键速查表 (Keybindings)](#二快捷键速查表-keybindings)
  - [全局操作 (Global)](#全局操作-global)
  - [提交日志视图 (Log View)](#提交日志视图-log-view)
  - [分支树面板 (Log Branches Pane)](#分支树面板-log-branches-pane)
  - [全局分支浮窗 (Branches Popup)](#全局分支浮窗-branches-popup)
  - [提交工作台 (Commit View)](#提交工作台-commit-view)
  - [统一差异对比 (Diff View — Log / Commit / Stash)](#统一差异对比-diff-view--log--commit--stash)
  - [贮藏箱 (Stash View)](#贮藏箱-stash-view)
  - [控制台 (Console View)](#控制台-console-view)
  - [文件历史与冲突合并 (File History & Conflict Merge)](#文件历史与冲突合并-file-history--conflict-merge)
  - [菜单、命令面板与交互对话框 (Menus, Command Palette & Dialogs)](#菜单命令面板与交互对话框-menus-command-palette--dialogs)
- [三、配置体系指南 (Configuration)](#三配置体系指南-configuration)
  - [1. 配置文件生成与查看](#1-配置文件生成与查看)
  - [2. 配置文件路径与 4 级覆盖优先级](#2-配置文件路径与-4-级覆盖优先级)
  - [3. `jbgit.toml`：通用行为、图谱与布局比例](#3-jbgittoml通用行为图谱与布局比例)
  - [4. `theme.toml`：主题预设与语义配色](#4-themetoml主题预设与语义配色)
  - [5. `keymap.toml`：快捷键预设与增量覆盖](#5-keymaptoml快捷键预设与增量覆盖)
  - [6. 常用 `run` 动作标识符参考](#6-常用-run-动作标识符参考)
- [四、远端与凭据 (Remotes & Credentials)](#四远端与凭据-remotes--credentials)
- [五、切换仓库与 Worktree (Repositories & Working Trees)](#五切换仓库与-worktree-repositories--working-trees)
- [六、提交图谱：折叠线性分支 (Collapse Linear Branches)](#六提交图谱折叠线性分支-collapse-linear-branches)
- [七、应用 Patch 与按版本浏览 (Apply Patch & Browse at Revision)](#七应用-patch-与按版本浏览-apply-patch--browse-at-revision)

---

## 一、交互设计概览

jbgit 默认采用**零冲突统一键位（`preset = "default"`）**，并通过以下机制降低记忆门槛：

- **动态快捷键胶囊栏**：屏幕底部根据当前聚焦面板实时展示最常用快捷键。
- **Which-Key 前缀提示卡**：按下多键序列前缀（如 `g`）后，自动弹出可用后续按键提示。
- **可搜索命令与快捷键面板 (`?`)**：在任意界面按 `?` 打开，支持模糊检索，按 `Enter` 可直接执行高亮命令。
- **上下文操作菜单 (`.`)**：按 `.`、`<Shift-F10>` 或点击**鼠标右键**，随时打开当前选中提交、分支、文件或贮藏的完整操作菜单。
- **鼠标支持**：支持点击切换标签页与面板焦点、滚动列表、拖拽面板分割线、点击筛选芯片（Branch / User / Date / Paths）以及右键菜单。

---

## 二、快捷键速查表 (Keybindings)

> 下表以默认预设 `preset = "default"` 为准；若切换到 `"jetbrains"` 或 `"vim"` 预设，部分按键行为会相应调整（详见[第 5 节](#5-keymaptoml快捷键预设与增量覆盖)）。

### 全局操作 (Global)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `q` / `<Ctrl-c>` | 退出 jbgit |
| `?` | 打开可搜索快捷键帮助 / 命令面板（按 `Enter` 可直接执行当前命令） |
| `.` / `<Shift-F10>` / `<Menu>` / `鼠标右键` | 打开当前选中项的操作菜单 |
| `r` / `<F5>` | 强制刷新 Git 仓库状态 |
| `1` / `2` / `3` / `4`（或 `<Alt-1>` – `<Alt-4>`） | 按标签栏顺序切换视图：**Commit (`1`)** / **Log (`2`)** / **Stash (`3`)** / **Console (`4`)** |
| `<Tab>` / `<Shift-Tab>` | 在当前视图各面板之间循环切换焦点 |
| `b` / `g b` / `` <Ctrl-`> `` / `<Ctrl-~>` / `<Alt-b>`*(非 Log 页)* | 呼出全局分支速查浮窗（`<Esc>` 关闭） |
| `f` / `p` / `P` | 远程同步：**Fetch (`f`)** / **快捷 Pull (`p`)** / **Push… (`P`)** |
| `<Alt-l>` / `<Alt-e>` | 打开 **Pull… 选项对话框 (`<Alt-l>`)** / **标准 Rebase… 选项对话框 (`<Alt-e>`)** |
| `<Alt-Shift-g>` / `<Alt-Shift-b>` / `<Alt-Shift-s>` / `<Alt-Shift-r>` | *(Bisect 进行中)* 标记当前提交为 **Good** / **Bad** / **Skip** / **Reset** 结束二分查找 |
| `<Alt-u>` | **Update Project**（三套预设一致）：贮藏本地改动 → `fetch --all --prune` → 当前分支按 `[update].mode` 合并或变基 upstream → 其他跟踪分支仅快进 → 可选同步更新已初始化子模块 → 恢复改动；若当前分支无上游或上游已删除 (`[gone]`) 会自动弹出跟踪分支选择器；完成通知 `<Alt-z>`「查看」打开只读 Range 筛选的 Log。头部分支胶囊中的 `↓N` 计数可点击打开模式选择（含子模块时提供 `Update Submodules` 复选框） |
| `<Alt-r>` | **Configure Remotes…**：远端列表（添加 / 重命名 / 编辑 URL / 删除 / 获取），详见[四、远端与凭据](#四远端与凭据-remotes--credentials) |
| `<Alt-o>` / 点击头部仓库名 | **切换仓库**：最近仓库 / 当前仓库的 linked worktree / 已初始化子模块，模糊过滤，`<Ctrl-o>` 输入任意路径；详见[五、切换仓库与 Worktree](#五切换仓库与-worktree-repositories--working-trees) |
| `<Alt-w>` | **Working Trees…**：Worktree 列表（打开 / 新建 / 锁定 / 删除 / 清理；需 git ≥ 2.17） |
| `<Ctrl-Alt-n>` | 从当前 HEAD 新建分支 |
| `<Alt-z>` / `<Alt-x>` | 触发当前底部通知栏的快捷动作 / 关闭通知 |
| `<F8>` / `<F12>` | 循环切换图谱渲染引擎（Auto / Vector / Cell） / 切换图标模式（Nerd Font / Unicode） |

### 提交日志视图 (Log View)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `j` / `k` 或 `<Down>` / `<Up>` | 上下移动光标浏览提交记录（`<Ctrl-Up>` / `<Ctrl-Down>` 仅移动光标而不清空多选） |
| `<Shift-Up>` / `<Shift-Down>` / `<Shift-Click>` | 以锚点连续区间多选提交记录（`<Ctrl-Shift-Click>` 追加区间） |
| `<Space>` / `<Ctrl-Click>` | *(提交表)* 勾选 / 取消勾选当前提交（支持非连续多选，选中项带 `◆` 标记，按 `<Esc>` 收起回到单选；超过 100 个提交时右侧变更区需按 `<Enter>` 或点击确认加载聚合变更） |
| `<PageUp>` / `<PageDown>` | 整页翻滚提交列表 / 变更树 |
| `g g` / `G`（或 `<Home>` / `<End>`） | 快速跳转至列表顶部 / 底部 |
| `<Left>` / `<Right>` / `<Shift-Right>` | *(提交表)* 跳转至子提交 / 第 1 父提交 / 第 2 父提交（多父时弹出选择） |
| `<Alt-Left>` / `<Alt-Right>` | *(提交表)* 导航历史后退 / 前进 |
| `<Enter>` / `<Esc>` | 提交表 → 右侧变更树 → Diff 预览逐级进入，`<Esc>` 逐级返回 |
| `/` / `<Ctrl-f>` / `<Ctrl-l>` | 聚焦分支与提交过滤输入框（搜索框内支持 `<Alt-r>` 正则、`<Alt-c>` 大小写敏感、`<Ctrl-u>` 清空） |
| `<Ctrl-b>` / `<Alt-b>` | 折叠 / 展开左侧分支树栏位 |
| `c` | 检出 (Checkout) 选中的提交或分支 |
| `n` | 在当前提交创建新分支 |
| `y` | 复制选中提交哈希（提交右键菜单 / 命令面板亦支持 **Copy Commit Message** 复制完整提交说明与 **Copy Row Summary** 复制 `hash subject — author (date)` 行摘要，支持多选逐行复制） |
| `<Ctrl-d>` | 版本差异对比（任意选中两个提交（含非连续）时自动按较旧 → 较新对比两者；单选时开关差异预览） |
| `X` | *(双提交对比模式)* 交换对比方向（Base ↔ Target），亦可在变更树 / Diff 工具栏点击 `⇄ 交换` |
| `a` | 切换「仅显示受影响变更」（启用 Paths 路径筛选时仅列出命中路径的变更文件，标题栏显示可点击的 `[已筛选: M/N]` 徽标） |
| `<F4>` / `<Ctrl-Enter>` | *(变更树 / Diff)* 切换全屏差异预览 |
| `v` / `e` / `E` | *(变更树)* 切换按目录树分组 (`v`) / 全部展开 (`e`) / 全部折叠 (`E`) |
| `H` | *(变更树)* 查看选中文件的完整提交历史（File History） |
| `<Alt-g>` | *(变更树 / Diff)* 打开 Git Blame 逐行追溯（Annotate）浮窗 |
| `g r` / `<Ctrl-g>` | 按提交哈希、分支名或 Tag 快速定位跳转 |
| `m` | 为合并提交选择 / 切换父提交 |
| `u` / `<F2>` / `<Del>` | 撤回最近提交 (Uncommit) / 修改说明 (Reword) / 丢弃提交 (Drop) |
| `<Alt-t>` | *(提交表)* 按版本浏览仓库文件树（Browse Repository at Revision，见[第七章](#七应用-patch-与按版本浏览-apply-patch--browse-at-revision)）；`g r` 跳转弹窗中 `<Alt-Enter>` 同样打开 |
| `<Ctrl-Shift-->` / `<Ctrl-Shift-=>` | 折叠 / 展开全部线性分支（vim 预设另有 `z c` / `z o` 折叠 / 展开选中处、`z M` / `z R` 全部折叠 / 展开），详见[第六章](#六提交图谱折叠线性分支-collapse-linear-branches) |

### 分支树面板 (Log Branches Pane)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `/` / `<Ctrl-f>`（`<Esc>` 退出） | 进入分支树模糊搜索（可在 `keymap.toml` 设为 `tree_speed_search = true` 开启直接打字速查） |
| `<Left>` / `<Right>` / `<PageUp>` / `<PageDown>` | 折叠或展开分组节点 / 翻页滚动 |
| `<Space>` | 添加 / 取消收藏当前分支（置顶显示） |
| `<Enter>` | 按选中分支过滤提交日志 |
| `J` | 跳转至选中分支对应的提交记录 |
| `c` / `n` / `<Ctrl-Alt-n>` / `<F2>` / `d` / `<Del>` | 检出 (`c`) / 新建分支 (`n`/`Ctrl+Alt+N`，支持实时空格→`_` 自动清洗、覆盖已有分支 `-B` 与 `--track` 追踪) / 重命名 (`F2`，可勾选同时取消上游追踪) / 删除分支或 Tag (`d`/`Del`，未合并分支确认框会列出未合并提交列表) |
| **本地/远程分支右键菜单** | `Set Upstream…` / `Unset Upstream`（本地分支显式管理上游追踪）、`Compare with…`（任意双分支对称对比：顶部对比条支持一键切换 `A..B` / `B..A` / `A...B` 方向与查看 `git diff A..B` 树差异）、`Checkout and Rebase onto Current`（远程分支检出/重置本地分支并变基到当前分支）、`Cleanup Branches…`（扫描已合并及 `[gone]` 分支并支持批量删除与一键恢复） |
| **Tag 节点右键菜单** | 检出（游离 HEAD）、从 Tag 新建分支、对比/合并、推送单个 Tag、删除本地 Tag（若远端存在同名 Tag 可勾选同时删除；删除后支持通知栏 `<Alt-z>` 一键恢复）、删除远端 Tag |
| **Remote 分组 / `<remote>` 节点右键菜单** | `Fetch '<remote>'`（仅获取该远端，`git fetch --prune <remote>`） / `Configure Remotes…` |

### 全局分支浮窗 (Branches Popup)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `直接打字` / `<Ctrl-u>` | Speed Search 模糊速查过滤分支与 Tag / 清空输入 |
| `<Enter>` / `<Right>` | 打开选中分支的操作子菜单（检出、新建、重命名、删除、对比等） |
| `<Ctrl-Alt-n>` | 从当前 HEAD 新建分支（跟随 `new_branch` 修饰键配置） |
| `<Alt-f>` / `<Esc>` | 执行远程 Fetch / 关闭浮窗 |
| 顶部 `↓ Update Project…` | 选择 `Follow Git config` / `Merge` / `Rebase` 后执行 Update Project（仅在非游离 HEAD 时显示） |
| 顶部 `🧹 Cleanup Branches…` | 打开已合并分支批量清理窗口（支持切换基准分支 `HEAD`/`main`、识别 `[gone]` 上游、多选勾选批量删除并在删除后一键恢复） |
| 顶部 `⚙ Configure Remotes…` | 打开远端管理窗口（同 `<Alt-r>`） |
| 顶部 `⎇ Working Trees…` | 打开 Worktree 管理窗口（同 `<Alt-w>`；git < 2.17 时不显示） |
| 分支行右侧 `⎇ <worktree>` | 该分支已在其他 Worktree 检出：子菜单中 Checkout 置灰并说明原因，改为提供 `Open in Worktree '<name>'` |

### 提交工作台 (Commit View)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `<Alt-p>` / `<Alt-Shift-p>` | 应用 Patch 文件（路径相对仓库根，`Tab` 补全） / 从剪贴板应用 Patch，见[第七章](#七应用-patch-与按版本浏览-apply-patch--browse-at-revision) |
| `<Space>`（或 `s`/`+`、`u`/`-`） | **暂存 / 取消暂存** 当前选中的文件或代码块 |
| `<Enter>` / `<Left>` / `<Right>` | 折叠/展开目录容器，或在文件上聚焦右侧 Diff 预览 |
| `c` | 聚焦提交说明编辑器（变更树与 Commit Diff 预览窗格均可直接按 `c`；编辑区内按 `<Esc>` 返回变更树） |
| `<Ctrl-s>` / `<Ctrl-Enter>` / `<Alt-Enter>` | 执行提交（编辑区内 `<Ctrl-Alt-k>` 提交并推送） |
| `A`（或编辑区内 `<Alt-a>` / `<Alt-m>`） | 切换追加至上一提交 (`Amend`) 模式 |
| `<Alt-s>` / `<Ctrl-m>` | 切换 Signed-off-by 签名 / 打开历史提交说明选择器 |
| `<Ctrl-a>` | 全选变更文件 |
| `d` / `<Del>` / `<Ctrl-Alt-z>` | 放弃选中文件或代码块的本地修改 (Discard Changes) |
| `v` / `e` / `H` / `<Alt-g>` | 按目录树分组 (`v`) / 外部编辑器 (`e`) / 文件历史 (`H`) / Git Blame 逐行追溯 (`<Alt-g>`) |
| `<Ctrl-e>` / `<Alt-q>` / `<Ctrl-z>` / `<Ctrl-y>` / `<Ctrl-w>` | *(提交说明编辑器内)* 外部编辑器编辑说明 / 72 列正文自动重排 (`Reformat`，保留标题行、列表、代码块、Trailers 与注释行) / 撤销 / 重做（亦支持 `<Ctrl-Shift-z>`）/ 删词（亦支持 `<Alt-Backspace>`） |

> 提示：
> - **模板、合并消息与注释剥离**：支持自动载入 `commit.template` 以及 Merge/Squash/冲突恢复时的 `.git/MERGE_MSG` / `.git/SQUASH_MSG`；提交时无论 CLI (`--cleanup=strip`) 还是 `git2` 路径均按 `core.commentChar` 剥离注释行；正文超过 72 列的行会在编辑器内高亮提示，可按 `<Alt-q>` 或点击 `[72↺]` 一键重排。
> - **提交选项与护栏**：选项栏支持 `Amend`、`Sign-off`、`Hooks`、`Empty`（`--allow-empty`）、`GPG` 三态签名开关（`Auto` 跟随仓库 `commit.gpgsign` / `-S` 强制签名 / `--no-gpg-sign` 跳过签名，可点击或经命令面板 `cycle_gpg_sign` 切换）与 `Author`（输入时支持按最近提交者 `Tab`/`↑`/`↓` 自动补全）；未配置 `user.name`/`user.email` 时自动弹出「配置 Git 身份信息」对话框（支持本地/全局）并在保存后自动继续提交；`Undo Last Commit` / `Uncommit` 遇到合并提交或已推送至上游的 HEAD 时会弹出二次危险确认；`Commit & Push…` 提交成功后直接进入完整 Push 对话框。
> - **灵活 `.gitignore` / `.git/info/exclude`**：变更树右键菜单提供「添加忽略规则… (`Add to Ignore…`)」子菜单，可按精确路径、扩展名（`*.ext`）或父目录（`/<dir>/`）写入仓库根 `.gitignore`、最近祖先嵌套 `.gitignore` 或 `.git/info/exclude`，并可一键在 `$EDITOR` 中打开对应规则文件。
> - 当仓库（含 linked worktree 共享的 `common_dir/hooks`）未配置提交类 Git Hooks 时，Commit 面板的 `Hooks` 复选框会置灰显示为 `Hooks (none)`；处于 Merge / Cherry-Pick / Revert / Rebase 状态时，提交会自动走原生 `git commit` CLI 以保留多父提交并清理 sequencer 状态文件。
> - 符号链接（含悬空链接）、类型变更与 Submodule gitlink 在变更树中支持整条暂存 / 取消暂存 / 放弃指针变更，行级局部暂存自动禁用。

### 统一差异对比 (Diff View — Log / Commit / Stash)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `j` / `k` / `<PageUp>` / `<PageDown>` / `g g` / `G`（或 `<Home>` / `<End>`） | 上下移动、翻页及跳转顶部 / 底部（含全屏 Diff 模式） |
| `[` / `]` | 跳转至上一个 / 下一个 Hunk 代码块 |
| `{` / `}` | 跳转至上一个 / 下一个变更文件 |
| `S` | 切换单列统一对比 (Unified) / 双列并排对比 (Side-by-Side) |
| `w` | 循环切换上下文保留行数 (3 行 / 10 行 / 全量) |
| `I` | 循环切换忽略空白模式（`Exact` → `-b` 忽略空白数量变化 → `-w` 忽略全部空白；Commit Diff 开启忽略空白时自动禁用局部行/块暂存，避免丢失空白） |
| `W` | 切换软换行（Soft Wrap，按显示宽度与 CJK 字符边界自动折行并同步滚屏高度） |
| `h` / `l` 或 `<Left>` / `<Right>` | 未开启软换行时按列水平滚动（`<Shift-Left>` / `<Shift-Right>` 大步进），标题栏显示 `Col +N` 指示；Tab 字符按真实 `tab_size` 列对齐展开 |
| `/` · `n` / `N` · `<Esc>` | Diff 内联搜索：`/` 打开底部搜索栏（忽略大小写、实时统计 `[cur/total]` 并高亮命中片段），`Enter` 确认后用 `n` / `N` 跳转下一/上一命中并自动居中滚屏，`<Esc>` 优先关闭搜索 |
| `m` / `z`（或 `<F4>` / `<Ctrl-Enter>`） | *(Log Diff)* 切换停靠位置 (`m`) / 全屏差异预览 (`z`/`F4`/`Esc`)；全屏下按 `P` 切换合并父提交；二进制文件自动展示 Old/New 字节体积变化与短 Blob OID 元数据卡片 |
| `<Space>` / `v` / `d` | *(Commit Diff)* 暂存/取消 (`Space`/`s`/`u`) · 行级可视选择 (`v`) · 回滚选区 (`d`/`Del`，`<Ctrl-z>` 撤销) |
| `x` / `i` / `c` / `<Esc>` | *(Commit Diff)* 切换未暂存与已暂存侧 (`x`) · 三方检视 (`i`) · 聚焦提交说明 (`c`) · 退出可视选择或返回变更树 (`Esc`) |

> 差异视图顶部会在发生文件权限/类型变更或符号链接变更时展示元数据头（如 `mode 100644 → 100755`、`symlink target (120000)`），并在文件末尾无换行符时显式展示 `\ No newline at end of file`。

### 贮藏箱 (Stash View)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `<Enter>` / `<Right>` / `<Esc>` | 安全切换焦点至右侧差异预览（不会误操作应用贮藏），`<Esc>` 返回贮藏列表 |
| `a` / `o` | 应用选中贮藏 (`a`) / 应用并弹出 (`o`)；右键菜单另提供 `--index` 还原暂存态（`Apply/Pop (Reinstate Index)`） |
| `n` / `B` / `d` / `C` | 新建贮藏 (`n`，支持消息、`--include-untracked`、`--keep-index` 与仅选中文件；Stage 变更树右键 `Stash Changes…` 同理) / 从贮藏创建分支 (`B`) / 删除 (`d`/`Del`) / 清空 (`C`) |
| `v` / `e` / `E` / `<Left>` / `<Right>` | *(右侧变更树)* 按目录分组 (`v`) / 全展开 (`e`) / 全折叠 (`E`) / 单项折叠展开；右键单文件支持 **Diff with Local** 与 **Apply Single File** (`git checkout stash@{n} -- <path>`) |

> 提交日志右键菜单中，Cherry-Pick 与 Revert 均提供普通提交与 **Without Committing (`-n`)** 两个选项；选中合并提交时自动弹出主线父提交选择（`-m 1` / `-m 2`）；遇到空 Cherry-Pick 时自动弹出 **Skip / Allow Empty / Abort** 恢复弹窗。

### 控制台 (Console View)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `j` / `k` 或 `<Down>` / `<Up>` | 在命令列表切换命令，或在输出详情逐行滚动 |
| `g g` / `G`（或 `<Home>` / `<End>`） | 快速跳转至顶部 / 底部 |
| `<Ctrl-d>` / `<Ctrl-u>` / `<PageDown>` / `<PageUp>` | *(输出详情)* 半页 / 整页滚动输出日志（命令列表支持 `<PageUp>`/`<PageDown>` 翻 10 行） |
| `<Enter>` / `<Esc>` | 进入右侧输出详情面板 / 返回左侧命令历史列表 |
| `p` | *(命令列表)* 执行 `git status --short --branch` 状态探测（可通过 `git_status_probe` 重绑） |

### 文件历史与冲突合并 (File History & Conflict Merge)

**文件历史浮窗**（在 Log 或 Commit 变更树按 `H` 打开；对目录按 `H` 或右键「显示选中目录的历史」会保存当前 Log 筛选条件快照，在 Log 按 `<Esc>` 或点击 `[← 恢复筛选]` 即可无损还原）：

| 快捷键 | 功能描述 |
| :--- | :--- |
| `<Tab>` / `<Shift-Tab>` | 在提交列表、全部影响文件（开启时）与差异预览之间切换焦点 |
| `j` / `k` / `<PageUp>` / `<PageDown>` / `g g` / `G`（或 `<Home>` / `<End>`） | 上下移动、翻页及跳转顶部 / 底部 |
| `<Shift-Up>` / `<Shift-Down>` / `<Space>` | 扩展多选两个提交，或按 `<Space>` 标记/取消任意两个非相邻提交进行对比 |
| `/` | 提交列表聚焦时开启模糊搜索（匹配提交信息/作者/短哈希）；Diff 聚焦时开启 Diff 内搜索 |
| `B` / `L` | 切换分支范围（当前 HEAD / 全部分支 `--all` / 指定本地分支；`--all` 自动禁用 `--follow`）/ 分页加载更多 (+500) |
| `.`（或右键提交行） | 打开文件历史上下文菜单（获取此版本、全部影响文件、打开仓库版本、复制补丁、复制哈希、切换提交详情等） |
| `R` / `a` / `v` / `p` / `y` / `i` | 获取此版本 (`R`，需确认) / 显示全部影响文件 (`a`) / 打开仓库版本只读预览 (`v`) / 复制补丁 (`p`) / 复制哈希 (`y`) / 切换提交详情 (`i`) |
| `<Enter>` | 跳转并在主 Log 视图定位到当前选中提交 |
| `f` / `l` / `b`（或 `<Alt-g>`） | 切换追踪重命名 (`f`) / 对比本地工作区 (`l`) / 在当前提交打开 Annotate 逐行追溯 (`b`/`Alt-g`) |
| `S` / `w` / `W` / `z`（或 `<F4>`） | 切换并排对比 (`S`) / 循环上下文行数 (`w`) / 自动换行 (`W`) / 全屏差异预览 (`z`/`F4`) |
| `<Esc>` | 依次关闭仓库版本预览、搜索栏、影响文件/详情面板、标记锚点、全屏 Diff 或文件历史浮窗 |

**Git Blame 逐行追溯浮窗 (Annotate)**（在 Log / Commit 变更树或 Diff 预览按 `<Alt-g>`，或由上下文菜单 / File History 按 `b` 进入；内建 16 槽位 LRU 缓存，往返修订零重复执行 `git blame`，仓库刷新时自动失效）：

| 快捷键 | 功能描述 |
| :--- | :--- |
| `j` / `k` / `<PageUp>` / `<PageDown>` / `g g` / `G` | 上下逐行浏览源码与边栏 / 跳转首尾行 |
| `:` / `<Ctrl-g>` | 输入目标行号快速跳转（Go to Line） |
| `v` / `V`（或 `<Shift-Up>` / `<Shift-Down>`） | 开关可视行选择并扩展行区间（在 Commit Diff 预览同样支持 `v` 选中块/行） |
| `H` | 对当前光标行或可视选中行区间执行 `git log -L` 查看 **所选代码行历史 (Selection History)** |
| `r` / `p` / `P`（或 `<Backspace>`） | 追溯当前选中行所属修订 (`Annotate This Revision`) / 上一修订 (`Previous Revision`) / 返回导航栈 |
| `w` / `m` / `D` / `C` | 切换忽略空白 (`-w`) / 移动复制检测三态循环 (`off` → `-M` → `-M -C`) / 切换作者日期与提交日期 (`D`) / 切换着色模式 (`Age` → `Author` → `None`) |
| `i`（或点击底部详情条） / `d` / `<Tab>` | 展开完整提交信息与全部影响文件 (`i`) / 开关底部内联 Diff 预览 (`d`) / 切换焦点 (`Tab`) |
| `<Enter>` / `y` / `.` | 跳转到主 Log 对应提交 / 复制该行提交哈希 / 打开 Annotate 上下文菜单（含独立列显隐开关：哈希/作者/日期） |
| `<Esc>` | 依次关闭 Diff 搜索、行跳转输入、提交详情面板、可视选区或 Annotate 浮窗 |

**多文件冲突队列与三栏 3-Way 合并浮窗**（按 `<Alt-c>` 或点击 Ongoing 横幅 `[Resolve Conflicts]` 进入；存在 ≥ 2 个冲突文件时自动打开冲突队列，单文件时直接进入三栏合并器，也可从右键菜单随时打开队列）：

*多文件冲突队列 (`ConflictQueue`)*：

| 快捷键 | 功能描述 |
| :--- | :--- |
| `j` / `k` / `<PageUp>` / `<PageDown>` / `g` / `G` | 在冲突文件表格（`Path`、`Yours`、`Theirs`、`Status`）中移动 |
| `<Space>` / `a` | 勾选 / 取消多选文件，或全选 / 清空选择 |
| `<Enter>` / `m` | 打开选中文件的三栏 3-Way 合并器（保存暂存后自动前进到下一个未解决文件，全部解决后弹出继续 Rebase/Merge 或前往提交面板确认框） |
| `y` / `t` | 对当前或批量勾选的未解决文件执行 `Accept Yours` / `Accept Theirs` |
| `r` | 对已误解文件执行 **Revert Resolved (`git checkout -m`)** 重新恢复冲突态 |
| `M` / `<Esc>` | 在外部 Mergetool 打开当前文件 / 关闭冲突队列 |

*三栏 3-Way 冲突合并浮窗 (`ConflictResolver`)*：

| 快捷键 | 功能描述 |
| :--- | :--- |
| `j` / `k` / `<PageUp>` / `<PageDown>`、`h` / `l` / `<Left>` / `<Right>` | 垂直滚动冲突视图 / 水平平移长行 |
| `n` / `<Tab>`、`p` / `<Shift-Tab>` | 跳转至下一个 / 上一个冲突块（视口自动跟随居中定位） |
| `a` | **Apply Non-Conflicting Changes**：基于索引 `:1/:2/:3` 一键自动应用所有非冲突单侧变更（`LeftOnly` / `RightOnly` / `BothSame`） |
| `b` | 展开 / 折叠当前冲突块的 **Base (`:1`) 共同祖先对照区** |
| `1` / `y`、`2` / `t`、`3` / `B`、`4` / `d`、`0` / `x` / `u` | 当前块：采纳己方 (Ours) / 对方 (Theirs) / 保留双方 / 双向丢弃 (`Neither`) / 重置为未解决（Delete/Modify 冲突提供「保留文件」/「删除文件」决策） |
| `e` | 进入当前块的**轻量多行内联编辑模式**修改 Result 文本（保留语法高亮与水平滚动，`<Ctrl-s>` / `<F2>` 应用、`<Esc>` 取消） |
| `Y` / `T` | 全部冲突块一键采纳己方 / 采纳对方 |
| `m` / `E` | 在外部 Mergetool / `$EDITOR` 中打开冲突文件 |
| `<Enter>` / `<Ctrl-s>` / `<Esc>` | 保存（或删除）解决结果并暂存 / 取消关闭 |

> **冲突语义与护栏说明**：
> - **真正 3-Way 模型与高亮**：读取 Git index `:1/:2/:3` 并基于 `imara-diff` 区分非冲突改动与真冲突，三栏视图接入可选语法高亮与块内词级差异提示；
> - **Rebase 冲突方向自动对齐**：在 `Rebase` / 交互式 `Rebase` 暂停期间，界面上的 **Yours（我方，重放提交）** 与 **Theirs（对方，目标分支）** 以及 `Accept Yours` / `Accept Theirs` 均自动翻转映射至正确的 Git 索引阶段（`:3` / `:2`），与 `Merge` 保持一致语义；
> - **Delete/Modify 与 Both-Deleted 冲突**：采纳删除侧时自动通过 `git rm -f`（或三栏弹窗内「删除并暂存」）移除工作区文件并清理索引冲突；
> - **行尾符无损保留**：解决文本冲突写盘时自动保留原文件的 `CRLF` (`\r\n`) / `LF` (`\n`) 与末尾换行符状态；
> - **二进制 / 子模块 / 目录冲突护栏**：遇到二进制文件（8 KiB NUL 嗅探）、子模块 gitlink (`0o160000`) 或目录冲突时，不以乱码打开三栏文本编辑器，而是弹出专用选择对话框展示两侧 OID 与文件大小，支持直接采纳我方 / 对方或调用外部 Mergetool；若冲突文件在打开期间被外部工具解决，浮窗顶部会给出失效预警。

### 菜单、命令面板与交互对话框 (Menus, Command Palette & Dialogs)

| 组件 | 常用快捷键 |
| :--- | :--- |
| **上下文级联菜单** | `j`/`k`/`↑`/`↓`/`Home`/`End` 选择；`→` 展开子菜单；`←`/`Esc` 关闭当前层；`<Enter>`/`<Space>` 确认；`<Del>` 触发删除项；按高亮助记字母直达 |
| **快捷键命令面板 (`?`)** | 直接打字模糊搜索；`↑`/`↓`/`PgUp`/`PgDn` 选行；`<Enter>` 执行高亮命令；`<Ctrl-u>` 清空；`<Esc>`（或空查询按 `?`）关闭 |
| **交互式变基待办 (Rebase Todo)** | `j`/`k`/`↑`/`↓` 选行；`J`/`K` 或 `<Alt/Shift-Up/Down>` 调整提交顺序；`<Space>` 循环动词；`p` pick / `r` 或 `w` reword / `e` edit（暂停并切至 Commit 面板 amend 后 Continue）/ `s` squash / `f` fixup / `x` 或 `<Del>` drop（`update-ref` 行只读保留）；底部实时展示当前提交的变更文件摘要，`[`/`]` 切换文件，`d` / `<Ctrl-d>` 展开完整 Diff 预览；`<Enter>` 执行；`<Esc>` 关闭 Diff 或取消。支持在 Log 多选**非连续提交**自动重排后 Squash，以及独立创建 `Fixup! Commit` / `Squash! Commit` 或直接 `Rebase '<cur>' onto Selected Commit` |
| **输入 / 消息 / 选择 / 危险确认弹窗** | 单行输入：`<Tab>`/`↑`/`↓` 切复选框、`<Ctrl-u>` 清空、`<Enter>` 确认；多行消息：`<Enter>` 换行、`<Ctrl-s>`/`<Ctrl-Enter>`/`<Alt-Enter>` 确认；单选/确认：`↑`/`↓`/`←`/`→`/`<Tab>` 切换、`<Space>` 切换可选复选框、助记键或 `y`/`n`/`q` 快速响应、`<Esc>` 取消 |
| **筛选与跳转浮窗** | `FilterEditor` 支持 `<Tab>` 自动补全候选；`ChooseParent` 支持 `1`–`9` 数字键直选父提交 |

**Merge 选项对话框**（分支 / 远端分支 / Tag 菜单中的 `Merge '<x>' into '<cur>'…`；无省略号的快速 Merge 仍保持 `merge --no-edit <rev>`）：

| 快捷键 | 功能描述 |
| :--- | :--- |
| `↑` / `↓` / `<Tab>` | 在选项行之间移动（`Fast-forward`、`--squash`、`--no-commit`、`--allow-unrelated-histories`、`-s` 策略、`-X` 策略选项、`-m` 提交说明） |
| `<Space>` / `←` / `→` | 切换复选框或循环下拉值（`default` / `--no-ff` / `--ff-only`；`ort` / `recursive` / `resolve` / `octopus` / `ours` / `subtree`；`ours` / `theirs` / `patience` …） |
| `f` / `s` / `n` / `u` / `t` / `x` / `m` | 跳到对应行并切换（`m` 聚焦提交说明，聚焦后直接打字、`<Alt-Enter>` 换行、`<Ctrl-u>` 清空） |
| `<Enter>` / `<Esc>` | 执行合并 / 取消。互斥组合（`--squash` 与 `--no-ff`/`--ff-only`/`-m`、`--ff-only` 与 `-m`/`--no-commit`、`-m` 与 `--no-commit`）会红字提示并禁用 `Merge` |

- 标题下方显示 `cur..rev` 待合并提交数，底部实时预览将执行的 `git merge …` 命令；选项（不含提交说明）按仓库记忆在 `<common_dir>/jbgit-merge-options`。
- `--squash` / `--no-commit` 完成后自动切到 `[1 Commit]` 并以 `.git/SQUASH_MSG` / `MERGE_MSG` 预填提交说明。
- `--ff-only` 因分叉失败时弹出「普通合并 / 变基到 '<rev>' / 取消」。

**Pull… 选项对话框**（全局 `<Alt-l>`、分支速查浮窗或当前分支菜单中的 `Pull…`；快捷 `p` 仍保持默认 `git pull --no-edit`）：

- 支持切换远端 (`Remote`)、编辑与 `←`/`→` 补全远端分支 (`Remote branch`)，按 `<Ctrl-r>` / `<F5>` 或点击 `[Fetch]` 在弹窗内即时拉取并刷新远端分支列表；
- 支持 `--rebase`、`Fast-forward`（`default` / `--no-ff` / `--ff-only`）、`--squash`、`--no-commit`、`--no-verify`，自动校验互斥组合（`--rebase` ⟂ `--squash`/`--no-ff`/`--ff-only`/`--no-commit`，`--squash` ⟂ `--no-ff`/`--ff-only`）并实时预览命令；选项按仓库记忆至 `<common_dir>/jbgit-pull-options`。

**标准 Rebase… 选项对话框**（全局 `<Alt-e>`、分支速查浮窗或分支 / 远端分支 / Tag 菜单中的 `Rebase…`）：

- 支持配置 `Target / --onto`、可选 `Old base (upstream)`（填写后自动转为 `--onto <new> <old>`）、可选切换分支 (`Branch`)，以及 `--interactive`、`--rebase-merges`、`--update-refs`（自动校验 Git ≥ 2.38）、`--keep-empty`、`--autosquash`、`--root`（启用 `--root` 时禁用显式 `upstream`）；选项按仓库记忆至 `<common_dir>/jbgit-rebase-options`。

**完整 Push 推送浮窗**（全局按 `P`（含游离 HEAD 推送），或在分支/提交菜单选择 `Push…` / `Push up to Here` / Commit 面板 `Commit & Push…` 打开；包含 `Target | Outgoing Commits | Changed Files / Diff` 三栏预览）：

| 快捷键 | 功能描述 |
| :--- | :--- |
| `<Tab>` / `<Shift-Tab>` | 在 `Target` / `Outgoing Commits` / `Changed Files` 三栏之间循环切换焦点 |
| `e` / `r` | 内联编辑目标分支名（支持 `<Tab>` 补全远端分支、`↑`/`↓` 选候选、`<Ctrl-u>` 清空、`<Enter>` 确认并刷新待推提交，默认遵循 `remote.<name>.push` refspec） / 切换远端仓库 (`r`；无远端时可直接点击 `[+ 添加远端…]` 或按 `r`/`a` 内联定义远端) |
| `j` / `k` / `<PageUp>` / `<PageDown>` / `g g` / `G` | 浏览待推提交列表、右侧变更文件树或文件差异 |
| `d` / `<Enter>` *(Files 栏)* | 打开或关闭当前选中变更文件的 Diff 预览（Diff 内支持 `S` 并排对比、`c` 上下文行数、`[`/`]` Hunk 跳转） |
| `t` / `f` / `u` / `n` | 循环切换 Tag 模式 (`None` / `Current branch (--follow-tags)` / `All (--tags)`) / 强推模式 (`No` / `With lease`（精确绑定 `--force-with-lease=<ref>:<oid>`）/ `Force`，受 `[push].protected_branches` 保护拦截) / 勾选 `Set upstream`（游离 HEAD 自动禁用） / 勾选 `Skip hooks` (`--no-verify`) |
| `<Enter>` / `<Ctrl-Enter>` / `<Esc>` | 执行推送（若因非快进被拒绝，弹窗提供带 Smart 自动贮藏/恢复的 `Merge & Retry` / `Rebase & Retry`，若待变基区间包含合并提交还会显式警示） / 取消关闭 |

---

## 三、配置体系指南 (Configuration)

jbgit 采用与 Yazi 类似的**多文件 TOML + 分层覆盖**配置体系，所有配置文件均可按需缺省或仅写需修改的字段。

### 1. 配置文件生成与查看

```bash
# 在全局用户配置目录下生成主配置 jbgit.toml 及全部主题与快捷键预设（themes/*.toml、keymaps/*.toml，已存在的文件会跳过）
jbgit --init-config

# 将内置默认配置或预设打印到标准输出
jbgit --print-default-config jbgit            # 打印 jbgit.toml 默认内容
jbgit --print-default-config darcula          # 打印 themes/darcula.toml 完整配色（亦可用 theme）
jbgit --print-default-config catppuccin-mocha # 打印 themes/catppuccin-mocha.toml
jbgit --print-default-config gruvbox-dark     # 打印 themes/gruvbox-dark.toml
jbgit --print-default-config keymaps/default  # 打印 keymaps/default.toml 完整底表（亦可用 keymap）
jbgit --print-default-config jetbrains        # 打印 keymaps/jetbrains.toml
jbgit --print-default-config vim              # 打印 keymaps/vim.toml
```

### 2. 配置文件路径与 4 级覆盖优先级

按优先级**由高到低**逐层叠加覆盖（仅覆盖显式指定的字段，未指定字段保留下层值）：

1. **`JBGIT_CONFIG_DIR` 环境变量目录**：`$JBGIT_CONFIG_DIR/jbgit.toml` 及 `themes/*.toml`、`keymaps/*.toml`
2. **仓库级配置目录**：`<repo-workdir>/.jbgit/jbgit.toml`（亦支持 `.jbgit/config.toml` 别名）及 `themes/*.toml`、`keymaps/*.toml`
3. **全局用户配置目录**：
   - **Windows**: `%APPDATA%\jbgit\config\`（若未设置 `APPDATA` 则回退至 `%USERPROFILE%\.config\jbgit\`）
   - **Linux / macOS**: `$XDG_CONFIG_HOME/jbgit/`（默认 `~/.config/jbgit/`）
4. **程序内置默认值**

配置目录采用**单一主入口 + 预设子目录**结构：
- `jbgit.toml`：唯一主配置文件（通用设置、选择主题 `flavor`、选择快捷键 `[keymap].preset`、图谱渲染、Diff 选项、视图栏位比例等）
- `themes/<flavor>.toml`：各主题预设的完整调色板（内置 `darcula` / `catppuccin-mocha` / `gruvbox-dark`，可直接修改覆盖，也可新建自定义 `themes/<name>.toml` 并在 `jbgit.toml` 中设置 `flavor = "<name>"`）
- `keymaps/<preset>.toml`：各快捷键预设的完整/增量定义（内置 `default` / `jetbrains` / `vim`，可直接修改覆盖，也可新建自定义 `keymaps/<name>.toml` 并在 `jbgit.toml` 中设置 `[keymap] preset = "<name>"`）

> 兼容说明：若配置目录中仍放有旧版独立的 `theme.toml` 或 `keymap.toml`，程序仍会将其作为同层增量覆写叠加加载，但 `--init-config` 不再生成这两个冗余包装文件。

### 3. `jbgit.toml`：通用行为、图谱与布局比例

```toml
[general]
# 界面语言: "zh-CN" (简体中文), "en-US" (English), "auto" (自动跟随系统环境变量)
language = "zh-CN"

# 启动时默认打开的工作区标签页: "log", "commit", "branches", "stash", "console"
default_tab = "log"

# 主题预设风格: "darcula", "catppuccin-mocha", "gruvbox-dark"（或自定义 themes/<name>.toml）
flavor = "darcula"

[keymap]
# 快捷键预设风格: "default", "jetbrains", "vim"（或自定义 keymaps/<name>.toml）
preset = "default"
# 底部快捷键提示栏最大行数（默认 1；>1 自动多行；0 隐藏）
footer_max_lines = 1
# 分支树面板直接打字速查（默认跟随所选预设；取消注释可强制覆盖）
# tree_speed_search = false

[graph]
# 提交图渲染引擎: "auto" (自动侦测图形协议), "vector" (Kitty/Sixel/iTerm2 矢量图), "cell" (字符网格)
backend = "auto"

# 图标模式: "nerd_font" (需要终端安装 Nerd Font), "unicode" (通用 Unicode 符号)
glyph_mode = "nerd_font"

# 是否默认折叠长分支标签胶囊 (紧凑模式)
compact_refs = false

[log]
# 启用路径筛选 (Paths) 时，右侧变更文件树是否仅显示命中筛选路径的文件
show_only_affected_changes = true
# 是否高亮当前 Git 用户的提交 (My Commits)
highlight_my_commits = true
# 是否淡化显示已被 cherry-pick 到 HEAD 的提交 (Already Cherry-Picked)
highlight_cherry_picked = true
# 提交日志表是否显示 Author 列与 Date 列
show_author_column = true
show_date_column = true
# 是否将提交引用胶囊右对齐显示
align_labels_right = false
# 是否默认启用 LinearBek 线性化合并折叠视图
linear_bek = false

[diff]
# 是否开启代码语法高亮
syntax_highlight = true

# 制表符 (Tab) 宽度
tab_size = 4

# Diff 上下文保留行数
context_lines = 3

# 默认忽略空白模式: "default" (精确) | "ignore-change" (-b) | "ignore-all" (-w)
whitespace = "default"

# 是否默认开启 Diff 软换行
soft_wrap = false

[layout]
# --- Log 提交历史视图 ---
# 横向三栏权重: [左侧分支树, 中间提交图谱, 右侧变更与详情区]
log_ratio = [ 1, 3, 2 ]

# 右侧栏上下权重: [上方变更文件树, 下方提交详情/内联 Diff]
log_details_ratio = [ 3, 2 ]

# 右侧停靠 Diff 预览横向权重: [左侧主区(提交图谱+变更详情), 右侧停靠 Diff]
log_diff_right_ratio = [ 2, 1 ]

# 底部停靠 Diff 预览上下权重: [上方日志主区, 下方停靠 Diff]
log_diff_bottom_ratio = [ 3, 2 ]

# --- Commit 提交工作台 ---
# 横向两栏权重: [左侧本地变更与提交区, 右侧 Diff 预览]
commit_ratio = [ 1, 2 ]

# 左侧栏上下权重: [上方本地变更文件树, 下方提交说明]
commit_left_ratio = [ 3, 2 ]

# --- Stash 贮藏箱 ---
# 横向两栏权重: [左侧贮藏列表, 右侧变更与 Diff 预览]
stash_ratio = [ 2, 3 ]

# 右侧预览区上下权重: [上方变更文件树, 下方 Diff 预览]
stash_preview_ratio = [ 1, 2 ]

# --- Console 控制台 ---
# 横向两栏权重: [左侧命令历史, 右侧执行输出]
console_ratio = [ 2, 3 ]

# --- File History 文件历史窗口 ---
# 宽屏横向两栏权重: [左侧提交列表, 右侧 Diff 预览]
file_history_ratio = [ 2, 3 ]

# 窄屏上下两栏权重: [上方提交列表, 下方 Diff 预览]
file_history_vertical_ratio = [ 2, 3 ]

[annotate]
# 逐行注解 (Git Blame) 默认选项与热力边栏宽度
ignore_whitespace = false
# 移动与复制检测级别: "none" | "within-file" (-M) | "across-files" (-M -C)（亦兼容旧布尔值 true/false）
detect_moves = "none"
# 是否默认使用提交日期（committer-time）而非作者日期（author-time）
prefer_commit_date = false
gutter_width = 28

[push]
# 受保护目标分支名（支持 glob 模式），命中时禁用强推并隐藏被拒后的强推重试按钮
protected_branches = [ "main", "master" ]

[update]
# Update Project（<Alt-u>）同步当前分支的方式: "git-config"（遵循 branch.<name>.rebase / pull.rebase）| "merge" | "rebase"
mode = "git-config"
# 更新前自动贮藏本地改动（含未跟踪文件），结束后恢复；当前分支冲突时贮藏会保留，继续 / 中止后自动恢复
save_changes = true
# 同时快进其他设置了 upstream 的本地分支；无法快进（已分叉）的分支会在完成通知中列为“已跳过”
update_other_branches = true

[worktree]
# 新建 Worktree 的默认父目录（默认路径 <目录>/<仓库名>-<分支名>，分支名中的 / 换成 -）；留空 = 主工作区的父目录，支持 ~/
default_parent = ""
# 可选：新建 Worktree 成功后提示确认运行的初始化命令（如 "npm ci"）；留空禁用，jbgit 绝不静默执行
setup_command = ""

[git]
# 自定义外部 git 可执行文件路径；留空 = 按 PATH → 平台常见安装目录自动探测（配置无效路径时启动即给出明确错误提示）
executable = ""

[commit]
# 当仓库 core.autocrlf 未设为 true/input 且待提交的暂存文本包含 CRLF (\r\n) 换行符时发出提交前提醒
warn_crlf = true

[performance]
# 超过该索引条目阈值时按大仓库自适应降级 status 扫描开销
large_index_entries = 20000
# 工作区未暂存重命名检测策略: "auto" (大仓库自动关闭) | "always" | "never"
worktree_renames = "auto"
# 未跟踪目录递归展开策略: "always" | "auto" (大仓库仅展示目录项) | "never"
recurse_untracked_dirs = "always"
# 快照加载时是否预读提交正文（关闭可降低超大仓库冷启动内存，但文本搜索仅匹配标题/作者/哈希）
load_commit_bodies = true
```

> 提示：若在同一配置层中同时存在 `jbgit.toml` 和 `theme.toml`，且 `theme.toml` 未显式设置 `flavor` 或 `[glyphs].mode`，则会继承该层 `jbgit.toml` 中的 `[general].flavor` 与 `[graph].glyph_mode`。

### 4. `theme.toml`：主题预设与语义配色

内置三套预设调色板（完整色值位于配置目录 `themes/darcula.toml`、`themes/catppuccin-mocha.toml`、`themes/gruvbox-dark.toml`），既可直接编辑对应 `themes/<flavor>.toml` 覆盖，也可以新建 `themes/<自定义名称>.toml` 并在 `theme.toml` 中指定 `flavor = "<自定义名称>"`（未指定的颜色项自动回退至 Darcula）。

```toml
# 主题预设: "darcula" | "catppuccin-mocha" | "gruvbox-dark" | "<自定义名称>"
flavor = "darcula"

[glyphs]
# 图标模式: "nerd_font" | "unicode"
mode = "nerd_font"

[palette]
# 取消注释并填入十六进制 RGB (#RRGGBB) 即可单独覆写对应语义色
# bg_canvas = "#1E1F22"
# bg_panel = "#2B2D30"
# bg_elevated = "#313438"
# bg_selection_focused = "#2E436E"
# bg_selection_unfocused = "#393B40"
# bg_current_branch_row = "#252930"
# bg_hover = "#2D2F34"
# border_splitter = "#393B40"
# border_focused = "#3574F0"
# fg_primary = "#BCBEC4"
# fg_bright = "#DFE1E5"
# fg_muted = "#6F737A"
# fg_accent_link = "#548AF7"
# status_modified = "#6897BB"
# status_added = "#629755"
# status_untracked = "#D1675A"
# status_deleted = "#808080"
# status_renamed = "#70AEFF"
# status_conflicted = "#E55765"
# diff_add_bg = "#294436"
# diff_add_word_bg = "#37664B"
# diff_del_bg = "#482A2D"
# diff_del_word_bg = "#6E383E"
# diff_mod_bg = "#25364D"
# annotate_age_0 = "#3D3124"
# annotate_age_1 = "#2D3D30"
# annotate_age_2 = "#253746"
# annotate_age_3 = "#282E3B"
# annotate_age_4 = "#25262A"

[refs]
# 分支/标签胶囊样式覆写示例:
# head = { icon = "#E1C731", pill_bg = "#3A3728", pill_fg = "#F2E49B" }

[graph]
# 图谱 14 条轨道配色覆写示例:
# tracks = [ { fg = "#FF6285", muted_bg = "#603239" } ]
```

### 5. `keymap.toml`：快捷键预设与增量覆盖

`keymap.toml` 支持全局按键预设切换与分作用域（`[global]`、`[log]`、`[branch]`、`[commit]`、`[diff]`、`[stash]`、`[console]`）增量定制：

- **预设风格 (`preset`)**：
  - `"default"`：极简零冲突推荐预设（完整底表位于 `keymaps/default.toml`）。
  - `"jetbrains"`：贴近 IntelliJ IDEA 习惯（完整定义位于 `keymaps/jetbrains.toml`，自动开启 `tree_speed_search = true`，并补充 `<C-k>` 打开 Commit、`<C-S-k>` Push、`<C-t>` Pull、`<C-A-n>` 新建分支、`<A-m>` Amend 等映射）。注意 `<C-t>` 保持绑定 Pull 以不改变已有习惯；Update Project 在三套预设中统一为 `<A-u>`，如需 IDEA 的 `Ctrl+T = Update Project`，可在 `[global]` 中 `prepend_keymap = [{ on = "<C-t>", run = "update_project" }]`。
  - `"vim"`：Vim / Lazygit 风格（完整定义位于 `keymaps/vim.toml`，Log 中 `<C-d>`/`<C-u>` 半页翻滚、`d` 对比提交/开关差异、`z c`/`z o`/`z M`/`z R` 折叠 / 展开线性分支，Diff 中 `s`/`u` 暂存/取消暂存，`c` 切换上下文行数）。
  - **直接修改或自定义预设**：可编辑配置目录下的 `keymaps/default.toml`、`keymaps/jetbrains.toml`、`keymaps/vim.toml` 覆写内置预设（非 default 预设在磁盘存在对应 TOML 时以磁盘文件为准），也可新建 `keymaps/<自定义名称>.toml` 并在 `keymap.toml` 中设置 `preset = "<自定义名称>"`（会在 `keymaps/default.toml` 之上叠加）。
- **分支树直接打字搜索 (`tree_speed_search`)**：
  - `false`（默认）：在分支树面板按 `/` 或 `<Ctrl-f>` 进入搜索，单键 `j`/`k`/`c`/`q`/`r` 等保留导航与快捷操作。
  - `true`：在分支树面板直接打字即可实时过滤分支。
- **底栏快捷键提示定制 (`show_hint` 与 `footer_max_lines`)**：
  - 每条按键绑定支持可选参数 `show_hint = true | false`，用于控制该动作是否显示在底部快捷键提示栏中；省略时通过 `prepend_keymap` / `key_bindings` 改键会自动继承该动作原有的展示设置（使用 `run = "noop"` 解绑主按键时也会由存活别名继承），而 `append_keymap` 仅追加低优先级别名而不抢占主绑定的提示，显式设置则覆盖默认状态。
  - **展示顺序**：当前聚焦作用域（如 `diff`、`branch`、`commit`、`log`、`stash`、`console`）优先，其次为 `global`；同一作用域内严格按照 `keymap.toml` 中的声明顺序从左向右排列（`prepend_keymap` 会排在作用域前列，`?` 帮助固定锚定在底栏最右侧）。
  - **超出宽度与多行 (`footer_max_lines`)**：
    - `1`（默认）：单行展示，超出终端宽度的条目自动隐藏并在右侧显示 `+N` 提示角标。
    - `2` / `3` 等正整数：允许底栏按宽度自动折行多行展示，仅在超过最大行数时折叠隐藏并显示 `+N`。
    - `0`：隐藏底栏常驻快捷键提示（错误/通知 Toast、右键菜单禁用原因、图谱跳转悬停提示等临时状态仍会按需显示）。
- **增量覆盖与解绑**：
  - `prepend_keymap`（推荐）：只覆盖同按键或追加优先绑定，保留同一动作的其他默认别名。
  - `append_keymap`：追加在默认列表末尾。
  - `key_bindings`：替换同按键或同 `run` 动作的现有绑定。
  - `run = "noop"`：解绑指定按键。
  - `on` 支持单键字符串（如 `on = "r"`）或按键数组（如 `on = ["g", "g"]`）；修饰键支持 `<C-...>` / `<A-...>` / `<S-...>`（可组合如 `<C-S-k>`、`<C-A-k>`）。

示例：

```toml
preset = "default"
tree_speed_search = false
footer_max_lines = 1

[global]
prepend_keymap = [
    { on = "r", run = "noop" },                                  # 解绑默认 r
    { on = ["g", "s"], run = "git_status_probe", desc = "状态探测" },
    { on = "<C-t>", run = "update_project" },                    # IDEA 风格 Ctrl+T 改为 Update Project
]

[log]
prepend_keymap = [
    { on = "y", run = "copy_hash", show_hint = false },          # 隐藏底栏 y 复制哈希提示
    { on = "u", run = "uncommit", show_hint = true },            # 在底栏前置展示 u 撤回提交
]

[diff]
prepend_keymap = [
    { on = "s", run = "stage_selection", desc = "暂存选中块/行" },
    { on = "u", run = "unstage_selection", desc = "取消暂存选中块/行" },
]
```

### 6. 常用 `run` 动作标识符参考

| 作用域 | 可用 `run` 动作标识符 |
| :--- | :--- |
| `global` | `quit`, `help`, `context_menu`, `refresh`, `tab_commit`, `tab_log`, `tab_branches`, `tab_stash`, `tab_console`, `focus_next`, `focus_prev`, `toggle_branches_popup`, `toggle_branches_pane`, `git_fetch`, `git_pull`, `git_pull_dialog`, `git_rebase_dialog`, `git_push`, `update_project`, `configure_remotes`, `switch_repository`, `clone_repository`, `worktrees`, `resolve_conflicts`, `update_submodules`, `unshallow`, `bisect_good`, `bisect_bad`, `bisect_skip`, `bisect_reset`, `cycle_gpg_sign`, `goto_revision`, `palette`, `toggle_glyph_mode`, `cycle_graph_renderer`, `git_status_probe`, `noop` |
| `log` | `cursor_down`, `cursor_up`, `goto_top`, `goto_bottom`, `page_down_half`, `page_up_half`, `goto_revision`, `focus_filter`, `toggle_branches_pane`, `toggle_log_row_selection`, `compare_or_diff`, `swap_compare_direction`, `toggle_only_affected_changes`, `zoom_diff`, `copy_hash`, `copy_commit_message`, `copy_row_summary`, `checkout`, `new_branch`, `uncommit`, `reword`, `drop_commit`, `choose_parent`, `toggle_group`, `file_history`, `annotate`, `collapse_linear_branch`, `expand_linear_branch`, `collapse_all_linear_branches`, `expand_all_linear_branches`, `browse_revision`, `toggle_no_merges`, `toggle_linear_bek`, `log_speed_search`, `maximize_commit_message`, `open_reflog`, `log_load_more` |
| `branch` | `branch_search`, `toggle_favorite`, `checkout`, `new_branch`, `rename_branch`, `delete_branch`, `navigate_branch`, `open_reflog` |
| `commit` | `cursor_down`, `cursor_up`, `goto_top`, `goto_bottom`, `toggle_stage`, `stage_selection`, `unstage_selection`, `focus_commit_msg`, `commit`, `commit_and_push`, `toggle_amend`, `toggle_signoff`, `commit_history`, `select_all`, `discard_changes`, `toggle_group`, `edit_file`, `file_history`, `annotate`, `apply_patch`, `apply_patch_clipboard` |
| `diff` | `cursor_down`, `cursor_up`, `goto_top`, `goto_bottom`, `prev_hunk`, `next_hunk`, `prev_file`, `next_file`, `toggle_side_by_side`, `cycle_diff_context`, `cycle_diff_whitespace`, `toggle_diff_soft_wrap`, `diff_scroll_left`, `diff_scroll_right`, `diff_search`, `diff_search_next`, `diff_search_prev`, `cycle_diff_dock`, `zoom_diff`, `swap_compare_direction`, `toggle_stage`, `stage_selection`, `unstage_selection`, `toggle_visual`, `rollback_selection`, `undo_rollback`, `switch_diff_side`, `three_way_inspector` |
| `stash` | `cursor_down`, `cursor_up`, `goto_top`, `goto_bottom`, `stash_new`, `stash_apply`, `stash_pop`, `stash_drop`, `stash_branch`, `stash_clear`, `toggle_group` |
| `console` | `cursor_down`, `cursor_up`, `goto_top`, `goto_bottom`, `page_down_half`, `page_up_half` |

---

## 四、远端与凭据 (Remotes & Credentials)

### 1. 远端管理 (`Configure Remotes…`)

入口：全局 `<Alt-r>`、分支浮窗顶部 `⚙ Configure Remotes…`、分支树 `Remote` 分组或 `<remote>` 节点的右键菜单。窗口列出每个远端的获取 URL 与推送 URL（未单独设置时显示「与获取 URL 相同」）；URL 中的密码部分始终脱敏显示（`https://user:***@host/x.git`），控制台回放的命令行与错误信息同样脱敏。

| 快捷键 | 功能描述 |
| :--- | :--- |
| `↑` / `↓` / `j` / `k` / `g` / `G` | 选择远端（打开时默认选中当前分支 upstream 所在远端 → `origin` → 第一个远端） |
| `a` / `n` / `<Insert>` | 添加远端：先输入名称（按 `git check-ref-format` 规则校验并查重），再输入 URL 或本地路径（仅做非空 / 无空白校验，不联网验证）。成功后通知栏 `<Alt-z>` 可「获取 '<name>'」 |
| `r` / `<F2>` | 重命名远端（`git remote rename`，远端跟踪分支随之改名） |
| `e` / `<Enter>` / 双击 | 编辑获取 URL（`git remote set-url`） |
| `p` | 编辑推送 URL（`git remote set-url --push`） |
| `d` / `<Del>` | 删除远端（二次确认，提示将删除的 `refs/remotes/<name>/*` 数量及失去 upstream 的本地分支数） |
| `f` | 仅获取该远端（`git fetch --prune <name>`；全局 `f` 仍为 `fetch --all --prune`） |
| `<Esc>` / `q` | 关闭 |

未显式指定远端时（Push 默认目标、删除 Tag 时的远端探测等），jbgit 依次选择：当前分支 upstream 所在远端 → `origin` → 第一个已配置远端。

### 2. HTTPS / SSH 凭据交互（askpass 回调桥）

jbgit 运行的所有 git 子进程都设置 `GIT_TERMINAL_PROMPT=0`，不会在终端里卡住等待输入。没有配置 credential helper / ssh-agent 时，联网操作（Fetch、Pull、Push、Update Project、远端 Tag / 分支删除、单远端 Fetch）需要的用户名、密码、令牌或 SSH 私钥口令改由 jbgit 的 **Git 凭据** 对话框收集：

1. jbgit 启动时创建一个**仅当前用户可访问**的本地通道（Windows：带当前用户 SID 专属 DACL 的命名管道，拒绝远程客户端；Unix：`$XDG_RUNTIME_DIR` 或临时目录下 `0700` 目录中的 `0600` socket；不使用 TCP 端口），并生成 32 字节随机会话令牌。
2. 仅对联网命令（`fetch` / `pull` / `push` / `ls-remote`）设置 `GIT_ASKPASS`、`SSH_ASKPASS`（均指向当前 `jbgit` 可执行文件）、`SSH_ASKPASS_REQUIRE=force`、`JBGIT_ASKPASS_ADDR`、`JBGIT_ASKPASS_TOKEN`；本地命令不设置。
3. git / ssh 以 `jbgit "<提示>"` 形式回调；该进程出示令牌连回会话（令牌不匹配立即断开），提示在 TUI 中以对话框显示。多个 git 进程同时请求时按先后顺序逐个显示。
4. 提示含 `Password` / `passphrase` / `token` / `PIN` 时输入以 `•` 掩码显示，`<Tab>` 切换明文；支持粘贴；`<Ctrl-u>` 清空；`<Enter>` 发送；`<Esc>` 取消（askpass 以退出码 1 结束，git 随即失败）。遇到 OpenSSH 首次连接主机密钥确认提示（`Are you sure you want to continue connecting (yes/no/[fingerprint])?`）时，自动切换为**非掩码 SSH 主机密钥验证对话框**，完整换行展示服务器指纹原文并引导输入 `yes` / `no`。单次提示 120 秒未回答自动取消。
5. 认证失败（`Authentication failed`、`Permission denied (publickey)`、`could not read Username` 等）会弹出「身份验证失败」对话框，给出配置 credential helper（`git config --global credential.helper manager` 等）或 `ssh-add` 的指引；GPG 签名提交或签发签名标签（Create Tag 对话框支持按 `<Ctrl-s>` 勾选 `Sign tag with GPG (-s)`）因 `gpg-agent` / `pinentry` 失败时，自动弹出包含排查建议的 **GPG 签名失败** 提示。

**jbgit 不保存任何凭据**：输入值只在内存中（用后清零）转交给等待的 askpass 进程，不进入控制台、日志、草稿或任何文件；环境变量（含会话令牌）也从不在控制台显示。若你配置了 credential helper，git 会在认证成功后自行调用其 `store`，jbgit 不干预。

> 后台的「远端是否存在同名 Tag」探测（`ls-remote`，5 秒超时）不会弹出凭据对话框，需要认证时视为「未知」。
>
> 脚本化 / 沙箱测试可设置 `JBGIT_ASKPASS_RESPONDER=cancel`（总是取消）或 `JBGIT_ASKPASS_RESPONDER=value:<文本>`（总是回答该文本）替代对话框。

---

## 五、切换仓库与 Worktree (Repositories & Working Trees)

jbgit 同一时间只绑定**一个活动仓库**（多根同步模型不做，见 Spec 12 Issue 11），但可以在运行中快速切换，不必退出重开。

### 1. 切换仓库 (`<Alt-o>` / 点击头部仓库名)

切换器列出三类目标（当前仓库本身不列出），输入即模糊过滤（同时匹配名称与路径）：

| 标签 | 来源 |
| :--- | :--- |
| `最近` / `recent` | 用户级 `recent-repos.toml`（位于 `$JBGIT_CONFIG_DIR` 或全局配置目录；记录路径、显示名、最后打开时间，按时间倒序，最多 20 个）。已不存在的路径在加载时自动剔除，并在下次打开切换器时提示 |
| `worktree` | 当前仓库的其他 linked worktree（`git worktree list`；已失效的不列出） |
| `子模块` / `submodule` | 当前仓库中已初始化的子模块（`.gitmodules`） |

| 快捷键 | 功能描述 |
| :--- | :--- |
| `直接打字` / `<Backspace>` / `<Ctrl-u>` | 过滤 / 删除 / 清空 |
| `↑` / `↓` / `<Ctrl-p>` / `<Ctrl-n>` | 选择 |
| `<Enter>` / 双击 | 切换到选中仓库 |
| `<Ctrl-o>` | 输入任意路径（仓库内任意目录均可；相对路径相对于当前仓库所在目录） |
| `<Ctrl-l>` | 打开「克隆仓库…」对话框（也可从命令面板 `?` 中的 `clone_repository` 或非 Git 目录引导界面打开） |
| `<Del>` | 从最近仓库列表中移除选中项 |
| `<Esc>` | 关闭 |

切换流程：先校验目标（路径不存在 / 不是仓库时保持当前仓库并报错，同时将其移出最近列表）→ 取消全部在途只读请求、同步写出当前提交草稿 → 停止文件监听与引擎（等待在途写操作最多 5 秒，超时则提示其在后台继续）→ 绑定新仓库：重新加载按仓库叠加的 `.jbgit/` 配置层、分支面板设置与该仓库（该 worktree）的提交草稿。Log / Stage / Stash / Console 全部指向新仓库；旧仓库晚到的结果按会话代号丢弃，不会污染新仓库。

以下情况会先确认或拒绝：

- 有 Git 操作或提交仍在运行、或有未完成的变基 / 合并 / 推送 / 消息编辑对话框 → 确认对话框（默认「取消」）。
- 内置冲突合并窗口中有未保存的选择 → 拒绝，需先保存或放弃。

> 提交草稿按工作区保存（`<git-dir>/jbgit-commit-draft`）：主工作区与每个 linked worktree 各自保留自己的草稿。

### 1.5 非 Git 目录引导、初始化与克隆仓库（`git init` / `git clone`）

- **非 Git 目录启动**：在非 Git 仓库目录启动 `jbgit` 时不会直接报错退出，而是打开轻量欢迎模态框，提供三项入口：
  1. `Initialize Git Repository`（`i`）：在当前目录执行 `git init` 并直接切入工作台；
  2. `Clone Repository…`（`c`）：打开克隆对话框；
  3. `Open Recent / Switch Repository…`（`o`）：复用仓库切换器打开已有仓库。
- **克隆仓库对话框**：支持远程 URL（输入或粘贴后自动推导默认目录名）、目标目录、可选浅克隆深度（`--depth <N>`）及递归子模块（`--recurse-submodules`）。执行 `git clone --progress` 时自动复用 `GIT_ASKPASS` / `SSH_ASKPASS` 凭据对话框并实时展示进度，克隆完成后通过进程内重绑定自动切入新仓库。
- **`[git] executable` 探测与展示**：`jbgit` 按 `[git] executable` → `PATH` → 平台常见安装目录（Windows `Program Files\Git\{cmd,bin}\git.exe` / `%LOCALAPPDATA%\Programs\Git\cmd\git.exe`，Unix/macOS `/usr/local/bin/git` / `/opt/homebrew/bin/git` / `/usr/bin/git`）顺序解析 `git`，并在 `?` 帮助面板与 Console 启动摘要中显示实际解析到的版本与路径。
- **Windows CRLF 提交前提醒**：当 `[commit] warn_crlf = true` 且 `core.autocrlf` 未设为 `true`/`input` 时，提交前若检测到已暂存文本文件包含 `\r\n`（自动跳过二进制、LFS 指针以及 `.gitattributes` 已显式声明 `text`/`crlf`/`eol` 策略的文件），将弹出三选一确认框：**修复并提交**（设置 `core.autocrlf=true` 后提交）、**按原样提交**、或**不再提醒**（持久化写入本仓 `.jbgit/jbgit.toml` 的 `[commit] warn_crlf = false`）。

### 2. Working Trees (`<Alt-w>`)

入口：全局 `<Alt-w>`、分支浮窗顶部 `⎇ Working Trees…`。列表每项显示目录名、分支（或 `(detached)`）、HEAD 短哈希与路径，`●` 标记当前工作区，并附 `[主]` / `[已锁定：原因]` / `[可清理]` 标签。

| 快捷键 | 功能描述 |
| :--- | :--- |
| `↑` / `↓` / `j` / `k` / `g` / `G` | 选择 |
| `<Enter>` / `o` / 双击 | 在该 Worktree 中打开（即切换仓库，见上节） |
| `n` / `a` / `<Insert>` | 新建 Worktree（见下） |
| `l` | 锁定（可填原因，`git worktree lock --reason`） / 解锁；主工作区不可锁定 |
| `d` / `<Del>` | 删除（`git worktree remove`，确认框说明分支会保留）；主工作区、当前工作区、已锁定的不可删除。工作区有改动被 git 拒绝时，再次确认后以 `--force` 删除 |
| `p` | 清理（`git worktree prune --verbose`，确认框列出可清理项） |
| `y` | 复制路径 |
| `<Esc>` / `q` | 关闭 |

**新建 Worktree**：先选择检出方式——**从 HEAD 新建分支**（`-b <名称>`）、**已有分支**（只接受尚未在任何工作区检出的本地分支）、**游离 HEAD**（`--detach`）；再确认位置，默认 `<主工作区父目录>/<仓库名>-<分支名>`（由 `[worktree] default_parent` 改变父目录；已存在且非空的路径会被拒绝），并可按 `<Tab>` 勾选 `--force` 强制检出。创建成功后：
- 若仓库根目录存在 `.worktreeinclude`（使用 gitignore 语法声明需复制到新 worktree 的被忽略本地文件，如 `.env`、`config/local/`），jbgit 会自动将匹配文件复制到新工作区（不覆盖已有文件）；
- 若在 `jbgit.toml` 中配置了 `[worktree] setup_command`，创建成功后会弹出确认对话框（默认取消），经用户显式确认后才在新工作区目录中运行初始化命令；
- 通知栏 `<Alt-z>`「打开 '<名称>'」可立即切换过去。

其他入口：

- 分支菜单（分支树 / 分支浮窗）：未被检出的本地分支提供 `New Worktree…`；远程分支提供 `New Worktree from '<remote>/<branch>'…` 直接基于远程分支创建新 Worktree；已在其他 Worktree 检出的分支 Checkout 置灰并提供 `Open in Worktree '<name>'`。
- Log 提交右键菜单：`New Worktree from Here…`（从该提交新建分支或游离 HEAD）。
- git 拒绝检出（`is already checked out at` / `is already used by worktree at`）时弹出「分支已在其他工作区检出」对话框，可一键打开该 Worktree。

需要 git ≥ 2.17（`worktree remove` / `lock --reason`）；更低版本时菜单项与浮窗入口隐藏，`<Alt-w>` 仅提示所需版本（`?` 帮助中同样注明）。

---

## 六、提交图谱：折叠线性分支 (Collapse Linear Branches)

对应 IDEA 的 `Collapse / Expand Linear Branches`：把**线性片段**——每个提交恰好一个子提交、一个父提交，且不带任何 ref（分支 / 远端分支 / Tag）、不是 `HEAD` 的连续提交（至少 2 个）——折叠为一条虚线边，只保留分叉点、合并点与带 ref 的提交，快速看清拓扑。折叠只影响可见投影，提交本身与筛选、跳转、导航历史都不受影响。

| 操作 | 入口 |
| :--- | :--- |
| 全部折叠 / 全部展开 | `<Ctrl-Shift-->` / `<Ctrl-Shift-=>`（vim 预设 `z M` / `z R`）；`[⚙ Presentation]` 菜单 `Collapse Linear Branches` / `Expand Linear Branches` |
| 折叠选中处 | vim 预设 `z c`；提交右键菜单 `Collapse Linear Branch`。选中提交位于某片段中时折叠该片段并把光标移到片段上端；否则折叠其下方的片段 |
| 展开选中处 | vim 预设 `z o`；提交右键菜单 `Expand Linear Branch`（选中提交下方有折叠时替换折叠项） |
| 鼠标 | 单击折叠标记（空心节点 `○`；矢量渲染为空心圆环）或折叠虚线边，展开其下方被折叠的片段 |

- 折叠标记：片段上端的节点绘制为空心（`HEAD` 节点保留 HEAD 颜色）；字符网格与矢量两种渲染引擎一致。多条平行片段（菱形 / 章鱼合并的多个分支臂）共用一条虚线边，点击后一并展开。
- 「全部折叠」会保留选中提交所在的片段；之后刷新出的新片段也会自动折叠。跳转到被折叠的提交（`g r` 跳转、点击父提交哈希、导航后退 / 前进）时自动展开其所在片段；刷新后选中提交落入新片段时同样自动展开。
- 仅启用 `Branch` 筛选时仍允许折叠线性分支；同时启用文本/作者/日期/路径/`--no-merges` 筛选或开启 `First Parent Only` 时折叠不可用（菜单置灰并说明原因），关闭后恢复原折叠状态。鼠标悬停在展开的可折叠片段节点或边上时，将高亮整段可折叠线性分支。
- **LinearBek 视图模式 (`L` / `[⚙ Presentation]` → `LinearBek`)**：将双父合并提交的跨分支边折叠为从合入分支尾端指向主线父提交的虚线边，以单列线性展示历史；点击该虚线边或在对应行按 `Enter` 可展开单次合并，全部折叠/展开同步支持 LinearBek。

---

## 七、应用/创建 Patch 与按版本浏览 (Apply/Create Patch & Browse at Revision)

### 1. 应用 Patch (`Apply Patch…`)

入口：Stage 变更树右键菜单 `Apply Patch…` / `Apply Patch from Clipboard`，或 `[1 Commit]` 中 `<Alt-p>` / `<Alt-Shift-p>`。`Apply Patch…` 先询问 Patch 文件路径（相对仓库根目录或绝对路径，`Tab` 补全目录与文件名，文件不存在时红字提示）。

对话框打开时先以只读方式运行 `git apply --check --numstat [选项] --whitespace=nowarn` 预检（不修改工作区与暂存区），列出可勾选的受影响文件与增删行数；预检失败时显示 git 的首条错误，并在缺少 Git `index` 头的普通 unified patch 执行 `--3way` 失败时明确建议切回工作区模式或调整 `-p` 剥离层级。

| 快捷键 | 功能描述 |
| :--- | :--- |
| `1` / `2` / `3` / `m` | 应用模式：仅工作区（`git apply`） / 工作区 + 暂存区（`--index`） / 三方合并（`--3way`）；切换后重新预检 |
| `Space` / `a` | 勾选/取消勾选当前文件（`--include`） / 全选与取消全选（自动防抖重跑 `--check`） |
| `d` / `<Enter>` | 在右侧预览当前选中补丁文件的实际 unified diff（走 `syntect` 语法高亮）；`Tab` 在文件列表与预览间切换焦点 |
| `r` | 切换反向应用（`-R`），自动防抖重新预检 |
| `+` / `-` | 调整路径剥离层级（`-p0`..`-p9`，默认 `-p1`），自动防抖重新预检 |
| `D` | 设置目标子目录前缀（`--directory=<dir>`，`Enter` 生效、`Esc` 取消），自动防抖重新预检 |
| `j` / `k` / `<PageUp>` / `<PageDown>` | 移动选中文件（或焦点在预览时滚动 Diff；`<Ctrl-d>` / `<Ctrl-u>` 滚动预览） |
| `A` / `<Ctrl-Enter>` / 点击 `[ Apply ]` | 执行应用（预检未通过或未勾选任何文件时禁用） |
| `<Esc>` / `q` | 焦点在预览时返回文件列表；否则关闭弹窗 |

- 三方合并预检会标出将产生冲突的文件；应用后冲突文件出现在 `[1 Commit]` 的冲突分组中，通知栏 `<Alt-z>`「解决冲突」直接打开三栏合并窗口（单个冲突文件时）或冲突队列（多文件时）。
- 剪贴板读取：终端 OSC 52 只能写入剪贴板，因此 jbgit 调用系统工具读取（Windows `Get-Clipboard`、macOS `pbpaste`、Linux `wl-paste` / `xclip` / `xsel`），内容经 stdin 交给 `git apply -`；控制台只记录命令行，不记录 Patch 内容。整段统一为 CRLF 换行时会转换为 LF。环境变量 `JBGIT_CLIPBOARD_FILE` 可把"剪贴板"指向一个文件（测试 / 无图形环境）。

### 2. 创建 Patch (`Create Patch…`)

入口：Log 提交右键菜单 `Create Patch…` → `Save Patch Files…` / `Copy Patch to Clipboard`，以及 `[1 Commit]` Stage 变更树 / Diff 右键菜单 `Create Patch…` / `Create Patch to Clipboard`。

- 统一弹出「创建补丁」配置框：选择输出到 **文件/目录** 或 **剪贴板**、编辑目标路径（支持 `Tab` 补全；默认定位到上次目录或仓库外置父目录 `<repo>/..`，避免未经确认直接落盘到仓库根目录污染 `git status`）、可选开启 **Reverse Patch (`-R`)**。
- Stage 侧支持同时选中 **Staged + Unstaged + Untracked** 混合生成完整补丁：同文件同时存在已暂存与未暂存改动时自动合并输出 `HEAD..worktree` 净差异，未追踪文本/二进制文件以标准 unified new-file hunk 纳入补丁。

### 3. 按版本浏览仓库 (`Browse Repository at Revision`)

入口：Log 提交右键菜单 `Browse Repository at Revision`、提交表 `<Alt-t>`、`g r` / `<Ctrl-g>` 跳转弹窗中输入修订后按 `<Alt-Enter>`。

浮窗左侧为该提交的目录树（目录按层懒加载，目录在前），右侧为文件内容预览（语法高亮；符号链接明确标注 `symlink → <target>`；二进制、超过 4 MiB 的文件显示占位；子模块条目显示其指向的提交且只读）。

| 快捷键 | 功能描述 |
| :--- | :--- |
| `/` | 打开异步模糊路径搜索（`git ls-tree -r --name-only -z <oid>` + `nucleo` 匹配高亮；`<Enter>` 自动展开祖先目录并定位预览） |
| `Space` | 勾选/取消多选文件，用于批量 `Get Version` |
| `j` / `k` / `<PageUp>` / `<PageDown>` / `<Home>` / `G` | 移动（焦点在预览时滚动预览；`<Ctrl-d>` / `<Ctrl-u>` 半页滚动预览） |
| `→` / `l` / `<Enter>` / 双击 | 展开目录 / 聚焦文件预览 |
| `←` | 折叠目录或跳到父目录（预览中返回目录树） |
| `<Tab>` | 在目录树与预览之间切换焦点 |
| `c` | 与本地对比（文件：该文件 vs 工作区；目录：限定该子目录 vs 工作区），在 Log 变更树中打开 |
| `g` | 获取此版本（支持多选批量恢复；弹窗可选「仅恢复到工作区 `git restore --source=<oid> --worktree`」或「同时更新暂存区 `--staged --worktree`」） |
| `Y` | 复制预览文件内容（或符号链接目标）到剪贴板 |
| `e` | 导出文件到指定路径（`Export to File…`，直接读取 git 对象，支持文本、二进制及大文件） |
| `a` | 以该版本逐行追溯（Annotate），`<Esc>` 返回浏览器 |
| `h` | 从该版本开始查看文件 / 目录历史 |
| `y` | 复制相对路径 |
| `<Esc>` / `q` | 关闭（搜索打开时先关闭搜索） |

---

## 八、仓库扩展能力（签名验证、子模块、Git LFS 与浅克隆）

1. **提交签名验证详情与 Log 签名列**
   - `Commit Details` 面板通过 `git log -1 --format=%G?%x1f%GS%x1f%GK` 细分显示 8 种状态（`Good`、`GoodUnknownValidity (U)`、`Expired (X/Y)`、`Revoked (R)`、`CannotCheck (E)`、`Bad (B)`、`Unsigned (N)`、`Pending`），并展示签名者（`%GS`）与密钥 ID（`%GK`）。
   - `[⚙ Presentation]` → `Show Signature Column` 可选开启 Log 提交列表的签名图标列（默认关闭，仅对可视区提交批量验证），鼠标悬停图标弹出签名者与密钥详情提示。

2. **Submodule gitlink 暂存与更新 (`Update Submodules`)**
   - `[1 Commit]` 变更树为子模块展示 `[new commits]` / `[dirty workdir]` / `[uninitialized]` 徽标，右侧差异预览显示 `HEAD` / `Index` / `Working tree` 指向提交的 SHA；支持对子模块条目执行**整条暂存（`git add`）、取消暂存（`git reset`）与放弃指针变更**以正常提交子模块指针更新（仍禁止在右侧 Diff 做文本行级暂存）。
   - 右键菜单亦提供「在仓库切换器中打开子模块」及「更新子模块（`git submodule update --init --recursive`）」；工具集动作 `update_submodules` 可在命令面板中更新全部子模块。

3. **Git LFS 指针占位预览**
   - 检测 `.gitattributes` 中的 `filter=lfs` 规则（`RepoSnapshot.has_lfs` 与每文件 attribute 检查）；在 Log Diff、Stage Diff 及「按版本浏览仓库」遇到 LFS 指针文件时显示 `sha256` 前缀、字节大小与未下载提示，不触发 `git lfs smudge` 下载实体对象。

4. **浅克隆检测与完整拉取 (`Unshallow`)**
   - 当仓库为浅克隆（`Repository::is_shallow`）时，Log 副标题显示 `· shallow`，提交列表底部出现浅克隆边界标记与可点击的 `[Unshallow…]` 按钮；分支速查浮窗、Log 右键菜单与 `unshallow` 动作（命令面板）同样可发起确认。
   - 确认后执行 `git fetch --unshallow --progress <remote>`，控制台原地刷新 `\r` 进度输出。

5. **增量 Log 头部刷新与超 50k 提交渐进分页 (`LogLoadMore`)**
   - 常规提交、fetch 新增提交、分支快进、新建 tag、删除分支或小范围 reset/amend 触发 `RefsAndGraph` 刷新时，引擎对比新旧 ref tips，使用 `git2::Revwalk` 以旧 tips 为 `hide` 边界仅扫描新增差集，再通过 `VcsLogJoiner` 风格算法按拓扑序拼接到缓存的提交表头部并裁剪不可达孤立提交；若发生大规模历史改写、孤立分支或浅克隆加深/`unshallow`，则自动安全回退到全量加载。
   - 大仓库默认首屏加载 50,000 条提交，当仓库存在更早历史（`has_more_commits == true`）时，Log 副标题显示 `50000+`，提交表底端渲染可鼠标点击或用方向键/`G` 移入后按 `Enter` 触发的 `[Load More (+50,000 commits)…]` 虚拟尾行（也可通过 Log 右键菜单或命令目录动作 `log_load_more` 触发）；后台异步分页加载完成后无损保留当前滚动位置与选中提交 `Oid`。

6. **交互式二分查找 (`Git Bisect`)**
   - Log 提交右键菜单提供「从此处开始二分查找 (Bad)」与「标记为正常提交 (Bisect Good)」，支持先选 Bad 再选 Good 或在已开始的 Bisect 中随时标记历史提交。
   - 仓库进入 `Bisect` 状态后，顶部橙色状态横幅实时估算剩余修订数与约需步数，并提供可点击的 `[Good]` / `[Bad]` / `[Skip]` / `[Reset]` 按钮；对应动作 `bisect_good` / `bisect_bad` / `bisect_skip` / `bisect_reset` 均已注册至 `CommandCatalog`（可在 `?` 命令面板执行或于 `keymap.toml` 自定义绑定）。

---

## 九、VCS Log 高亮器、增强筛选、表内速查与 Git Reflog 恢复视图

1. **Log 高亮器与自动抑制**
   - **My Commits**（`[⚙ Presentation]` → `Highlight: My Commits`，配置项 `[log].highlight_my_commits = true`）：加粗高亮当前 Git 用户（`user.name` / `user.email`）的提交；当仓库仅有单一作者或已启用 `User: @me` 筛选时自动抑制。
   - **Already Cherry-Picked**（`[⚙ Presentation]` → `Highlight: Already Cherry-Picked`，配置项 `[log].highlight_cherry_picked = true`）：当分支筛选聚焦于单个非当前分支或执行 `Compare with Current` 时，在独立异步槽位（`RequestSlotId::LogCherryPicked`）运行 `git cherry <HEAD> <branch>`，将已存在等价补丁（`- <oid>`）的提交淡化显示。
   - **Current Branch 自动抑制**：当 `Branch` 筛选仅限定为 `HEAD` 或当前分支本身时，自动抑制整表当前分支背景色，避免全表同色干扰阅读。

2. **父/子提交候选选择器、增强筛选与表内速查**
   - **Go to Parent (`→`) / Go to Child (`←`)**：当目标父提交或可见子提交数量 ≥ 2 时，弹出编号候选列表（展示 `hash "subject" author date`），按 `1–9` 或 `Enter` 精准跳转。
   - **No-Merges 筛选 (`M` / 筛选栏 `[No Merges]`)**：等价于 `--no-merges`，快速过滤合并提交。
   - **增强 Branch 与 Hash 筛选**：`Branch` 筛选框原生支持 `a..b` 范围语法（如 `main..feat`）、内置 `Favorites` 快捷组（自动展开收藏分支），以及各筛选字段最近 10 条历史建议；文本搜索框支持空格分隔多个哈希前缀同时匹配。
   - **表内速查 (`s`)**：在 Log 提交表内高亮匹配提交（主题/作者/哈希/引用名）且**不隐藏**不匹配行，确认后用 `n` / `N` 步进定位，`Esc` 清除。
   - **列与详情增强**：`[⚙ Presentation]` 独立开关 `Author` 列（`[log].show_author_column`）、`Date` 列（`[log].show_date_column`）及 `Align Labels to Right`（引用胶囊右对齐，`[log].align_labels_right`）；Commit Details 支持点击正文中的 7–40 位提交哈希跳转，以及点击 `[⤢ Full]` 或按 `E` 打开可滚动的完整提交说明浮窗。

3. **Git Reflog 历史恢复视图 (`R`)**
   - 入口：Log 提交表/分支树按 `R`、Log 右键菜单 `Show Reflog (HEAD)…`、分支右键菜单 `Show Reflog for '<branch>'…` 或 `?` 命令面板 `open_reflog`。
   - 左侧展示分页 `git reflog show` 记录（`HEAD@{n}`、操作类型徽标 `reset`/`rebase`/`commit`/`checkout` 等、短哈希、相对时间与说明，按 `m` 加载更多），右侧同步展示对应提交的变更文件树与语法高亮差异预览（独立 `RequestSlot` 单飞取消）。
   - 支持对任意历史条目一键恢复：`c` 检出（游离 HEAD）、`n` 在此新建分支（恢复误 `reset --hard` 丢失的提交）、`p` 拣选（Cherry-Pick）、`r` 将当前分支重置至此、`y` 复制哈希、`Enter`/`l` 在 Log 中定位。
