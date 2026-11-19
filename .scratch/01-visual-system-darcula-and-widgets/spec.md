# Spec 01: 视觉美学规范 — Darcula 14色轨道、双重分屏与 Pill 徽标系统

- **目录**：`.scratch/01-visual-system-darcula-and-widgets/`
- **下属 Issues**：
  - [`01-darcula-theme-14-color-palette-and-icons.md`](./issues/01-darcula-theme-14-color-palette-and-icons.md)
  - [`02-ref-pill-badges-and-grouping.md`](./issues/02-ref-pill-badges-and-grouping.md)
- **上游参考源码**：
  - `reference/platform/vcs-impl/shared/src/com/intellij/platform/vcs/impl/shared/ui/RepositoryColorsApi.kt`（JetBrains New UI 14 色定义）
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/VcsLogColorManagerFactory.kt`（14 色深色态容器背景）
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/graph/DefaultColorGenerator.kt`（HSB 动态轨道着色公式）
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/VcsLogStandardColors.java`（HEAD / Local / Remote / Tag 标准色）
  - `reference/plugins/git4idea/backend/src/log/GitRefManager.kt`（引用排序优先级、跟踪分支 `&` 合并规则）
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/render/LabelPainter.java`（Pill 徽标几何排版、截断与紧凑折叠）

---

## 1. 终端色彩与字体基线

### 1.1 色彩能力分级
- **主推目标（TrueColor 24-bit RGB）**：使用 `ratatui::style::Color::Rgb(r, g, b)` 精确还原 JetBrains Darcula / Islands Dark 每一处语义色。通过环境变量 `COLORTERM=truecolor|24bit` 或现代终端特征自动识别。
- **降级兼容（ANSI 256 色）**：提供预计算的 CIE76 最近邻 256 色映射表，确保 SSH 远程老旧终端下依然层次分明。

### 1.2 字符渲染双模式（`GlyphMode`）
支持在配置文件与运行期快捷键中一键切换两种图标/几何渲染基线：
1. **`GlyphMode::Unicode`（默认，零字体门槛）**：仅使用标准 Unicode 盒线字符（Box Drawing `U+2500..U+257F`）、几何图形（Geometric Shapes `U+25A0..U+25FF`）与半块元素（Block Elements `U+2580..U+259F`），兼容所有等宽字体。
2. **`GlyphMode::NerdFont`（推荐增强模式）**：面向安装有 `JetBrainsMono Nerd Font` 等字体的现代终端（Windows Terminal、WezTerm、Kitty、Alacritty），启用圆角胶囊左右半圆端帽（`\u{e0b6}` `` / `\u{e0b4}` ``）及 Git 分支/标签/文件类型专属矢量字形。

---

## 2. Darcula / Islands Dark 全局语义色板

所有 UI 组件严禁硬编码颜色值，统一通过 `ThemePalette` 结构体读取：

| 语义令牌 (Token) | Hex 值 (Darcula Dark) | RGB | 应用场景 |
| :--- | :--- | :--- | :--- |
| `bg_canvas` | `#1E1F22` | `(30, 31, 34)` | 主工作区、提交表格、代码 Diff 底层背景 |
| `bg_panel` | `#2B2D30` | `(43, 45, 48)` | 工具栏、左侧分支树头部、状态栏、浮窗主体面板背景 |
| `bg_elevated` | `#313438` | `(49, 52, 56)` | 右键上下文菜单背景、模态对话框卡片、Pill 默认底色 |
| `bg_selection_focused` | `#2E436E` | `(46, 67, 110)` | 当前获得焦点面板的选中行背景（经典 IDEA 深蓝选区） |
| `bg_selection_unfocused`| `#393B40` | `(57, 59, 64)` | 非焦点面板中保持联动选中的行背景（保证三屏联动视觉锚点清晰） |
| `bg_current_branch_row`| `#252930` | `(37, 41, 48)` | `CurrentBranchHighlighter`：当前 HEAD 可达提交行的微弱底色衬托 |
| `bg_hover` | `#2D2F34` | `(45, 47, 52)` | 鼠标悬停行或菜单项高亮 |
| `border_splitter` | `#393B40` | `(57, 59, 64)` | `OnePixelSplitter` 单列/单行分屏细线（`│` / `─`） |
| `border_focused` | `#3574F0` | `(53, 116, 240)` | 焦点输入框边框、主按钮（Primary Button `[Commit]`）高亮色 |
| `fg_primary` | `#BCBEC4` | `(188, 190, 196)` | 正文、提交标题、未归档普通文件名 |
| `fg_bright` | `#DFE1E5` | `(223, 225, 229)` | 选中行高亮文字、浮窗标题、HEAD 提交加粗标题 |
| `fg_muted` | `#6F737A` | `(111, 115, 122)` | Merge 提交弱化文字（`MergeCommitsHighlighter`）、父路径目录灰字、哈希值、相对时间 |
| `fg_accent_link` | `#548AF7` | `(84, 138, 247)` | 可点击父提交哈希、Diff Hunk 内联操作链接（`Stage` / `Unstage`） |
| `status_modified` | `#6897BB` | `(104, 151, 187)` | Darcula 修改文件 `[M]` 文件名颜色 |
| `status_added` | `#629755` | `(98, 151, 85)` | Darcula 新增/已暂存新增文件 `[A]` 颜色 |
| `status_untracked` | `#D1675A` | `(209, 103, 90)` | Darcula 未跟踪文件 `[?]` 红褐色 |
| `status_deleted` | `#808080` | `(128, 128, 128)` | Darcula 删除文件 `[D]` 颜色 |
| `status_renamed` | `#70AEFF` | `(112, 174, 255)` | 重命名/移动文件 `[R]` 颜色 |
| `status_conflicted` | `#E55765` | `(229, 87, 101)` | 合并冲突文件 `[!]` 告警红 |
| `diff_add_bg` | `#294436` | `(41, 68, 54)` | Diff 新增行整行背景 |
| `diff_add_word_bg` | `#37664B` | `(55, 102, 75)` | Diff 行内词级（ByWord）新增片段高亮背景 |
| `diff_del_bg` | `#482A2D` | `(72, 42, 45)` | Diff 删除行整行背景 |
| `diff_del_word_bg` | `#6E383E` | `(110, 56, 62)` | Diff 行内词级（ByWord）删除片段高亮背景 |
| `diff_mod_bg` | `#25364D` | `(37, 54, 77)` | Diff 修改对照块背景 |

---

## 3. Darcula 14 色分支轨道调色板与分配算法

综合 `RepositoryColorsApi.kt`（第 31-47 行 `NEW_UI_REPOSITORY_COLORS`）、`VcsLogColorManagerFactory.kt`（第 28-44 行）以及 `DefaultColorGenerator.kt`（第 39-58 行），我们定义 **jbgit Darcula 14 色高对比度分支轨道调色板**。

针对终端深色背景 `#1E1F22`，所有 14 色均对齐 JetBrains New UI 色相环并校准亮度（WCAG 对比度 ≥ 4.5:1），同时配备对应的暗调胶囊容器色（用于多根仓库徽标或轨道选中衬底）：

| 索引 `#` | 色相语义 | 轨道主色 (Track Fg) | 容器衬底色 (Muted Bg) | 典型视觉印象 |
| :---: | :--- | :--- | :--- | :--- |
| `0` | Rose / Coral | `#FF6285` | `#603239` | 珊瑚粉红（常用于主干或核心高频分支） |
| `1` | Sage / Emerald | `#57D185` | `#375239` | 翡翠青绿 |
| `2` | Indigo / Iris | `#7B6FF6` | `#383275` | 鸢尾蓝紫 |
| `3` | Apricot / Amber | `#F2B181` | `#614438` | 暖杏橙色 |
| `4` | Cyan / Teal | `#32E0D9` | `#1D4D4A` | 亮青碧色 |
| `5` | Gold / Sand | `#FED277` | `#5E4D33` | 流金明黄 |
| `6` | Magenta / Orchid | `#F46BFF` | `#562B58` | 兰紫品红 |
| `7` | Bronze / Ochre | `#CE9E7A` | `#624E3F` | 古铜暖棕 |
| `8` | Mint / Spring | `#4FF2B7` | `#0D573C` | 薄荷亮绿 |
| `9` | Violet / Lavender | `#A177F4` | `#3B3147` | 薰衣草紫 |
| `10` | Ocean / Cerulean | `#46C2C7` | `#204145` | 蔚蓝海青 |
| `11` | Plum / Mauve | `#C985C9` | `#503250` | 梅子紫红 |
| `12` | Wheat / Honey | `#ECCA88` | `#594D36` | 麦穗浅金 |
| `13` | Slate / Silver | `#B8BDC9` | `#494B57` | 银灰岩蓝 |

### 3.1 确定性颜色分配与超 14 轨回退规则

对标 `GraphColorManagerImpl.kt` 与 `DefaultColorGenerator.kt`：

1. **命名分支主轨道稳定性（Name-Stable Hashing）**：
   - 若某条轨道对应的 `headCommit` 处于其主片段（`head_layout_index == current_layout_index`），且该提交上有分支引用（按 `GitLabelComparator` 取首个分支名 `ref_name`，优先去除 `origin/` 前缀使 `feature/login` 与 `origin/feature/login` 共享同一色系）：
   - 使用稳定哈希（Java 兼容 `wrapping_mul(31)` 字符串哈希或 FNV-1a）映射到 `0..14` 调色板索引；其中 `main` / `master` 固定优先绑定索引 `0` 或可配置的固定槽位，保证同一分支在任意时刻刷新不会变色。
2. **匿名分叉片段（Unnamed Diverged Fragments）**：
   - 当某段历史没有直接分支尖端指向（例如已合并的历史子分支，`head_layout_index != fragment_layout_index`），以 `fragment_layout_index` 作为 `color_id`。
   - 若相邻活跃轨道发生同槽位撞色，在行级可视分配时优先向后顺移一个当前可视窗口未占用的 14 色槽位；
   - 当同屏极端并发分支超过 14 条时，启用 `DefaultColorGenerator.calcColor(color_id)` 的 Rust 等价 HSB 生成器（`saturation = 0.50`, `brightness = 0.78`, `range_fix(n) = (n % 100).abs() + 70`），确保任意数量分支绝不退化为纯白或不可见色。

---

## 4. Pill 引用徽标系统 (`PillBadgeBar`)

精准复刻 `VcsLogStandardColors.Refs`、`GitRefManager.kt` 与 `LabelPainter.java`。

### 4.1 引用类型语义色与排序优先级

#### 徽标颜色令牌（对标 `VcsLogStandardColors.java` 第 10-14 行）
| 引用种类 (`RefKind`) | 左侧指示条/图标色 | Pill 底色 (未选中行) | Pill 文字色 | Unicode 图标 | Nerd Font 图标 |
| :--- | :--- | :--- | :--- | :---: | :---: |
| **`HEAD` (附着分支)** | `#E1C731` (亮金黄) | `#3A3728` | `#F2E49B` | `◆` | `\u{f418}` / `★` |
| **`HEAD` (Detached 游离)** | `#E5A93B` (警告橙) | `#453422` | `#FFCC66` | `⚠` | `\u{f071}` |
| **`LocalBranch`** | `#3CB45C` (翠绿) | `#283B2E` | `#D4E8D9` | `⎇` | `\u{e0a0}` (``) |
| **`TrackedPair` (`origin & x`)** | 双色 `#3CB45C` + `#9F79B5` | `#2F3440` | `#DFE1E5` | `⎇⇄` | `\u{e0a0}`双色点 |
| **`RemoteBranch`** | `#9F79B5` (丁香紫) | `#362D3E` | `#DFCEF0` | `☁` | `\u{f0c2}` / `` |
| **`Tag`** | `#999999` (中灰) | `#313337` | `#B8BAC0` | `⌗` | `\u{f02b}` (``) |

#### 同一提交多徽标排列优先级（对标 `GitRefManager.GitLabelComparator` 第 191-200 行）
当一个提交同时挂载多个引用时，严格按以下顺序从左至右排列：
1. `Detached HEAD`（若处于游离头指针状态，独立渲染带 `⚠` 的警告胶囊）
2. `HEAD + Current Branch`（当处于分支上时，IDEA 将 `HEAD` 金色标识合并入当前分支胶囊左侧：`[◆ main]`，而非拆成两个冗余胶囊！）
3. `main` / `master`（或与其同提交的跟踪组 `origin & main`）
4. `origin/main` / `origin/master`
5. 其他 `TrackedPair`（本地分支与对应跟踪远程分支指向同一提交时，融合为 **`origin & feature/foo`** 单一胶囊！参见 `GitRefManager.createTrackedGroup` 第 312-332 行）
6. 其他 `LocalBranch`（按自然字典序）
7. 其他 `RemoteBranch`
8. `Tag`（受工具栏 `Show Tag Names` 开关控制：关闭时仅渲染紧凑标签色块图标，开启时渲染完整 `[⌗ v1.2.0]`）

### 4.2 双模式胶囊字符造型规范

1. **标准 Unicode 模式 (`GlyphMode::Unicode`)**：
   - 使用前景色=胶囊底色的左半块 `▐` (`U+2590`) 与右半块 `▌` (`U+258C`) 包裹胶囊主体，实现比普通方括号 `[main]` 更接近 GUI 实心圆角标签的视觉质感：
   ```text
   ▐◆ origin & main▌ ▐⎇ feat/dag▌ ▐⌗ v0.1.0▌ 提交说明文字...
   ```
   - 其中 `▐` 以 `fg = pill_bg, bg = row_bg` 绘制，内部文字以 `fg = pill_fg, bg = pill_bg` 绘制（图标本身使用对应的语义高亮色 `#E1C731` / `#3CB45C` / `#9F79B5`），右侧端帽 `▌` 以 `fg = pill_bg, bg = row_bg` 收尾。
2. **Nerd Font 增强模式 (`GlyphMode::NerdFont`)**：
   - 将左右端帽替换为 Powerline 圆角半圆 `\u{e0b6}` (``) 与 `\u{e0b4}` (``)，在现代终端中呈现与 IntelliJ Swing `RoundRectangle2D(arc=6)` 完全一致的丝滑胶囊圆角！

### 4.3 截断与紧凑折叠规则（对标 `LabelPainter.java` 第 62-64、241-247、319-332 行）

1. **单分支长名称缩写（`MAX_LENGTH = 22`）**：
   - 若引用显示文本长度 > 22 列且剩余列宽不足，且包含 `/` 分隔符（如 `feature/user-authentication-oauth2`），先将首个 `/` 之前的前缀折叠为 `..` -> `../user-authentication-oauth2`，若仍超宽则尾部截断加 `…` -> `../user-authenticat…`。
2. **空间不足溢出聚合（Overflow Badge）**：
   - 提交标题至少保留 **24 列** 可读宽度；当某行挂载过多引用超出可用宽度时，后续放不下的引用折叠为一个尾随紧凑计数胶囊 `▐+3▌`（悬停或右键点击该胶囊可展开查看该提交上的全部引用列表）。
3. **Compact References View 开关**：
   - 开启后仅显示第一个核心引用的文本，其余同提交引用浓缩为彩色竖条组，最大化让渡提交消息阅读空间。

---

## 5. 双重分屏与整体窗口骨架（`OnePixelSplitter` TUI 还原）

高保真复刻 IDEA Git 工具窗口的 **极简无外框细线分屏（1-cell Splitter）**：
- **禁止传统 TUI 的繁琐四周边框套娃**（不在每个子面板外画一圈浪费 2 行 2 列的 `┌──┐` 盒子！），严格遵循 IDEA New UI 设计：面板之间仅用 **1 列宽竖线 `│` (`#393B40`)** 或 **1 行高横线 `─` (`#393B40`)** 分隔，通过面板头部工具栏底色区分（`#2B2D30` vs `#1E1F22`）建立视觉层级！
- 当前获得键盘焦点的子面板，在其标题栏左侧点亮 **2 列宽蓝色焦点指示条 `▎` (`#3574F0`)** 或高亮面板标题文字。
- 所有 `Splitter` 均支持：
  1. **鼠标左键按住拖拽** 实时改变分屏比例（最小面板保护阈值：左栏 ≥ 18 列，中栏 ≥ 36 列，右栏 ≥ 28 列）；
  2. **双击分割线** 恢复 IDEA 默认黄金比例（如左分支树 `0.22`、中日志表 `0.48`、右详情变更区 `0.30`；上下分屏 `0.65 : 0.35`）；
  3. **键盘快捷键** `Ctrl+Shift+Left/Right/Up/Down` 微调当前面板尺寸，`Alt+1` / `Alt+2` / `Alt+3` 快速聚焦或折叠面板。

### 全局界面骨架线框图（Log 三屏 + 右侧上下双重分屏态）

```text
 jbgit  ⎇ main ↑2 ↓1 │ [1 Commit (3)]  [2 Log ●]  [3 Stash]  [4 Console]            [Fetch] [Pull] [Push]
────────────────────┼──────────────────────────────────────────────────────┬─────────────────────────────
▎Branches      [🔍] │ 🔍 Text or Hash…  │ Branch: All ▾ │ User ▾ │ [⚙ ▾]  │ Changes (4 files)   [⊞][👁]
 ▼ HEAD (main)      │ ●  ▐◆ main▌ ▐☁ origin/main▌ feat: add non-cross…  │ ▼ src/graph
 ▼ Local            │ │ ╭─●  ▐⎇ feat/stage▌ wip: hunk line tracker       │   M layout.rs        +48 -12
   ▼ feature        │ ├─╯ │                                                │   A print_elem.rs   +210  -0
     ★ dag-layout ↑1│ ●   │  fix: handle octopus merge edge color          │ ▼ tests
       stage-ui     │ ├─╮ │                                                │   M graph_test.rs    +65  -4
 ▼ Remote           │ │ ● │  docs: update architecture spec                ├─────────────────────────────
   ▼ origin         │ ├─╯ │                                                │ a8f3d21c9e4b · Verified ✓
       main         │ ●───╯  refactor: extract git2 revwalk pool           │ Author:  Alice <a@ex.com>
       feat/stage   │ ●      chore: initial workspace setup                │ Date:    2026-11-18 14:20
 ▶ Tags (12)        │                                                      │ Branches: main, origin/main
                    │                                                      │ 
                    │                                                      │ feat: add non-crossing DAG
                    │                                                      │ Implements GraphLayoutBuilder
────────────────────┴──────────────────────────────────────────────────────┴─────────────────────────────
 [Enter] Diff  [Space] Filter Branch  [m / RightClick] Context Menu  [Ctrl+Shift+`] Branches  [?] Help
```
