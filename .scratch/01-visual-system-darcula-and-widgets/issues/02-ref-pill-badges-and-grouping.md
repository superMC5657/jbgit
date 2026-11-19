# 02: 实现 IDEA 风格 Pill 引用徽标组件（跟踪分支融合、紧凑折叠与双模渲染）

Status: ready-for-agent
Blocked by: 01
Spec: [../spec.md](../spec.md)
Milestone: M2 — 非交叉 DAG 图与 Pill 徽标

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/log/GitRefManager.kt` (`groupForTable`, `GitLabelComparator`, `createTrackedGroup`)
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/render/LabelPainter.java`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/render/VcsLogLabelPainter.kt`

---

## 1. 背景与目标

高保真复刻 IDEA 提交行首部的彩色圆角胶囊引用徽标（Pill Badges），不仅在视觉上提供标准 Unicode 半块胶囊与 Nerd Font 圆角胶囊双形态，更在逻辑上精准还原 `GitRefManager.groupForTable` 的智慧降噪机制（如 `HEAD` 融入当前分支胶囊、同提交本地与远程跟踪分支自动合并为 `origin & feature/x` 双色胶囊、超宽自动折叠 `+N`）。

---

## 2. 核心实现任务清单

- [x] **引用分组与融合引擎 (`jbgit-core::refs::group_refs_for_table`)**：
  - 实现 `GitLabelComparator` 排序：`HEAD` -> `CURRENT_BRANCH` -> `main/master` -> `origin/main` -> `LOCAL_BRANCH` -> `REMOTE_BRANCH` -> `TAG` -> `OTHER`。
  - **跟踪分支同位融合（对标 `createTrackedGroup`）**：当本地分支 `B` 与其配置的上游跟踪分支（或匹配的 `<remote>/B`）指向同一个 Commit Oid 时，自动将二者合并为单个 `RefGroup::TrackedPair { remote: "origin", branch: "B" }`，显示文本为 `"origin & B"`，附带双色标识（`#3CB45C` + `#9F79B5`）！
  - **HEAD 融合与 Detached HEAD 独立告警**：当仓库处于正常分支 `B` 上时，将 `HEAD` 金色图标并入 `B`（或 `origin & B`）胶囊最前端；当处于 Detached HEAD 时，独立生成首枚橙色警告胶囊 `⚠ HEAD`（悬停提示游离头状态说明）。
  - 支持 `compact: bool` 与 `show_tag_names: bool` 两个视图控制开关。
- [x] **宽度预算与智能缩写 (`LabelPainter` 移植)**：
  - 实现 `shorten_ref_name(name, max_len = 22, available_cols)`：超长带 `/` 分支名优先将首段缩写为 `../suffix`，仍超长则尾部截断加 `…`。
  - 确保为右侧提交 Subject 至少预留 `MIN_SUBJECT_COLS = 24` 列；超出可用预算的剩余引用组折叠为末尾汇总胶囊 `+K`。
- [x] **双模 TUI `PillBadgeBar` 渲染控件与鼠标命中**：
  - 在 `GlyphMode::Unicode` 下使用 `▐` (`fg=pill_bg, bg=row_bg`) + 内部高亮图标与文字 + `▌` (`fg=pill_bg, bg=row_bg`) 渲染无缝胶囊；
  - 在 `GlyphMode::NerdFont` 下切换为 `\u{e0b6}` (``) 与 `\u{e0b4}` (``) 圆角半圆端帽；
  - 为每枚渲染出的胶囊单独注册 `HitTarget::CommitRefPill { visible_row, ref_name }`，支持后续直接右键点击某个特定分支胶囊对其执行 Checkout / Delete / Rename！

---

## 3. 边界情况与不变量

- 包含 CJK 中文字符或 Emoji 的分支/标签名（如 `feat/登录优化`）：必须通过 `unicode_width::UnicodeWidthStr` 计算精确终端占列数，杜绝胶囊右端帽错位或覆盖提交标题。
- 选中行背景适配：当所在提交行被选中（背景变为深蓝 `#2E436E`）时，胶囊外侧端帽背景色 `row_bg` 必须同步取 `#2E436E`，避免胶囊四角出现黑色锯齿黑边！

---

## 4. 验收标准 (Acceptance Criteria)

- [x] 单元测试验证：同一提交同时拥有 `HEAD`、`refs/heads/main`、`refs/remotes/origin/main`、`refs/tags/v1.0.0` 时，`group_refs_for_table` 输出 `[HEAD + Tracked("origin & main"), Tag("v1.0.0")]` 两枚胶囊而非四枚杂乱标签。
- [x] 单元测试验证：Detached HEAD 状态下输出独立的 `DetachedHead` 警告胶囊排在最左。
- [x] `TestBackend` 渲染测试：验证窄列宽下的 `../` 缩写、`+N` 溢出胶囊以及选中行下端帽底色无黑边。

---

## Comments

### 2026-11-18 — implemented

- 分组/排序/缩写：`crates/jbgit-core/src/refs.rs`（`group_refs_for_table`、`shorten_ref_name`、`natural_cmp`）。HEAD 通过合成的 `RefInfo::head_marker` 传入：有 `is_head` 本地分支则并入其胶囊，否则生成最左 `DetachedHead`。
- 渲染：`crates/jbgit-tui/src/widgets/pill_badge.rs`，布局为纯函数 `layout_pills`；`+N` 溢出胶囊注册 `HitTarget::CommitRefOverflow { visible_row, hidden_refs }`，悬停展开全部引用浮层、右键弹出按隐藏引用分组的子菜单。
- 双色跟踪胶囊：Unicode 用 `⎇⇄`，NerdFont 用 `\u{e0a0}` + `\u{f444}`（双色点）。
- Compact / Show Tag Names：Log 表格中按 `c` / `t` 切换，或右键 “Reference View Options”。
- 已在 WezTerm 无头 mux 中运行真实二进制核验（`../user-a…`、`+3`、选中行端帽无黑边、CJK 对齐）。
