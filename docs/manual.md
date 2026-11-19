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
- [三、配置体系指南 (Configuration)](#三配置体系指南-configuration)
  - [1. 配置文件生成与查看](#1-配置文件生成与查看)
  - [2. 配置文件路径与 4 级覆盖优先级](#2-配置文件路径与-4-级覆盖优先级)
  - [3. `jbgit.toml`：通用行为、图谱与布局比例](#3-jbgittoml通用行为图谱与布局比例)
  - [4. `theme.toml`：主题预设与语义配色](#4-themetoml主题预设与语义配色)
  - [5. `keymap.toml`：快捷键预设与增量覆盖](#5-keymaptoml快捷键预设与增量覆盖)
  - [6. 常用 `run` 动作标识符参考](#6-常用-run-动作标识符参考)

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
| `.` / `<Shift-F10>` / `鼠标右键` | 打开当前选中项的操作菜单 |
| `r` / `<F5>` | 强制刷新 Git 仓库状态 |
| `1` / `2` / `3` / `4`（或 `<Alt-1>` – `<Alt-4>`） | 按标签栏顺序切换视图：**Commit (`1`)** / **Log (`2`)** / **Stash (`3`)** / **Console (`4`)** |
| `<Tab>` / `<Shift-Tab>` | 在当前视图各面板之间循环切换焦点 |
| `b` / `g b` / `` <Ctrl-`> `` / `<Ctrl-~>` | 呼出全局分支速查浮窗（`<Esc>` 关闭） |
| `f` / `p` / `P` | 远程同步：**Fetch (`f`)** / **Pull (`p`)** / **Push… (`P`)** |

### 提交日志视图 (Log View)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `j` / `k` 或 `<Down>` / `<Up>` | 上下移动光标浏览提交记录 |
| `g g` / `G`（或 `<Home>` / `<End>`） | 快速跳转至提交历史顶部 / 底部 |
| `/` / `<Ctrl-f>` / `<Ctrl-l>` | 聚焦分支与提交过滤输入框（搜索框内支持 `<Alt-r>` 正则、`<Alt-c>` 大小写敏感） |
| `<Ctrl-b>` / `<Alt-b>` | 折叠 / 展开左侧分支树栏位 |
| `c` | 检出 (Checkout) 选中的提交或分支 |
| `n` | 在当前提交创建新分支 |
| `y` | 复制选中提交哈希 |
| `<Ctrl-d>` | 版本差异对比（选中两个提交时对比两者；单选时开关差异预览） |
| `g r` / `<Ctrl-g>` | 按提交哈希、分支名或 Tag 快速定位跳转 |
| `m` | 为合并提交选择 / 切换父提交 |
| `u` / `<F2>` / `<Del>` | 撤回最近提交 (Uncommit) / 修改说明 (Reword) / 丢弃提交 (Drop) |

### 分支树面板 (Log Branches Pane)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `/` / `<Ctrl-f>`（`<Esc>` 退出） | 进入分支树模糊搜索（可在 `keymap.toml` 设为 `tree_speed_search = true` 开启直接打字速查） |
| `<Space>` | 添加 / 取消收藏当前分支（置顶显示） |
| `<Enter>` | 按选中分支过滤提交日志 |
| `J` | 跳转至选中分支对应的提交记录 |
| `c` / `n` / `<F2>` / `d` / `<Del>` | 检出 (`c`) / 新建分支 (`n`) / 重命名 (`F2`) / 删除分支 (`d`/`Del`) |

### 全局分支浮窗 (Branches Popup)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `直接打字` / `<Ctrl-u>` | Speed Search 模糊速查过滤分支 / 清空输入 |
| `<Enter>` / `<Right>` | 打开选中分支的操作子菜单（检出、新建、重命名、删除、对比等） |
| `<Alt-f>` / `<Esc>` | 执行远程 Fetch / 关闭浮窗 |

### 提交工作台 (Commit View)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `<Space>` | **暂存 / 取消暂存** 当前选中的文件或代码块 |
| `c` | 聚焦提交说明编辑器（编辑区内按 `<Esc>` 返回变更树） |
| `<Ctrl-s>` / `<Ctrl-Enter>` / `<Alt-Enter>` | 执行提交（编辑区内 `<Ctrl-Alt-k>` 提交并推送） |
| `A`（或编辑区内 `<Alt-a>` / `<Alt-m>`） | 切换追加至上一提交 (`Amend`) 模式 |
| `<Alt-s>` / `<Ctrl-m>` | 切换 Signed-off-by 签名 / 打开历史提交说明选择器 |
| `<Ctrl-a>` | 全选变更文件 |
| `d` / `<Del>` | 放弃选中文件或代码块的本地修改 (Discard Changes) |
| `v` / `e` | 切换按目录层级树展示 (`v`) / 在外部编辑器打开 (`e`) |

### 统一差异对比 (Diff View — Log / Commit / Stash)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `[` / `]` | 跳转至上一个 / 下一个 Hunk 代码块 |
| `{` / `}` | 跳转至上一个 / 下一个变更文件 |
| `S` | 切换单列统一对比 (Unified) / 双列并排对比 (Side-by-Side) |
| `w` | 循环切换上下文保留行数 (3 行 / 10 行 / 全量) |
| `m` / `z`（或 `<F4>`） | 切换 Log Diff 停靠位置 (`m`) / 全屏差异预览 (`z`/`F4`/`Esc`) |
| `<Space>` / `v` / `d` | *(Commit Diff)* 暂存/取消 (`Space`) · 行级可视选择 (`v`) · 回滚选区 (`d`/`Del`，`<Ctrl-z>` 撤销) |
| `x` / `i` | *(Commit Diff)* 切换未暂存与已暂存侧 (`x`) · 打开三方检视 (`i`) |

### 贮藏箱 (Stash View)

| 快捷键 | 功能描述 |
| :--- | :--- |
| `<Enter>` | 安全切换焦点至右侧差异预览（不会误操作应用贮藏） |
| `a` / `o` | 应用选中贮藏 (`a`) / 应用并弹出 (`o`) |
| `n` / `B` / `d` / `C` | 新建贮藏 (`n`) / 从贮藏创建分支 (`B`) / 删除 (`d`/`Del`) / 清空 (`C`) |

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
# Log 视图三栏权重: [左侧分支树, 中间提交图谱, 右侧提交详情/Diff]
log_ratio = [ 1, 3, 2 ]

# Commit 工作台两栏权重: [左侧变更文件树, 右侧 Diff 预览]
commit_ratio = [ 1, 2 ]

# Stash 贮藏箱两栏权重: [左侧贮藏列表, 右侧 Diff 预览]
stash_ratio = [ 2, 3 ]

# Console 控制台两栏权重: [左侧命令历史, 右侧执行输出]
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

`keymap.toml` 支持全局按键预设切换与分作用域（`[global]`、`[log]`、`[branch]`、`[commit]`、`[diff]`、`[stash]`）增量定制：

- **预设风格 (`preset`)**：
  - `"default"`：极简零冲突推荐预设（默认）。
  - `"jetbrains"`：贴近 IntelliJ IDEA 习惯（自动开启 `tree_speed_search = true`，并补充 `<C-k>` 打开 Commit、`<C-S-k>` Push、`<C-t>` Pull、`<C-A-n>` 新建分支、`<A-m>` Amend 等映射）。
  - `"vim"`：Vim / Lazygit 风格（Log 中 `<C-d>`/`<C-u>` 半页翻滚、`d` 对比提交/开关差异，Diff 中 `s`/`u` 暂存/取消暂存，`c` 切换上下文行数）。
- **分支树直接打字搜索 (`tree_speed_search`)**：
  - `false`（默认）：在分支树面板按 `/` 或 `<Ctrl-f>` 进入搜索，单键 `j`/`k`/`c`/`q`/`r` 等保留导航与快捷操作。
  - `true`：在分支树面板直接打字即可实时过滤分支。
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

[global]
prepend_keymap = [
    { on = "r", run = "noop" },                                  # 解绑默认 r
    { on = ["g", "s"], run = "git_status_probe", desc = "状态探测" },
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
| `global` | `quit`, `help`, `context_menu`, `refresh`, `tab_commit`, `tab_log`, `tab_branches`, `tab_stash`, `tab_console`, `focus_next`, `focus_prev`, `toggle_branches_popup`, `toggle_branches_pane`, `git_fetch`, `git_pull`, `git_push`, `goto_revision`, `palette`, `toggle_glyph_mode`, `cycle_graph_renderer`, `git_status_probe`, `noop` |
| `log` | `cursor_down`, `cursor_up`, `goto_top`, `goto_bottom`, `page_down_half`, `page_up_half`, `goto_revision`, `focus_filter`, `toggle_branches_pane`, `compare_or_diff`, `zoom_diff`, `copy_hash`, `checkout`, `new_branch`, `uncommit`, `reword`, `drop_commit`, `choose_parent` |
| `branch` | `branch_search`, `toggle_favorite`, `checkout`, `new_branch`, `rename_branch`, `delete_branch`, `navigate_branch` |
| `commit` | `cursor_down`, `cursor_up`, `goto_top`, `goto_bottom`, `toggle_stage`, `stage_selection`, `unstage_selection`, `focus_commit_msg`, `commit`, `commit_and_push`, `toggle_amend`, `toggle_signoff`, `commit_history`, `select_all`, `discard_changes`, `toggle_group`, `edit_file` |
| `diff` | `cursor_down`, `cursor_up`, `prev_hunk`, `next_hunk`, `prev_file`, `next_file`, `toggle_side_by_side`, `cycle_diff_context`, `cycle_diff_dock`, `zoom_diff`, `toggle_stage`, `stage_selection`, `unstage_selection`, `toggle_visual`, `rollback_selection`, `undo_rollback`, `switch_diff_side`, `three_way_inspector` |
| `stash` | `cursor_down`, `cursor_up`, `goto_top`, `goto_bottom`, `stash_new`, `stash_apply`, `stash_pop`, `stash_drop`, `stash_branch`, `stash_clear` |
