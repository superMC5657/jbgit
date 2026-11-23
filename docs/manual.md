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
| `f` / `p` / `P` | 远程同步：**Fetch (`f`)** / **Pull (`p`)** / **Push… (`P`)** |
| `<Alt-u>` | **Update Project**（三套预设一致）：贮藏本地改动 → `fetch --all --prune` → 当前分支按 `[update].mode` 合并或变基 upstream → 其他跟踪分支仅快进 → 恢复改动；完成通知 `<Alt-z>`「查看」打开只读 Range 筛选的 Log。头部分支胶囊中的 `↓N` 计数可点击打开模式选择 |
| `<Alt-r>` | **Configure Remotes…**：远端列表（添加 / 重命名 / 编辑 URL / 删除 / 获取），详见[四、远端与凭据](#四远端与凭据-remotes--credentials) |
| `<Alt-o>` / 点击头部仓库名 | **切换仓库**：最近仓库 / 当前仓库的 linked worktree / 已初始化子模块，模糊过滤，`<Ctrl-o>` 输入任意路径；详见[五、切换仓库与 Worktree](#五切换仓库与-worktree-repositories--working-trees) |
| `<Alt-w>` | **Working Trees…**：Worktree 列表（打开 / 新建 / 锁定 / 删除 / 清理；需 git ≥ 2.17） |
| `<Ctrl-Alt-n>` | 从当前 HEAD 新建分支 |
| `<Alt-z>` / `<Alt-x>` | 触发当前底部通知栏的快捷动作 / 关闭通知 |
| `<F8>` / `<F12>` | 循环切换图谱渲染引擎（Auto / Vector / Cell） / 切换图标模式（Nerd Font / Unicode） |

### 提交日志视图 (Log View)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `j` / `k` 或 `<Down>` / `<Up>` | 上下移动光标浏览提交记录 |
| `<Shift-Up>` / `<Shift-Down>` | 扩展多选提交记录（`<Esc>` 取消多选） |
| `<PageUp>` / `<PageDown>` | 整页翻滚提交列表 / 变更树 |
| `g g` / `G`（或 `<Home>` / `<End>`） | 快速跳转至列表顶部 / 底部 |
| `<Left>` / `<Right>` / `<Shift-Right>` | *(提交表)* 跳转至子提交 / 第 1 父提交 / 第 2 父提交（多父时弹出选择） |
| `<Alt-Left>` / `<Alt-Right>` | *(提交表)* 导航历史后退 / 前进 |
| `<Enter>` / `<Esc>` | 提交表 → 右侧变更树 → Diff 预览逐级进入，`<Esc>` 逐级返回 |
| `/` / `<Ctrl-f>` / `<Ctrl-l>` | 聚焦分支与提交过滤输入框（搜索框内支持 `<Alt-r>` 正则、`<Alt-c>` 大小写敏感、`<Ctrl-u>` 清空） |
| `<Ctrl-b>` / `<Alt-b>` | 折叠 / 展开左侧分支树栏位 |
| `c` | 检出 (Checkout) 选中的提交或分支 |
| `n` | 在当前提交创建新分支 |
| `y` | 复制选中提交哈希 |
| `<Ctrl-d>` | 版本差异对比（选中两个提交时对比两者；单选时开关差异预览） |
| `<F4>` / `<Ctrl-Enter>` | *(变更树 / Diff)* 切换全屏差异预览 |
| `v` / `e` / `E` | *(变更树)* 切换按目录树分组 (`v`) / 全部展开 (`e`) / 全部折叠 (`E`) |
| `H` | *(变更树)* 查看选中文件的完整提交历史（File History） |
| `<Alt-g>` | *(变更树 / Diff)* 打开 Git Blame 逐行追溯（Annotate）浮窗 |
| `g r` / `<Ctrl-g>` | 按提交哈希、分支名或 Tag 快速定位跳转 |
| `m` | 为合并提交选择 / 切换父提交 |
| `u` / `<F2>` / `<Del>` | 撤回最近提交 (Uncommit) / 修改说明 (Reword) / 丢弃提交 (Drop) |

### 分支树面板 (Log Branches Pane)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `/` / `<Ctrl-f>`（`<Esc>` 退出） | 进入分支树模糊搜索（可在 `keymap.toml` 设为 `tree_speed_search = true` 开启直接打字速查） |
| `<Left>` / `<Right>` / `<PageUp>` / `<PageDown>` | 折叠或展开分组节点 / 翻页滚动 |
| `<Space>` | 添加 / 取消收藏当前分支（置顶显示） |
| `<Enter>` | 按选中分支过滤提交日志 |
| `J` | 跳转至选中分支对应的提交记录 |
| `c` / `n` / `<Ctrl-Alt-n>` / `<F2>` / `d` / `<Del>` | 检出 (`c`) / 新建分支 (`n`/`Ctrl+Alt+N`) / 重命名 (`F2`) / 删除分支或 Tag (`d`/`Del`) |
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
| 顶部 `⚙ Configure Remotes…` | 打开远端管理窗口（同 `<Alt-r>`） |
| 顶部 `⎇ Working Trees…` | 打开 Worktree 管理窗口（同 `<Alt-w>`；git < 2.17 时不显示） |
| 分支行右侧 `⎇ <worktree>` | 该分支已在其他 Worktree 检出：子菜单中 Checkout 置灰并说明原因，改为提供 `Open in Worktree '<name>'` |

### 提交工作台 (Commit View)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `<Space>`（或 `s`/`+`、`u`/`-`） | **暂存 / 取消暂存** 当前选中的文件或代码块 |
| `<Enter>` / `<Left>` / `<Right>` | 折叠/展开目录容器，或在文件上聚焦右侧 Diff 预览 |
| `c` | 聚焦提交说明编辑器（变更树与 Commit Diff 预览窗格均可直接按 `c`；编辑区内按 `<Esc>` 返回变更树） |
| `<Ctrl-s>` / `<Ctrl-Enter>` / `<Alt-Enter>` | 执行提交（编辑区内 `<Ctrl-Alt-k>` 提交并推送） |
| `A`（或编辑区内 `<Alt-a>` / `<Alt-m>`） | 切换追加至上一提交 (`Amend`) 模式 |
| `<Alt-s>` / `<Ctrl-m>` | 切换 Signed-off-by 签名 / 打开历史提交说明选择器 |
| `<Ctrl-a>` | 全选变更文件 |
| `d` / `<Del>` / `<Ctrl-Alt-z>` | 放弃选中文件或代码块的本地修改 (Discard Changes) |
| `v` / `e` / `H` / `<Alt-g>` | 按目录树分组 (`v`) / 外部编辑器 (`e`) / 文件历史 (`H`) / Git Blame 逐行追溯 (`<Alt-g>`) |
| `<Ctrl-e>` / `<Ctrl-z>` / `<Ctrl-y>` / `<Ctrl-w>` | *(提交说明编辑器内)* 外部编辑器编辑说明 / 撤销 / 重做（亦支持 `<Ctrl-Shift-z>`）/ 删词（亦支持 `<Alt-Backspace>`） |

### 统一差异对比 (Diff View — Log / Commit / Stash)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `j` / `k` / `<PageUp>` / `<PageDown>` / `g g` / `G`（或 `<Home>` / `<End>`） | 上下移动、翻页及跳转顶部 / 底部（含全屏 Diff 模式） |
| `[` / `]` | 跳转至上一个 / 下一个 Hunk 代码块 |
| `{` / `}` | 跳转至上一个 / 下一个变更文件 |
| `S` | 切换单列统一对比 (Unified) / 双列并排对比 (Side-by-Side) |
| `w` | 循环切换上下文保留行数 (3 行 / 10 行 / 全量) |
| `m` / `z`（或 `<F4>` / `<Ctrl-Enter>`） | *(Log Diff)* 切换停靠位置 (`m`) / 全屏差异预览 (`z`/`F4`/`Esc`)；全屏下按 `P` 切换合并父提交 |
| `<Space>` / `v` / `d` | *(Commit Diff)* 暂存/取消 (`Space`/`s`/`u`) · 行级可视选择 (`v`) · 回滚选区 (`d`/`Del`，`<Ctrl-z>` 撤销) |
| `x` / `i` / `c` / `<Esc>` | *(Commit Diff)* 切换未暂存与已暂存侧 (`x`) · 三方检视 (`i`) · 聚焦提交说明 (`c`) · 退出可视选择或返回变更树 (`Esc`) |

### 贮藏箱 (Stash View)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `<Enter>` / `<Right>` / `<Esc>` | 安全切换焦点至右侧差异预览（不会误操作应用贮藏），`<Esc>` 返回贮藏列表 |
| `a` / `o` | 应用选中贮藏 (`a`) / 应用并弹出 (`o`) |
| `n` / `B` / `d` / `C` | 新建贮藏 (`n`) / 从贮藏创建分支 (`B`) / 删除 (`d`/`Del`) / 清空 (`C`) |
| `v` / `e` / `E` / `<Left>` / `<Right>` | *(右侧变更树)* 按目录分组 (`v`) / 全展开 (`e`) / 全折叠 (`E`) / 单项折叠展开 |

### 控制台 (Console View)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `j` / `k` 或 `<Down>` / `<Up>` | 在命令列表切换命令，或在输出详情逐行滚动 |
| `g g` / `G`（或 `<Home>` / `<End>`） | 快速跳转至顶部 / 底部 |
| `<Ctrl-d>` / `<Ctrl-u>` / `<PageDown>` / `<PageUp>` | *(输出详情)* 半页 / 整页滚动输出日志（命令列表支持 `<PageUp>`/`<PageDown>` 翻 10 行） |
| `<Enter>` / `<Esc>` | 进入右侧输出详情面板 / 返回左侧命令历史列表 |
| `p` | *(命令列表)* 执行 `git status --short --branch` 状态探测（可通过 `git_status_probe` 重绑） |

### 文件历史与冲突合并 (File History & Conflict Merge)

**文件历史浮窗**（在 Log 或 Commit 变更树按 `H` 打开）：

| 快捷键 | 功能描述 |
| :--- | :--- |
| `<Tab>` / `<Shift-Tab>` | 在提交列表与差异预览之间切换焦点 |
| `j` / `k` / `<PageUp>` / `<PageDown>` / `g g` / `G`（或 `<Home>` / `<End>`） | 上下移动、翻页及跳转顶部 / 底部 |
| `<Shift-Up>` / `<Shift-Down>` | 扩展多选两个提交区间进行对比 |
| `<Enter>` | 跳转并在主 Log 视图定位到当前选中提交 |
| `f` / `l` / `b`（或 `<Alt-g>`） | 切换追踪重命名 (`f`) / 对比本地工作区 (`l`) / 在当前提交打开 Annotate 逐行追溯 (`b`/`Alt-g`) |
| `S` / `w` / `z`（或 `<F4>`） | 切换并排对比 (`S`) / 循环上下文行数 (`w`) / 全屏差异预览 (`z`/`F4`) |
| `<Esc>` | 退出全屏 Diff 或关闭文件历史浮窗 |

**Git Blame 逐行追溯浮窗 (Annotate)**（在 Log / Commit 变更树或 Diff 预览按 `<Alt-g>`，或由上下文菜单 / File History 按 `b` 进入）：

| 快捷键 | 功能描述 |
| :--- | :--- |
| `j` / `k` / `<PageUp>` / `<PageDown>` / `g g` / `G` | 上下逐行浏览源码与提交年代热力条 / 跳转首尾行 |
| `:` / `<Ctrl-g>` | 输入目标行号快速跳转（Go to Line） |
| `v` / `V`（或 `<Shift-Up>` / `<Shift-Down>`） | 开关可视行选择并扩展行区间（在 Commit Diff 预览同样支持 `v` 选中块/行） |
| `H` | 对当前光标行或可视选中行区间执行 `git log -L` 查看 **所选代码行历史 (Selection History)** |
| `p` / `P`（或 `<Backspace>`） | 追溯当前选中行的上一修订版本 (`Annotate Previous Revision`) / 返回导航栈 |
| `w` / `m` | 切换忽略空白字符 (`-w`) / 跨文件代码移动与复制追踪 (`-M -C`) |
| `d` / `<Tab>` | 开关底部内联 Diff 预览 / 在源码区与底部 Diff 之间切换焦点 |
| `<Enter>` / `y` / `.` | 跳转到主 Log 对应提交 / 复制该行提交哈希 / 打开 Annotate 上下文菜单 |
| `<Esc>` | 清除行跳转输入或退出可视选区，否则关闭 Annotate 浮窗 |

**三栏冲突合并浮窗**（在冲突文件上下文菜单或横幅进入）：

| 快捷键 | 功能描述 |
| :--- | :--- |
| `j` / `k` / `<PageUp>` / `<PageDown>` | 滚动冲突视图 |
| `n` / `<Tab>`、`p` / `<Shift-Tab>` | 跳转至下一个 / 上一个冲突块 |
| `1` / `y`、`2` / `t`、`3` / `b`、`0` / `x` | 当前冲突块：采纳己方 (Ours) / 采纳对方 (Theirs) / 保留双方 / 重置为未解决 |
| `Y` / `T` | 全部冲突块一键采纳己方 / 采纳对方 |
| `m` / `e` | 在外部 Mergetool / 外部编辑器中打开冲突文件 |
| `<Enter>` / `<Ctrl-s>` / `<Esc>` | 保存解决结果并暂存 / 取消关闭 |

### 菜单、命令面板与交互对话框 (Menus, Command Palette & Dialogs)

| 组件 | 常用快捷键 |
| :--- | :--- |
| **上下文级联菜单** | `j`/`k`/`↑`/`↓`/`Home`/`End` 选择；`→` 展开子菜单；`←`/`Esc` 关闭当前层；`<Enter>`/`<Space>` 确认；`<Del>` 触发删除项；按高亮助记字母直达 |
| **快捷键命令面板 (`?`)** | 直接打字模糊搜索；`↑`/`↓`/`PgUp`/`PgDn` 选行；`<Enter>` 执行高亮命令；`<Ctrl-u>` 清空；`<Esc>`（或空查询按 `?`）关闭 |
| **交互式变基待办 (Rebase Todo)** | `j`/`k`/`↑`/`↓` 选行；`J`/`K` 或 `<Alt/Shift-Up/Down>` 调整提交顺序；`<Space>` 循环动词；`p` pick / `r` 或 `w` reword / `s` squash / `f` fixup / `d` drop；`<Enter>` 执行；`<Esc>` 取消 |
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

**完整 Push 推送浮窗**（全局按 `P`，或在分支/提交菜单选择 `Push…` / `Push up to Here` 打开；包含 `Target | Outgoing Commits | Changed Files / Diff` 三栏预览）：

| 快捷键 | 功能描述 |
| :--- | :--- |
| `<Tab>` / `<Shift-Tab>` | 在 `Target` / `Outgoing Commits` / `Changed Files` 三栏之间循环切换焦点 |
| `e` / `r` | 内联编辑目标分支名（支持 `<Tab>` 补全远端分支、`↑`/`↓` 选候选、`<Ctrl-u>` 清空、`<Enter>` 确认并刷新待推提交） / 切换远端仓库 (`r`) |
| `j` / `k` / `<PageUp>` / `<PageDown>` / `g g` / `G` | 浏览待推提交列表、右侧变更文件树或文件差异 |
| `d` / `<Enter>` *(Files 栏)* | 打开或关闭当前选中变更文件的 Diff 预览（Diff 内支持 `S` 并排对比、`c` 上下文行数、`[`/`]` Hunk 跳转） |
| `t` / `f` / `u` | 循环切换 Tag 模式 (`None` / `Current branch (--follow-tags)` / `All (--tags)`) / 强推模式 (`No` / `With lease` / `Force`，受 `[push].protected_branches` 保护拦截) / 勾选 `Set upstream` |
| `<Enter>` / `<Ctrl-Enter>` / `<Esc>` | 执行推送（若因非快进被拒绝，弹窗提供 `Merge & Retry` / `Rebase & Retry` 串行拉取合入并重试） / 取消关闭 |

---

## 三、配置体系指南 (Configuration)

jbgit 采用与 Yazi 类似的**多文件 TOML + 分层覆盖**配置体系，所有配置文件均可按需缺省或仅写需修改的字段。

### 1. 配置文件生成与查看

```bash
# 在全局用户配置目录下生成带中文注释的默认配置模板（已存在的文件会跳过）
jbgit --init-config

# 将内置默认配置打印到标准输出
jbgit --print-default-config jbgit     # 打印 jbgit.toml 默认内容
jbgit --print-default-config theme     # 打印 theme.toml 默认内容
jbgit --print-default-config keymap    # 打印 keymap.toml 默认内容
```

### 2. 配置文件路径与 4 级覆盖优先级

按优先级**由高到低**逐层叠加覆盖（仅覆盖显式指定的字段，未指定字段保留下层值）：

1. **`JBGIT_CONFIG_DIR` 环境变量目录**：`$JBGIT_CONFIG_DIR/{jbgit,theme,keymap}.toml`
2. **仓库级配置目录**：`<repo-workdir>/.jbgit/{jbgit,theme,keymap}.toml`（主配置亦支持 `.jbgit/config.toml` 别名）
3. **全局用户配置目录**：
   - **Windows**: `%APPDATA%\jbgit\config\`（若未设置 `APPDATA` 则回退至 `%USERPROFILE%\.config\jbgit\`）
   - **Linux / macOS**: `$XDG_CONFIG_HOME/jbgit/`（默认 `~/.config/jbgit/`）
4. **程序内置默认值**

配置目录包含三个独立文件：
- `jbgit.toml`：通用设置、图谱渲染后端、Diff 选项、视图栏位比例
- `theme.toml`：主题预设、图标模式与语义调色板覆写
- `keymap.toml`：快捷键预设与按键映射覆写

### 3. `jbgit.toml`：通用行为、图谱与布局比例

```toml
[general]
# 界面语言: "zh-CN" (简体中文), "en-US" (English), "auto" (自动跟随系统环境变量)
language = "zh-CN"

# 启动时默认打开的工作区标签页: "log", "commit", "branches", "stash", "console"
default_tab = "log"

# 主题预设风格: "darcula", "catppuccin-mocha", "gruvbox-dark"
flavor = "darcula"

[graph]
# 提交图渲染引擎: "auto" (自动侦测图形协议), "vector" (Kitty/Sixel/iTerm2 矢量图), "cell" (字符网格)
backend = "auto"

# 图标模式: "nerd_font" (需要终端安装 Nerd Font), "unicode" (通用 Unicode 符号)
glyph_mode = "nerd_font"

# 是否默认折叠长分支标签胶囊 (紧凑模式)
compact_refs = false

[diff]
# 是否开启代码语法高亮
syntax_highlight = true

# 制表符 (Tab) 宽度
tab_size = 4

# Diff 上下文保留行数
context_lines = 3

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
detect_moves = false
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
```

> 提示：若在同一配置层中同时存在 `jbgit.toml` 和 `theme.toml`，且 `theme.toml` 未显式设置 `flavor` 或 `[glyphs].mode`，则会继承该层 `jbgit.toml` 中的 `[general].flavor` 与 `[graph].glyph_mode`。

### 4. `theme.toml`：主题预设与语义配色

```toml
# 主题预设: "darcula" | "catppuccin-mocha" | "gruvbox-dark"
flavor = "darcula"

[glyphs]
# 图标模式: "nerd_font" | "unicode"
mode = "nerd_font"

[palette]
# 取消注释并填入十六进制 RGB (#RRGGBB) 即可单独覆写对应语义色
# bg_canvas = "#1E1F22"
# bg_panel = "#2B2D30"
# bg_elevated = "#2B2D30"
# bg_selection_focused = "#2E436E"
# bg_selection_unfocused = "#35383C"
# bg_current_branch_row = "#25324D"
# bg_hover = "#2E3136"
# border_splitter = "#35373B"
# border_focused = "#3574F0"
# fg_primary = "#BCBEC4"
# fg_bright = "#DFE1E5"
# fg_muted = "#707278"
# fg_accent_link = "#548AF7"
# status_modified = "#6897BB"
# status_added = "#629755"
# status_untracked = "#808080"
# status_deleted = "#808080"
# status_renamed = "#3592C4"
# status_conflicted = "#C75450"
# diff_add_bg = "#294436"
# diff_add_word_bg = "#385E42"
# diff_del_bg = "#483033"
# diff_del_word_bg = "#6E3B3B"
# annotate_age_0 = "#3D3124"
# annotate_age_1 = "#2D3D30"
# annotate_age_2 = "#253746"
# annotate_age_3 = "#282E3B"
# annotate_age_4 = "#25262A"
```

### 5. `keymap.toml`：快捷键预设与增量覆盖

`keymap.toml` 支持全局按键预设切换与分作用域（`[global]`、`[log]`、`[branch]`、`[commit]`、`[diff]`、`[stash]`、`[console]`）增量定制：

- **预设风格 (`preset`)**：
  - `"default"`：极简零冲突推荐预设（默认）。
  - `"jetbrains"`：贴近 IntelliJ IDEA 习惯（自动开启 `tree_speed_search = true`，并补充 `<C-k>` 打开 Commit、`<C-S-k>` Push、`<C-t>` Pull、`<C-A-n>` 新建分支、`<A-m>` Amend 等映射）。注意 `<C-t>` 保持绑定 Pull 以不改变已有习惯；Update Project 在三套预设中统一为 `<A-u>`，如需 IDEA 的 `Ctrl+T = Update Project`，可在 `[global]` 中 `prepend_keymap = [{ on = "<C-t>", run = "update_project" }]`。
  - `"vim"`：Vim / Lazygit 风格（Log 中 `<C-d>`/`<C-u>` 半页翻滚、`d` 对比提交/开关差异，Diff 中 `s`/`u` 暂存/取消暂存，`c` 切换上下文行数）。
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
| `global` | `quit`, `help`, `context_menu`, `refresh`, `tab_commit`, `tab_log`, `tab_branches`, `tab_stash`, `tab_console`, `focus_next`, `focus_prev`, `toggle_branches_popup`, `toggle_branches_pane`, `git_fetch`, `git_pull`, `git_push`, `update_project`, `configure_remotes`, `switch_repository`, `worktrees`, `goto_revision`, `palette`, `toggle_glyph_mode`, `cycle_graph_renderer`, `git_status_probe`, `noop` |
| `log` | `cursor_down`, `cursor_up`, `goto_top`, `goto_bottom`, `page_down_half`, `page_up_half`, `goto_revision`, `focus_filter`, `toggle_branches_pane`, `compare_or_diff`, `zoom_diff`, `copy_hash`, `checkout`, `new_branch`, `uncommit`, `reword`, `drop_commit`, `choose_parent`, `toggle_group`, `file_history`, `annotate` |
| `branch` | `branch_search`, `toggle_favorite`, `checkout`, `new_branch`, `rename_branch`, `delete_branch`, `navigate_branch` |
| `commit` | `cursor_down`, `cursor_up`, `goto_top`, `goto_bottom`, `toggle_stage`, `stage_selection`, `unstage_selection`, `focus_commit_msg`, `commit`, `commit_and_push`, `toggle_amend`, `toggle_signoff`, `commit_history`, `select_all`, `discard_changes`, `toggle_group`, `edit_file`, `file_history`, `annotate` |
| `diff` | `cursor_down`, `cursor_up`, `goto_top`, `goto_bottom`, `prev_hunk`, `next_hunk`, `prev_file`, `next_file`, `toggle_side_by_side`, `cycle_diff_context`, `cycle_diff_dock`, `zoom_diff`, `toggle_stage`, `stage_selection`, `unstage_selection`, `toggle_visual`, `rollback_selection`, `undo_rollback`, `switch_diff_side`, `three_way_inspector` |
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
4. 提示含 `Password` / `passphrase` / `token` / `PIN` 时输入以 `•` 掩码显示，`<Tab>` 切换明文；支持粘贴；`<Ctrl-u>` 清空；`<Enter>` 发送；`<Esc>` 取消（askpass 以退出码 1 结束，git 随即失败）。单次提示 120 秒未回答自动取消。
5. 认证失败（`Authentication failed`、`Permission denied (publickey)`、`could not read Username` 等）会弹出「身份验证失败」对话框，给出配置 credential helper（`git config --global credential.helper manager` 等）或 `ssh-add` 的指引。

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
| `<Del>` | 从最近仓库列表中移除选中项 |
| `<Esc>` | 关闭 |

切换流程：先校验目标（路径不存在 / 不是仓库时保持当前仓库并报错，同时将其移出最近列表）→ 取消全部在途只读请求、同步写出当前提交草稿 → 停止文件监听与引擎（等待在途写操作最多 5 秒，超时则提示其在后台继续）→ 绑定新仓库：重新加载按仓库叠加的 `.jbgit/` 配置层、分支面板设置与该仓库（该 worktree）的提交草稿。Log / Stage / Stash / Console 全部指向新仓库；旧仓库晚到的结果按会话代号丢弃，不会污染新仓库。

以下情况会先确认或拒绝：

- 有 Git 操作或提交仍在运行、或有未完成的变基 / 合并 / 推送 / 消息编辑对话框 → 确认对话框（默认「取消」）。
- 内置冲突合并窗口中有未保存的选择 → 拒绝，需先保存或放弃。

> 提交草稿按工作区保存（`<git-dir>/jbgit-commit-draft`）：主工作区与每个 linked worktree 各自保留自己的草稿。

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

**新建 Worktree**：先选择检出方式——**从 HEAD 新建分支**（`-b <名称>`）、**已有分支**（只接受尚未在任何工作区检出的本地分支）、**游离 HEAD**（`--detach`）；再确认位置，默认 `<主工作区父目录>/<仓库名>-<分支名>`（由 `[worktree] default_parent` 改变父目录；已存在且非空的路径会被拒绝）。创建成功后通知栏 `<Alt-z>`「打开 '<名称>'」可立即切换过去。

其他入口：

- 分支菜单（分支树 / 分支浮窗）：未被检出的本地分支提供 `New Worktree…`；已在其他 Worktree 检出的分支 Checkout 置灰并提供 `Open in Worktree '<name>'`。
- Log 提交右键菜单：`New Worktree from Here…`（从该提交新建分支或游离 HEAD）。
- git 拒绝检出（`is already checked out at` / `is already used by worktree at`）时弹出「分支已在其他工作区检出」对话框，可一键打开该 Worktree。

需要 git ≥ 2.17（`worktree remove` / `lock --reason`）；更低版本时菜单项与浮窗入口隐藏，`<Alt-w>` 仅提示所需版本（`?` 帮助中同样注明）。
