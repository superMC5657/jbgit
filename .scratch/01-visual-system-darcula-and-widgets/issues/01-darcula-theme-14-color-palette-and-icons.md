# 01: 实现 Darcula 主题令牌、14 色分支轨道调色板与双模图标系统

Status: ready-for-agent
Blocked by: none
Spec: [../spec.md](../spec.md)
Milestone: M1 — 底座、主题与混合引擎

- **参考源码**：
  - `reference/platform/vcs-impl/shared/src/com/intellij/platform/vcs/impl/shared/ui/RepositoryColorsApi.kt`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/VcsLogColorManagerFactory.kt`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/graph/DefaultColorGenerator.kt`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/VcsLogStandardColors.java`

---

## 1. 背景与目标

将 JetBrains IntelliJ IDEA Darcula / Islands Dark 的完整色彩科学与图标语义沉淀为统一的主题基础设施，包括：
1. 全局 UI 语义色板与文件变更状态着色器；
2. 14 色高辨识度分支轨道调色板 + 基于分支名稳定哈希与 HSB 动态回退的 `BranchColorGenerator`；
3. 标准 Unicode 与 Nerd Font 可动态切换的 `GlyphSet` 字形规范表。

---

## 2. 核心实现任务清单

- [x] **`ThemePalette` 全局语义色板实现**：
  - 完整录入 [Spec 01 第 2 节](../spec.md#2-darcula--islands-dark-全局语义色板) 规定的画布背景、面板层、焦点/非焦点选区色（`#2E436E` / `#393B40`）、当前分支微亮行底色（`#252930`）、文件状态色（`M`/`A`/`D`/`R`/`?`/`!`）及 Diff 行级/词级背景色。
  - 实现 TrueColor 自动检测（`COLORTERM`、`WT_SESSION`、`TERM_PROGRAM`）及 CIE76 最近邻 ANSI-256 色降级转换函数 `rgb_to_ansi256(r, g, b)`。
- [x] **Darcula 14 色分支轨道管理器 (`BranchColorManager`)**：
  - 定义 14 组 `(track_fg: Color, muted_bg: Color)` 常量数组。
  - 实现对标 `GraphColorManagerImpl.kt` 的稳定哈希映射：剥离已知远程前缀（如 `origin/`）后计算确定性哈希（保证 `feature/x` 与 `origin/feature/x` 获得相同色相），`main` / `master` 固定映射至 0 号主色。
  - 实现对标 `DefaultColorGenerator.kt` 的超 14 轨 HSB 回退算法（当 `color_id` 为未命名远古分叉片段且发生冲突时，利用 `saturation = 0.50, brightness = 0.78` 计算高辨识度 RGB）。
- [x] **双模字形系统 (`GlyphMode::Unicode` & `GlyphMode::NerdFont`)**：
  - 封装统一结构体 `GlyphSymbols`：提供 Pill 左右端帽（`▐`/`▌` vs `\u{e0b6}`/`\u{e0b4}`）、引用类型图标（`HEAD`、`LocalBranch`、`RemoteBranch`、`Tag`、`Warning`、`FavoriteStar`）、同步箭头（`↑`/`↓`）、树折叠三角（`▼`/`▶`）以及图轨道字符集（`●`、`◎`、`○`、`│`、`┆`、`╭`、`╮`、`╰`、`╯`、`─`、`├`、`┤`、`┼`、`▾`、`▴`）。
  - 支持快捷键（如 `F12` 或设置项）运行期即时热切换 `Unicode <-> NerdFont` 模式。

---

## 3. 关键接口草案

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphMode {
    Unicode,
    NerdFont,
}

pub struct BranchColorManager {
    palette_14: [(Color, Color); 14],
    cache: FxHashMap<i32, Color>,
}

impl BranchColorManager {
    pub fn color_id_for_branch_name(ref_name: &str) -> i32;
    pub fn get_track_color(&mut self, color_id: i32) -> Color;
}
```

---

## 4. 验收标准 (Acceptance Criteria)

- [x] 单元测试验证：`main` 与 `origin/main`、`feature/auth` 与 `origin/feature/auth` 计算出一致的主轨道色槽位。
- [x] 单元测试验证：14 色调色板所有轨道主色与 `#1E1F22` 背景的 WCAG 相对亮度对比度全部 ≥ 4.5:1。
- [x] 单元测试验证：`DefaultColorGenerator` HSB 回退对任意正负 `i32` 输入（含 `i32::MIN`、`0`、连续递增序列）均不溢出且输出合法非暗黑 RGB。
- [x] 提供内置调色板预览调试视图（或快照测试），直观核验 Unicode 与 NerdFont 两套字形的显示宽度（`unicode_width` 校验无错位）。

---

## Comments

### 2026-11-18 — implemented

- 代码：`crates/jbgit-tui/src/theme/{mod,color,glyphs,branch_colors}.rs`；稳定哈希 `branch_color_id` 位于 `crates/jbgit-core/src/refs.rs`（供 Spec 02 图布局复用）。
- **偏离 spec**：Indigo 轨道色 `#7B6FF6` 对 `#1E1F22` 仅 4.27:1，不满足本 issue 的 ≥4.5 验收；保持色相、略降饱和度改为 `#8176F6`（4.60:1）。其余 13 色原样保留。
- ANSI-256 降级为真正的 CIE76（sRGB→Lab，仅匹配 16..=255，避开用户可改的 0..15）；TrueColor 按 `COLORTERM`、`WT_SESSION`、`TERM_PROGRAM` 自动检测。
- `BranchColorManager::get_track_color` 为 `&self` 且无缓存（槽位查表 O(1)，渲染期只持有 `&AppState`）；`BranchColorId` 封装分支颜色 id，行级撞色顺移与 >14 轨 HSB 回退在 `assign_visible`。
- 字形热切换：`F12`（保留 `Ctrl+G`）；调色板预览：`F9`（含 WCAG 比值、语义令牌、胶囊样例、字形宽度标尺）。
