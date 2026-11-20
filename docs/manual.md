# jbgit 操作与配置手册

本文档详细说明 **jbgit** 的交互操作方式、各视图快捷键以及多层配置文件体系。如果只需要快速安装与启动，请先参阅项目根目录的 [README.md](../README.md)。

---

## 目录

- [一、交互设计概览](#一交互设计概览)
- [二、快捷键速查表 (Keybindings)](#二快捷键速查表-keybindings)
  - [全局与统一导航 (Global & Navigation)](#全局与统一导航-global--navigation)
  - [双语义前缀族 (`g` 跳转 / `z` 视图)](#双语义前缀族-g-跳转--z-视图)
  - [提交日志视图 (Log View)](#提交日志视图-log-view)
  - [分支树面板 (Log Branches Pane)](#分支树面板-log-branches-pane)
  - [提交工作台 (Commit View)](#提交工作台-commit-view)
  - [统一差异对比 (Diff View — Log / Commit / Stash)](#统一差异对比-diff-view--log--commit--stash)
  - [贮藏箱 (Stash View)](#贮藏箱-stash-view)
  - [控制台 (Console View)](#控制台-console-view)
  - [模态弹窗与专项视图 (Modals)](#模态弹窗与专项视图-modals)
- [三、配置体系指南 (Configuration)](#三配置体系指南-configuration)
  - [1. 配置文件生成与查看](#1-配置文件生成与查看)
  - [2. 配置文件路径与 4 级覆盖优先级](#2-配置文件路径与-4-级覆盖优先级)
  - [3. `jbgit.toml`：通用行为、图谱与布局比例](#3-jbgittoml通用行为图谱与布局比例)
  - [4. `theme.toml`：主题预设与语义配色](#4-themetoml主题预设与语义配色)
  - [5. `keymap.toml`：快捷键预设与增量覆盖](#5-keymaptoml快捷键预设与增量覆盖)
  - [6. 常用 `run` 动作标识符参考](#6-常用-run-动作标识符参考)

---

## 一、交互设计概览

jbgit 采用**统一零冲突现代键位方案（`preset = "default"`）**，以 `KeymapConfig` 为单一事实源，通过三层渐进式暴露机制降低记忆门槛：

- **页内自适应动态键帽**：按钮与工具栏（如 `提交 [Ctrl+S]`、`▶ 应用 [a]`、`◫ 并排 [S]`、`[1] 我方`）自动从 `keymap.toml` 读取并展示当前绑定，空间紧张时自动隐藏键帽避免截断。
- **底部智能可点击胶囊栏**：屏幕底部根据当前焦点面板与运行时状态（如多选对比、合并提交、Visual 行选）分层展示最核心快捷键，支持自动成对合并（如 `[/] 代码块`、`a/A 应用/弹出`、`p/P 拉/推`），且**支持鼠标直接点击**胶囊执行动作、点击 `[g 跳转…]` / `[z 视图…]` 唤出 Which-Key、点击 `[+N]` 或 `[? 帮助]` 打开快捷键面板。
- **Which-Key 前缀提示卡**：按下 `g`（跳转族）或 `z`（视图/布局/折叠族）后自动弹出可用子键列表，支持继续按键或直接鼠标点击执行，`Esc` 取消。
- **可搜索命令与快捷键面板 (`?`)**：在任意界面按 `?` 打开，置顶当前面板可执行动作，支持中英文及按键名检索，按 `Enter` 直接执行。
- **上下文操作菜单 (`.`)**：按 `.`、`<Shift-F10>`、`Menu` 或点击**鼠标右键**，随时打开当前选中项的完整操作菜单，右侧动态对齐最新快捷键。
- **模态窗统一胶囊底栏**：文件历史、三方冲突合并、交互式变基、消息编辑器等弹窗均采用与主界面一致的胶囊提示条。

---

## 二、快捷键速查表 (Keybindings)

> 下表以默认预设 `preset = "default"` 为准；全视图遵守统一动词语义（`d`/`Del` 删弃回滚、`r`/`F2` 重命名/改说明、`n` 新建、`e` 外编、`H` 文件历史、`v` 行选、`m` 合并父提交），且局部键位永不静默遮蔽全局命令。

### 全局与统一导航 (Global & Navigation)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `q` / `<Ctrl-c>` | 退出 jbgit |
| `?` | 打开可搜索快捷键帮助 / 命令面板（按 `Enter` 直接执行高亮命令） |
| `.` / `<Shift-F10>` / `Menu` / `鼠标右键` | 打开当前选中项的操作菜单 |
| `R` / `<F5>` | 强制刷新 Git 仓库状态 |
| `1` / `2` / `3` / `4`（或 `<Alt-1>` – `<Alt-4>`） | 切换工作区：**Commit (`1`)** / **Log (`2`)** / **Stash (`3`)** / **Console (`4`)** |
| `<Tab>` / `<Shift-Tab>` | 在当前视图各面板之间循环切换焦点 |
| `b` / `g b` / `` <Ctrl-`> `` / `<Ctrl-~>` | 呼出全局分支速查浮窗（`<Esc>` 关闭） |
| `f` / `p` / `P` | 远程同步：**Fetch (`f`)** / **Pull (`p`)** / **Push… (`P`)** |
| `j` / `k` 或 `<Down>` / `<Up>` | 向下 / 向上移动或滚动一行（全视图及控制台通用） |
| `h` / `l` 或 `<Left>` / `<Right>` | 折叠 / 展开树节点；返回 / 进入详情；Log 提交表中跳至子 / 父提交 |
| `<Ctrl-d>` / `<Ctrl-u>` | 向下 / 向上半页滚动（全视图、全屏 Diff、控制台通用） |
| `<PgDn>` / `<PgUp>` | 向下 / 向上整页滚动 |
| `g g` / `G`（或 `<Home>` / `<End>`） | 快速跳转至顶部 / 底部（含全屏 Diff 与控制台） |
| `<Enter>` / `<Esc>` | 进入详情 / 确认 · 返回上一级 / 取消选区 |
| `<Alt-z>` / `<Alt-x>` | 触发底部 Toast 通知按钮动作 / 关闭通知 |

### 双语义前缀族 (`g` 跳转 / `z` 视图)

| 前缀 | 序列（别名） | 作用范围 | 功能描述 |
| :--- | :--- | :--- | :--- |
| **`g` 跳转** | `g g`（`<Home>`） | 全局 | 跳转至列表 / 内容顶部 |
| | `g b`（`b`） | 全局 | 呼出全局分支速查浮窗 |
| | `g r`（`<Ctrl-g>`） | Log | 按哈希 / 分支 / 标签快速跳转 |
| | `g c` | Branch 树 | 定位选中分支顶端在日志中的提交 |
| | `g o` / `g i`（`<Alt-Left>` / `<Alt-Right>` / `<Ctrl-o>`） | Log 提交表 | 日志浏览历史后退 / 前进 |
| **`z` 视图** | `z z`（`<F4>`） | Log / Diff | 切换全屏差异预览（Zoom） |
| | `z d` | Log Diff | 切换差异预览停靠位置（内联 / 右侧 / 底部） |
| | `z b`（`<Ctrl-b>` / `<Alt-b>`） | Log | 折叠 / 展开左侧分支树栏位 |
| | `z t` | 变更树（Log / Commit / Stash） | 切换目录树 / 扁平列表分组 |
| | `z e` / `z c` | 变更树（Log / Commit / Stash） | 展开全部目录 / 折叠全部目录 |
| | `z s`（`S`） | Diff | 切换单列 / 并排差异对比 |
| | `z w`（`w`） | Diff | 循环上下文保留行数（3 / 10 / 全量） |
| | `z g`（`<F8>`） | 全局 | 循环切换提交图渲染后端（Auto / Vector / Cell） |
| | `z i`（`<F12>`） | 全局 | 切换图标模式（Nerd Font / Unicode） |
| | `z p` | 全局 | 打开主题调色板预览 |

### 提交日志视图 (Log View)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `/` / `<Ctrl-f>` / `<Ctrl-l>` | 聚焦提交过滤输入框（输入态支持 `<Alt-r>` 正则、`<Alt-c>` 大小写、`<Ctrl-u>` 清空） |
| `D` / `=` | 对比选中的两个提交 / 开关差异预览 |
| `<Shift-Up>` / `<Shift-Down>` | 扩展多选提交（`<Esc>` 取消多选） |
| `<Shift-Right>` | 跳转至合并提交第二父节点（多父提交时弹窗选择） |
| `c` | 检出 (Checkout) 选中的提交 |
| `n` | 在当前提交创建新分支 |
| `y` | 复制选中提交哈希 |
| `r` / `<F2>` | 修改提交说明 (Reword) |
| `u` | 撤回最近提交并保留变更 (Uncommit) |
| `d` / `<Del>` | 丢弃提交 (Drop，需确认) |
| `m` | 选择 / 切换合并提交父节点（提交表 / 变更树 / Diff / 全屏 Diff 一致） |
| `H` | *(变更文件树)* 查看选中文件提交历史（目录行则按路径过滤日志） |
| `<Ctrl-Enter>` | *(变更树 / Diff)* 直接打开全屏差异预览 |

### 分支树面板 (Log Branches Pane)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `/` / `<Ctrl-f>`（`<Esc>` 退出） | 进入分支树模糊搜索（可在 `keymap.toml` 设为 `tree_speed_search = true` 开启直接打字速查） |
| `<Space>` | 添加 / 取消收藏当前分支（置顶显示） |
| `<Enter>` | 按选中分支过滤提交日志 |
| `g c` | 跳转至选中分支对应的提交记录 |
| `c` / `n` / `r`（`<F2>`）/ `d`（`<Del>`） | 检出 (`c`) / 新建分支 (`n`) / 重命名 (`r`/`F2`) / 删除分支 (`d`/`Del`) |

### 提交工作台 (Commit View)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `<Space>` | **暂存 / 取消暂存** 当前选中的文件或代码块 |
| `s` / `u`（或 `+` / `-`） | 显式暂存 (`s`) / 取消暂存 (`u`) |
| `d` / `<Del>` | 放弃选中文件的本地修改 (Discard Changes) |
| `c` | 聚焦提交说明编辑器（编辑区内按 `<Esc>` 返回变更树） |
| `<Ctrl-s>` / `<Ctrl-Enter>` / `<Alt-Enter>` | 执行提交（变更树与编辑区内均生效） |
| `<Alt-p>` / `<Ctrl-Alt-k>` | 提交并推送 (Commit & Push) |
| `A`（编辑区内 `<Alt-a>` / `<Alt-m>`） | 切换追加至上一提交 (`Amend`) 模式 |
| `<Alt-s>` / `<Ctrl-r>` | 切换 Signed-off-by 签名 / 打开历史提交说明选择器 |
| `<Ctrl-a>` | 全选变更文件 |
| `e` / `H` | 在外部编辑器打开文件 (`e`) / 查看选中文件提交历史 (`H`) |
| `<Ctrl-e>` / `<Ctrl-z>` / `<Ctrl-y>` / `<Ctrl-w>` | *(说明编辑器内)* 外部编辑器写提交说明 / 撤销 / 重做 / 删词 |

### 统一差异对比 (Diff View — Log / Commit / Stash)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `[` / `]` | 跳转至上一个 / 下一个 Hunk 代码块 |
| `{` / `}` | 跳转至上一个 / 下一个变更文件 |
| `S`（或 `z s`） | 切换单列统一对比 (Unified) / 双列并排对比 (Side-by-Side) |
| `w`（或 `z w`） | 循环切换上下文保留行数 (3 行 / 10 行 / 全量) |
| `z d` / `z z`（`<F4>`） | *(Log Diff)* 切换停靠位置 (`z d`) / 全屏差异预览 (`z z`/`F4`/`Esc`) |
| `m` | *(Log Diff / 全屏 Diff)* 切换合并提交父节点 |
| `<Space>` / `s` / `u` | *(Commit Diff)* 切换暂存 (`Space`) · 显式暂存 (`s`) · 取消暂存 (`u`) |
| `v` / `d`（`<Del>`）/ `<Ctrl-z>` | *(Commit Diff)* 行级可视选择 (`v`) · 回滚块/选行 (`d`/`Del`) · 撤销回滚 (`<Ctrl-z>`) |
| `x` / `i` | *(Commit Diff)* 切换未暂存与已暂存侧 (`x`) · 打开三方检视 (`i`) |

### 贮藏箱 (Stash View)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `<Enter>` / `l` / `<Right>` | 安全切换焦点至右侧差异预览（不会误操作应用贮藏） |
| `a` / `A` | 应用选中贮藏并保留 (`a`) / 应用并弹出 Pop (`A`) |
| `n` / `B` | 新建贮藏 (`n`) / 从贮藏创建分支 (`B`) |
| `d`（`<Del>`）/ `D` | 删除选中贮藏 (`d`/`Del`) / 清空所有贮藏 (`D`) |

### 控制台 (Console View)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `j` / `k` / `<Ctrl-d>` / `<Ctrl-u>` / `g g` / `G` | 在命令列表与输出详情中上下移动、半页滚动、跳顶/跳底 |
| `<Enter>` / `l` / `<Esc>` / `h` | 进入输出详情 / 返回命令列表 |
| `s` | 执行 `git status --short --branch` 状态探测（不覆盖全局 `p` 拉取） |

### 模态弹窗与专项视图 (Modals)

| 模态窗 | 常用快捷键 |
| :--- | :--- |
| **全局分支浮窗 (`BranchesQuickView`)** | 直接打字模糊搜索 · `<Up>`/`<Down>`/`<Ctrl-n>`/`<Ctrl-p>` 选择 · `<Ctrl-u>` 清空 · `<Enter>`/`<Right>` 打开分支菜单 · `<Alt-n>` 新建分支 · `<Alt-f>` Fetch · `<Esc>` 关闭 |
| **文件历史 (`FileHistory`)** | `j`/`k`/`<Ctrl-d>`/`<Ctrl-u>`/`g g`/`G` 导航 · `<Shift-Up>`/`<Shift-Down>` 双选对比 · `<Tab>` 切换列表与 Diff 焦点 · `<Enter>` 定位到主日志 · `f` 追踪重命名 (`--follow`) · `l` 对比本地 · `S` 并排 · `w` 上下文 · `z`/`<F4>` 全屏 · `<Esc>` 关闭 |
| **三方冲突合并 (`ConflictMerge`)** | `j`/`k`/`<Ctrl-d>`/`<Ctrl-u>` 滚动 · `[`/`]`（或 `n`/`p`/`<Tab>`）切换冲突块 · `1`/`2`/`3`/`0`（我方/对方/双方/重置）· `Y`/`T` 全部我方/对方 · `m` 外部合并工具 · `e` 外部编辑器 · `<Ctrl-s>`/`<Enter>` 保存并暂存 · `<Esc>` 关闭 |
| **交互式变基 (`RebaseTodo`)** | `j`/`k` 选择 · `J`/`K`（或 `<Alt-Up>`/`<Alt-Down>`）调整顺序 · `<Space>` 循环动作 · `p`/`r`/`s`/`f`/`d`（Pick/Reword/Squash/Fixup/Drop）· `<Enter>` 开始变基 · `<Esc>` 取消 |

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
```

### 5. `keymap.toml`：快捷键预设与增量覆盖

`keymap.toml` 支持全局按键预设切换与分作用域（`[global]`、`[log]`、`[branch]`、`[commit]`、`[diff]`、`[stash]`、`[console]`）增量定制：

- **预设风格 (`preset`)**：
  - `"default"`：统一现代零冲突预设（默认，原生包含 `j`/`k`/`h`/`l`/`<Ctrl-d>`/`<Ctrl-u>`/`gg`/`G` 全视图导航与 `g`/`z` 前缀族）。
  - `"jetbrains"`：贴近 IntelliJ IDEA 习惯（开启 `tree_speed_search = true`，置顶 `<C-k>` Commit、`<C-S-k>` Push、`<C-t>` Pull、`<C-A-n>` 新建分支、`<C-d>` 差异对比、`<A-m>` Amend、`<F2>` 重命名/改说明、`<F4>` 全屏 Diff）。
  - `"vim"`：`default` 兼容别名，并在 Diff 底栏优先展示 `s`/`u` 暂存与取消暂存胶囊。
- **分支树直接打字搜索 (`tree_speed_search`)**：
  - `false`（默认）：在分支树面板按 `/` 或 `<Ctrl-f>` 进入搜索，单键保留导航与快捷操作。
  - `true`：在分支树面板直接打字即可实时过滤分支。
- **底栏与页内键帽定制 (`show_hint`、`footer_max_lines`、`footer_compact_pairs`、`show_inline_key_badges`)**：
  - `show_hint = true | false`：控制该绑定是否展示在底部快捷键提示栏（省略时通过 `prepend_keymap` / `key_bindings` 改键自动继承原设置；使用 `run = "noop"` 解绑主按键时由存活别名继承）。
  - **展示顺序与分层**：当前焦点面板的核心操作与 `g` / `z` 前缀入口优先，其次为全局高频操作；`?` 帮助固定锚定最右侧；所有胶囊及 `+N` 均支持鼠标直接点击。
  - **`footer_max_lines`**：`1`（默认单行并折叠溢出为 `+N`）、`2`~`N`（自动折行多行展示）、`0`（隐藏常驻底栏，临时通知仍按需显示）。
  - **`footer_compact_pairs`**（默认 `true`）：自动合并成对底栏胶囊（如 `[/]` 代码块、`{/}` 文件、`s/u` 暂/取、`a/A` 应用/弹出、`d/D` 删除/清空、`p/P` 拉/推）。
  - **`show_inline_key_badges`**（默认 `true`）：在页面按钮与工具栏自适应内嵌动态快捷键键帽（如 `提交 [Ctrl+S]`、`▶ 应用 [a]`），空间不足时自动省略以避免截断。
- **增量覆盖与解绑**：
  - `prepend_keymap`（推荐）：只覆盖同按键或追加优先绑定，保留同一动作的其他默认别名。
  - `append_keymap`：追加在默认列表末尾。
  - `key_bindings`：替换同按键或同 `run` 动作的现有绑定。
  - `run = "noop"`：解绑指定按键。
  - `on` 支持单键字符串（如 `on = "R"`）或按键数组（如 `on = ["z", "z"]`）；修饰键支持 `<C-...>` / `<A-...>` / `<S-...>`（可组合如 `<C-S-k>`、`<C-A-k>`）。

示例：

```toml
preset = "default"
tree_speed_search = false
footer_max_lines = 1
footer_compact_pairs = true
show_inline_key_badges = true

[global]
prepend_keymap = [
    { on = "R", run = "noop" },                                  # 解绑默认 R（保留 F5）
]

[log]
prepend_keymap = [
    { on = "y", run = "copy_hash", show_hint = false },          # 隐藏底栏 y 复制哈希提示
    { on = "u", run = "uncommit", show_hint = true },            # 在底栏前置展示 u 撤回提交
]

[console]
prepend_keymap = [
    { on = ["g", "s"], run = "git_status_probe", desc = "状态探测" },
]
```

### 6. 常用 `run` 动作标识符参考

| 作用域 | 可用 `run` 动作标识符 |
| :--- | :--- |
| `global` | `quit`, `help`, `context_menu`, `refresh`, `tab_commit`, `tab_log`, `tab_branches`, `tab_stash`, `tab_console`, `focus_next`, `focus_prev`, `toggle_branches_popup`, `git_fetch`, `git_pull`, `git_push`, `cursor_down`, `cursor_up`, `tree_collapse`, `tree_expand`, `page_down_half`, `page_up_half`, `page_down`, `page_up`, `goto_top`, `goto_bottom`, `palette`, `toggle_glyph_mode`, `cycle_graph_renderer`, `noop` |
| `log` | `focus_filter`, `compare_or_diff`, `compare_versions`, `checkout`, `new_branch`, `copy_hash`, `file_history`, `reword`, `uncommit`, `drop_commit`, `choose_parent`, `goto_revision`, `nav_back`, `nav_forward`, `zoom_diff`, `toggle_branches_pane`, `toggle_group`, `expand_all`, `collapse_all` |
| `branch` | `branch_search`, `checkout`, `new_branch`, `rename_branch`, `delete_branch`, `toggle_favorite`, `navigate_branch` |
| `commit` | `toggle_stage`, `stage_selection`, `unstage_selection`, `discard_changes`, `focus_commit_msg`, `commit`, `commit_and_push`, `toggle_amend`, `toggle_signoff`, `commit_history`, `edit_file`, `file_history`, `select_all`, `toggle_group`, `expand_all`, `collapse_all` |
| `diff` | `toggle_stage`, `stage_selection`, `unstage_selection`, `toggle_visual`, `rollback_selection`, `undo_rollback`, `prev_hunk`, `next_hunk`, `prev_file`, `next_file`, `toggle_side_by_side`, `cycle_diff_context`, `zoom_diff`, `cycle_diff_dock`, `choose_parent`, `switch_diff_side`, `three_way_inspector` |
| `stash` | `stash_apply`, `stash_pop`, `stash_new`, `stash_drop`, `stash_clear`, `stash_branch`, `toggle_group`, `expand_all`, `collapse_all` |
| `console` | `git_status_probe`（并自动继承 `[global]` 的全部导航动作） |
