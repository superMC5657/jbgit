# jGit

<p align="center">
  <strong>JetBrains 风格的高性能现代终端 Git 客户端</strong>
  <br />
  <em>A modern, high-performance JetBrains-style Terminal Git Client written in Rust</em>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-2024_Edition-orange?logo=rust" alt="Rust 2024" />
  <img src="https://img.shields.io/badge/License-Apache--2.0-blue.svg" alt="License" />
  <img src="https://img.shields.io/badge/Architecture-Async_Tokio_%2B_libgit2-blueviolet" alt="Architecture" />
  <img src="https://img.shields.io/badge/UI-Ratatui_%2B_Crossterm-green" alt="UI" />
</p>

---

## 📖 简介 (Introduction)

**jGit** 是一款深度致敬 IntelliJ IDEA / JetBrains 系列 IDE 版本控制体验的终端 Git 客户端。

在享受终端轻量、跨平台与极速响应的同时，jGit 将 JetBrains 经典的 **VCS Log 三屏拓扑图谱**、**双栏暂存工作台**、**行级局部暂存**、**分支快速速查浮窗 (`<C-~>`)** 以及**冲突三方合并工作流**完整复刻至终端之中。项目采用 Rust 编写，结合 `libgit2` 异步线程池与原生 Git CLI 混合执行引擎，在 50,000+ 提交的超大仓库中依然能够提供丝滑流畅的虚拟化滚动与毫秒级交互。

---

## ✨ 核心特性 (Key Features)

### 1. 🌳 JetBrains 风格 VCS Log (提交历史图谱)
- **拓扑排序与 BEK 算法**：采用永久图 (Permanent Graph) 与 DFS 拓扑排序 + BEK (Branch-Earliest-Known) 排序算法，保持主分支平直延伸，生成优雅的非交叉连接线。
- **Darcula 经典配色与主题系统**：内置 JetBrains 原生 Darcula 14 色调色板、Catppuccin Mocha 以及 Gruvbox Dark 风格。
- **Ref Pill 徽标胶囊**：HEAD、本地分支、远程分支及 Tag 采用圆角胶囊与语义分色高亮，支持自适应折叠与 CJK 宽字符精准对齐。
- **双图谱渲染后端**：
  - **Cell 字符模式**：针对标准终端的 Unicode/ASCII 单宽字符光栅化。
  - **Vector 像素矢量模式**（基于 `tiny-skia`）：支持 Kitty / Sixel / iTerm2 图像协议，呈现抗锯齿的平滑彩色矢量导轨与节点。
- **三屏联动工作台**：
  - 左侧：**分支树 (Branches Dashboard)**（按目录层级折叠、收藏置顶、ahead/behind 计数）。
  - 中间：**提交图谱与筛选条 (VCS Log Table & Filter Bar)**。
  - 右侧：**提交详情与变更文件 Diff 联动 (Commit Changes Details & Diff)**。
- **多维复合筛选**：支持在过滤栏按分支 (`branch:`)、用户 (`user:`)、日期 (`date:`)、路径 (`path:`) 以及消息/哈希/正则快速检索。

### 2. ⚡ Commit 暂存工作台与行级局部暂存
- **双栏工作台**：左侧展示变更文件树（支持按目录层级折叠或扁平列表切换），右侧实时呈现文件差异（Diff）。
- **创新三态文档行追踪器 (Three-Doc Line Tracker)**：
  - 支持文件级别、Hunk 差异块级别、以及**逐行 (Line-level)** 的暂存 (Stage) 与取消暂存 (Unstage)。
  - 自动保留无尾部换行符 (`\No newline at end of file`) 及可执行权限位状态。
- **提交编辑器**：
  - 支持普通提交与追加至上一提交 (`Amend`)。
  - 具备提交前检查（Unborn 初始状态、空提交防护、Hook 失败保留草稿等）。

### 3. 🌿 分支管理与全局速查浮窗 (Branches Popup)
- **类似 IDEA 的全局浮窗**：按 `<C-~>` 或 `<C-`>` 随处唤出分支速查弹窗。
- **Speed Search 模糊速查**：键盘直接打字即可秒级过滤目标分支。
- **丰富的分支操作**：
  - 检出 (Checkout)、新建分支、重命名 (`<F2>`)、删除分支 (`<Del>`)、星标收藏置顶 (`f`)。
  - 远程分支追踪与分支对比 (Compare with Current)。
- **高级 Git 交互与右键菜单**：
  - Rebase、Merge、Cherry-pick、Revert、Reset (Soft / Mixed / Hard)。
  - 交互式变基相关操作：修改提交信息 (Reword)、删除提交 (Drop)、压缩提交 (Squash)。
  - 变基/合并冲突检测横幅与交互式三方解决工具 (3-Way Conflict Resolver)。

### 4. 📦 Stash 贮藏箱与 Git Console 控制台
- **Stash 贮藏管理**：浏览所有贮藏快照，查看差异预览，一键执行 Apply、Pop、Drop 或检出为新分支 (Unstash as Branch)。
- **Git Console 控制台**：完全透明化展示底层执行的原生 Git CLI 命令与完整输出回放。

### 5. ⚙️ Yazi 风格配置体系与多语言国际化 (i18n)
- **多文件配置规范**：
  - `jgit.toml`：通用设置、布局栏位比例、图谱渲染模式。
  - `theme.toml`：主题预设 (Flavors) 与 16 进制颜色微调。
  - `keymap.toml`：自定义快捷键映射与连续按键序列。
- **4 级层级覆盖机制**：
  `JGIT_CONFIG_DIR` 环境变量 > 仓库级 `.jgit/` > 全局用户配置 > 内置默认值。
- **全量多语言支持**：原生内置简体中文 (`zh-CN`) 与英文 (`en-US`)，支持根据系统环境自动切换。

---

## 🏗️ 架构拓扑 (Architecture)

```text
jGit/
├── Cargo.toml                  # Workspace 配置
├── crates/
│   ├── jgit-core/              # 核心算法库：DAG 拓扑、Permanent Graph、BEK 排序、
│   │                           # 三态文档行追踪器、分支树模型、多维过滤器
│   ├── jgit-engine/            # 混合 Git 引擎：libgit2 异步线程池 + 原生 Git CLI 执行器 +
│   │                           # notify 文件变动防抖监听与锁竞争自动重试
│   └── jgit-tui/               # 终端用户界面：Ratatui + Crossterm、主题/字体光栅化、
│                               # 窗口拆分器 (Splitter)、Hit-Test 交互路由与多语言引擎
└── docs/                       # 设计规约与架构文档
```

---

## 🚀 安装与构建 (Installation & Build)

### 编译前提
- **Rust 工具链**：Rust 2024 Edition (推荐 Rust 1.85+)
- **Git**：系统中已安装 `git` 命令行工具

### 1. 源码编译与安装
```bash
# 克隆仓库
git clone https://github.com/superMC5657/jGit.git
cd jGit

# 编译并在本地安装二进制
cargo install --path crates/jgit-tui

# 验证安装
jgit --help
```

### 2. 启用 Kitty / Sixel 矢量图后端 (可选)
如果希望在 Kitty、WezTerm 等现代终端中体验像素级平滑抗锯齿矢量图谱，可启用 `graphics-protocol` 特性（默认已启用）：
```bash
cargo build --release -p jgit-tui --features graphics-protocol
```

---

## 💻 快速开始与使用 (Quick Start)

### 命令行运行
```bash
# 在当前 Git 仓库中启动
jgit

# 打开指定目录的 Git 仓库
jgit /path/to/repo

# 在用户配置目录下生成全套带有中文注释的配置模板文件
jgit --init-config

# 查看内置的默认配置内容
jgit --print-default-config jgit      # 查看 jgit.toml 默认项
jgit --print-default-config theme     # 查看 theme.toml 默认项
jgit --print-default-config keymap    # 查看 keymap.toml 默认项
```

---

## ⌨️ 快捷键速查表 (Keybindings)

### 全局操作 (Global)
| 快捷键 | 功能描述 |
| :--- | :--- |
| `q` | 退出 jGit |
| `?` | 打开交互式快捷键帮助视窗 |
| `r` | 强制刷新 Git 仓库状态 |
| `<Alt-1>` | 切换至 **提交日志 (Log)** 视图 |
| `<Alt-2>` | 切换至 **提交工作台 (Commit)** 视图 |
| `<Alt-3>` | 切换至 **分支一览 (Branches)** 视图 |
| `<Alt-4>` | 切换至 **贮藏箱 (Stash)** 视图 |
| `<Ctrl-~>` / `<Ctrl-`>` | 呼出 / 关闭全局分支速查浮窗 (Branches Popup) |
| `<F9>` | 打开全局调色板与配色预览 |

### 提交日志视图 (Log View)
| 快捷键 | 功能描述 |
| :--- | :--- |
| `j` / `k` 或 `<Down>` / `<Up>` | 上下移动光标浏览提交记录 |
| `g g` / `G` | 快速跳转至提交历史顶部 / 底部 |
| `/` | 聚焦分支与提交过滤输入框 |
| `c` | 检出 (Checkout) 选中的提交或分支 |
| `<Ctrl-d>` | 版本差异对比 (Diff with Previous / Selection) |
| `<Ctrl-g>` | 按提交哈希、分支名或 Tag 快速定位跳转 |
| `u` | 撤销选中提交 (Uncommit / Reset Mixed) |
| `<F2>` | 修改提交说明 (Reword Commit) |
| `<Del>` | 删除提交 (Interactive Rebase Drop) |
| `鼠标右键` | 弹出上下文菜单 (Cherry-pick, Revert, Reset, Squash 等) |

### 提交工作台 (Commit View)
| 快捷键 | 功能描述 |
| :--- | :--- |
| `<Space>` | **暂存 / 取消暂存** 当前选中的文件、差异块 (Hunk) 或指定代码行 |
| `<Ctrl-Enter>` | 执行提交 |
| `<Alt-a>` | 切换追加至上一提交 (`Amend`) 模式 |
| `<Ctrl-a>` | 全选变更文件 |
| `<Del>` | 放弃选中文件或代码块的本地修改 (Discard Changes) |
| `v` | 切换按目录层级树展示 / 扁平列表展示 |
| `e` | 在外部系统编辑器中打开当前文件 |

### 分支管理 (Branches)
| 快捷键 | 功能描述 |
| :--- | :--- |
| `直接打字` | Speed Search 模糊速查过滤分支 |
| `f` | 添加 / 取消收藏当前分支 (置顶显示) |
| `c` | 检出选中的分支 |
| `<F2>` | 重命名选中的分支 |
| `<Del>` | 安全删除选中的分支 |

### 差异对比 (Diff View)
| 快捷键 | 功能描述 |
| :--- | :--- |
| `s` | 切换单列统一对比 (Unified) / 双列并排对比 (Side-by-Side) |
| `c` | 循环切换上下文行数 (3 行 / 10 行 / 全量代码) |
| `[` / `]` | 快速跳转至上一个 / 下一个变更文件 |
| `<F4>` | 切换全屏放大预览 Diff |

---

## 🛠️ 配置体系指南 (Configuration)

运行 `jgit --init-config` 即可在系统的用户配置目录下生成带详尽注释的配置模板：
- **Windows**: `%APPDATA%\jgit\config\`
- **Linux / macOS**: `~/.config/jgit/`

配置由三个互相独立的文件构成：

### 1. `jgit.toml` (通用行为与布局比例)
```toml
[general]
# 界面语言: "zh-CN" (简体中文), "en-US" (English), "auto" (自动跟随环境变量)
language = "zh-CN"

# 启动时默认打开的标签页: "log", "commit", "branches", "stash"
default_tab = "log"

# 主题预设: "darcula", "catppuccin-mocha", "gruvbox-dark"
flavor = "darcula"

[graph]
# 提交图渲染引擎: "auto", "vector" (Kitty/Sixel), "cell" (字符网格)
backend = "auto"
# 图标模式: "nerd_font" 或 "unicode"
glyph_mode = "nerd_font"

[diff]
syntax_highlight = true
tab_size = 4
context_lines = 3

[layout]
# Log 日志主视图栏位比例: [左侧分支树, 中间提交图谱, 右侧详情/Diff] (默认 [1, 3, 2])
log_ratio = [ 1, 3, 2 ]
# Commit 提交工作台比例: [左侧文件变更树, 右侧 Diff 预览] (默认 [1, 2])
commit_ratio = [ 1, 2 ]
# Stash 贮藏箱比例: [左侧贮藏条目, 右侧差异预览] (默认 [2, 3])
stash_ratio = [ 2, 3 ]
# Console 控制台比例: [左侧命令历史, 右侧执行输出] (默认 [2, 3])
console_ratio = [ 2, 3 ]
```

### 2. `theme.toml` (配色微调)
```toml
flavor = "darcula"

[glyphs]
mode = "nerd_font"

[palette]
# 可取消注释单独微调特定语义颜色 (十六进制 RGB: #RRGGBB)
# bg_canvas = "#1E1F22"
# bg_selection_focused = "#2E436E"
# fg_accent_link = "#548AF7"
# status_modified = "#6897BB"
# status_added = "#629755"
```

### 3. `keymap.toml` (按键绑定)
对标 Yazi 的按键映射体系，支持单键、组合键（`<C-x>`, `<A-x>`）及连续按键（如 `["g", "g"]`）：
```toml
[global]
key_bindings = [
    { on = [ "q" ], run = "quit", desc = "退出 jGit" },
    { on = [ "<F5>" ], run = "refresh", desc = "刷新仓库状态" },
]
```

---

## 📄 开源许可证 (License)

本项目遵循 [Apache License 2.0](LICENSE) 开源协议。
