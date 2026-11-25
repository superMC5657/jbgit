# 03: 弹窗窄屏布局、标题栏溢出、叠层边框对齐与菜单宽度修正

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M12 — 工作流正确性与 UI 交互加固
Priority: P2

- **关联文件**：
  - `crates/jbgit-tui/src/views/conflict_merge.rs`（`render_conflict_merge_modal` `:130-185`, 底部按钮行 `:755-776`）
  - `crates/jbgit-tui/src/views/log.rs`（`render_branch_compare_bar` `:250-320`）
  - `crates/jbgit-tui/src/tree_browser.rs`（`render_tree_browser` `:736-780`）
  - `crates/jbgit-tui/src/views/annotate.rs`（`render_annotate_modal` `:21-55`）
  - `crates/jbgit-tui/src/views/file_history.rs`（`render_file_history_modal` `:27-60`, 工具栏 `:119-229`）
  - `crates/jbgit-tui/src/views/overlays/context_menu.rs`（`context_menu_rect` `:42-67`, 渲染宽度 `:208-212`）

---

## 1. 根因定位

### 1.1 Merge Conflict 弹窗在窄屏（`80x24` / `50x16`）下按钮截断与底层文字穿透
- `crates/jbgit-tui/src/views/conflict_merge.rs:140-149` 固定使用 `width = screen.width.saturating_sub(4).min(136)` 与 `height = screen.height.saturating_sub(2).min(34)`：
  - 在 `80x24` / `50x16` 下，左右两侧固定留出 2 列未清屏空隙（且 `draw_shadow` 仅调暗背景色而不遮蔽字符），底层 Diff Preview 的双宽/边缘字符（如 `D)`、`r…`、`xt`）从弹窗边框外透出；同时 `50x16` 下 `y = 1` 压住了顶部标签栏。
  - 底部按钮区（`:755-776`）将最多 11 个按钮硬塞进单行，遇到 `bx + w > inner.right()` 直接 `break`，导致 `80x24` 裁掉 `关闭`，`50x16` 裁掉 `重置`、`合并工具`、`保存`、`关闭` 等核心主操作。

### 1.2 两分支对比标题栏开启 `树差异 ✓` 后计数被截断且 `✕` 被挤出边界
- `crates/jbgit-tui/src/views/log.rs:274-295` 存在 **1 列宽度预算偏差**：
  ```rust
  let btns_w = cycle_btn.width() + diff_btn.width() + close_btn.width() + 2;
  let label_w = (area.width as usize).saturating_sub(btns_w + 1);
  let mut spans = vec![Span::styled(format!(" {} ", truncate_to_width(&label, label_w)), ...)];
  ```
  `format!(" {} ", ...)` 实际占用 `label_w + 2`（左右各 1 空格），但 `label_w` 只减去了 `1`。总宽度超出 `area.width` 1 列，末尾的 `close_btn`（`" ✕ "`，宽 3）触发 `if x + w > area.right() { break; }` 直接消失；且开启 `树差异 ✓`（+2 列）时，左段把分支名和 `(← 1 · → 1)` 拼成单一字符串做末尾截断，优先裁掉了计数 `(← 1 · → …`。

### 1.3 `Alt+T` Tree Browser 弹窗在 `140x40` 下最右列露字
- `crates/jbgit-tui/src/tree_browser.rs:746-753` 使用 `Rect::new(screen.x + 1, screen.y + 1, screen.width - 2, screen.height - 2)` 并调用 `draw_shadow(frame, screen, rect)`，在 `col 0` 与 `col 139` 留出 1 列窄缝，底层 Log 表格字符（如 `─`、`8`）在右缝 `col 139` 暴露出来。

### 1.4 Annotate 叠开行历史（`H`）在第 36–37 行出现双层堆叠底边框
- 对比 `crates/jbgit-tui/src/views/annotate.rs:39-51`（`height = 90%` → `140x40` 下为 `36` 行，`y = 2..38`，底边在第 37 行）与 `crates/jbgit-tui/src/views/file_history.rs:45-57`（`height = 88%` → `140x40` 下为 `35` 行，`y = 2..37`，底边在第 36 行）：
  - 两者宽度同为 `92%`（128 列）、顶边同为 `y = 2`，仅高度相差 **1 行**，导致 FileHistory 叠在 Annotate 之上时，第 36 行与第 37 行紧挨着渲染两条 `╰───╯` 底边框。

### 1.5 File History 在 `80x24` 窄屏下顶部工具栏右侧按钮裁切
- `crates/jbgit-tui/src/views/file_history.rs:119-229` 在单行 `toolbar_rect` 上无条件平铺 12 个按钮（`范围`、`追踪重命名`、`对比本地`、`详情`、`受影响文件`、`并排/统一`、`上下文`、`空白`、`换行`、`全屏`、`Annotate`、`定位`）。在 `80x24` 下 `inner.width = 71`，右侧按钮超出 `area.right()` 被 `render_toolbar` 截断。

### 1.6 上下文菜单宽度少算 1–2 列导致最长项截断（`My Commit…`）
- `crates/jbgit-tui/src/views/overlays/context_menu.rs:57` 计算菜单外框宽度为 `i.label.width() + suffix_w + 4 + icon_w`，而渲染期（`:209`）左右边框占 `2` 列、内部前后空白与右侧分隔占 `3` 列（`avail = inner.width - right_label - 3 - icon_w`），合计固定开销为 **`5` 列**而非 `4` 列。因此菜单总宽度固定比最长标签少 1 列，导致 `✓ 高亮：我的提交 (My Commits)` 截断为 `✓ 高亮：我的提交 (My Commit…`。

---

## 2. 任务清单

- [x] **修复两分支对比标题栏宽度预算与计数保留**：
  - 修正 `crates/jbgit-tui/src/views/log.rs:274-295` 宽度公式：`label_w = (area.width as usize).saturating_sub(btns_w + 2)`，确保 `✕` 按钮始终完整可见（并在极窄屏下给按钮留足空间）；
  - 将 `(← N · → M)` 计数后缀与分支范围文本拆开截断：优先保留完整的 `(← N · → M)`，仅对过长的 `A...B` 分支名做省略。
- [x] **优化 Merge Conflict 弹窗窄屏占比、清屏与底部按钮自适应**：
  - 在 `crates/jbgit-tui/src/views/conflict_merge.rs:140-149`：当 `screen.width <= 84` 或 `screen.height <= 24` 时采用紧凑边距（如 `width = screen.width` 全宽或 `margin = 0`，避免在窄屏留 2 列漏字边缝；纵向起始行在 `screen.height >= 16` 时尽量避开 row 0/1 顶栏与标签栏）；
  - 底部操作区（`:755-776`）在单行放不下全部按钮时，优先保证核心主按钮（`保存`、`关闭`、`我方`/`对方`）始终可见——可按宽度自动换行到 2 行按钮栏，或采用紧凑标签 + 保留尾部 `保存` / `关闭` 的双端布局，不再粗暴 `break` 丢弃 `保存`/`关闭`。
- [x] **消除 `Alt+T` Tree Browser 右侧 1 列漏字**：
  - 调整 `crates/jbgit-tui/src/tree_browser.rs:746-753`：铺满 `screen.width` 或与 `FileHistory`/`Annotate` 统一为 `92%` 居中弹窗，不留 1 列残影缝隙。
- [x] **对齐 Annotate 与 File History 弹窗高度，消除双层底边框**：
  - 将 `crates/jbgit-tui/src/views/file_history.rs:45-50` 的高度比例统一为 `90%`（与 `views/annotate.rs:39-44` 一致），或在其叠在 `Annotate` 上方时显式内缩至少 2 行，消除仅差 1 行的紧贴双底边框。
- [x] **优化 File History 在 `80x24` 窄屏下的工具栏自适应**：
  - 在 `crates/jbgit-tui/src/views/file_history.rs:119-229`：当 `toolbar_rect.width < 90` 时为工具栏按钮提供紧凑/缩写标签（或在 80 列下折叠低频按钮至右键菜单并优先保留高频切换与 `全屏`），避免右侧按钮被裁切。
- [x] **修正上下文菜单外框宽度公式**：
  - 将 `crates/jbgit-tui/src/views/overlays/context_menu.rs:57` 中的 `+ 4` 修正为 `+ 5`（与 `:209` 的边框 `2` + 内边距 `3` 精确对齐），使 `✓ 高亮：我的提交 (My Commits)` 在 `140x40` 下完整显示无省略号。

---

## 3. 测试与验收

- **`TestBackend` 渲染测试（修复前必挂）**：
  - **两分支对比**：在 `140x40`（主面板宽 ~70 列）下开启 `branch_compare` + `tree_diff = true`（如 `main...feature/pull-me`），断言标题栏同时包含完整的 `(← 1 · → 1)` 与 `✕`，且 `HitTarget::BranchCompareClose` 已注册在可见区域内；
  - **Merge Conflict 窄屏**：在 `80x24` 与 `50x16` 下渲染 `render_conflict_merge_modal`，断言 `保存` 与 `关闭` 按钮及其 `HitTarget::ConflictMergeButton` 均存在，且无底层面板字符漏出弹窗右边框外；
  - **Tree Browser**：在 `140x40` 下渲染 `render_tree_browser`，验证边框对称无 1 列残影缝隙；
  - **Annotate + Line History 叠层**：在 `140x40` 下将 `FileHistory(line_range)` 叠在 `Annotate` 上渲染，断言不存在连续两行（第 36 与 37 行）紧贴的双层 `╰───╯` 底边框；
  - **File History 窄屏**：在 `80x24` 下渲染 `render_file_history_modal`，断言工具栏无半截裁切字符且核心按钮可用；
  - **上下文菜单宽度**：在 `140x40` 渲染 `presentation_menu_items`，断言 `高亮：我的提交 (My Commits)` 完整渲染无 `…`。

## Comments

- **2026-11-25**: 已修复并验证：
  - 两分支对比栏：修正 `label_w = area.width.saturating_sub(btns_w + 2)`，空间受限时先省去冗余前缀再按需截断分支名以保留完整 `(← N · → M)` 与 `✕`。
  - Merge Conflict 弹窗：窄屏（`width <= 84 || height <= 24`）采用全宽且起始于第 2 行（避开顶栏/标签栏），底部按钮动态换行至最多 2 行并固定保留 `保存` / `关闭`。
  - Tree Browser：宽屏统一为 `92% × 90%` 对称居中弹窗，窄屏全宽铺满，消除 1 列残影缝隙。
  - File History：高度对齐至 `90%` 消除叠在 Annotate 上时的双层底边框；`toolbar_rect.width < 90` 切换为中英对等紧凑标签，80 列下全部 12 枚工具栏按钮完整显示。
  - 上下文菜单：外框宽度公式修正为 `+ 5`。
  - **测试**：6 项 `TestBackend` 回归测试及全量工作区测试均通过。
  - **Pilot 实机复验（`full` + `conflict` 沙盒）**：`main...feature/pull-me` 开启 `树差异 ✓` 后完整显示 `(← 1 · → 1)` 与 `✕` 且点击 `✕` 正常关闭；`conflict` 场景 `80x24` / `50x16` 下无边缝漏字且全部 11 个按钮分布于 2 行；`Alt+T` 边框对称（cols `6..=133`）；Annotate 叠开 `H` 仅保留第 37 行单底边框；File History `80x24` 单行完整展示 `HEAD ⇵追踪 ⇄本地 ▤单列 ⤢全屏 ↕3 空白 折行 详情 文件 追溯 定位`；`⚙` 菜单完整显示 `✓ 高亮：我的提交 (My Commits)`。
