# 05: Log 筛选栏 140 列溢出治理、筛选弹窗 `Ctrl+U` 及新建 Worktree 弹窗宽度自适应

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M11 — Pilot QA 缺陷修复与体验加固
Priority: P2
QA-Refs: `P2-06`, `P2-13`

- **关联文件**：
  - `crates/jbgit-tui/src/views/log.rs`（`render_filter_bar` `:324-682`）
  - `crates/jbgit-tui/src/input/keyboard.rs`（`route_modal_key` `FilterEditor | GoToRevision | ChooseParent | ChooseChild | CommitHistory` `:213-387`）
  - `crates/jbgit-tui/src/views/dialogs.rs`（`render_input` `:1367-1435`，参考 `render_choice` `:1503-1507`）
  - `crates/jbgit-tui/src/dialogs.rs`（`WorktreeCreate::path_dialog` `:137-162`）
  - `crates/jbgit-tui/src/i18n/locales/{zh-CN,en-US}.toml`（`:715`, `:1292-1311`, `:1939`）
  - `crates/jbgit-tui/src/tests/pilot_findings_tests.rs`（`log_filter_bar_prioritizes_active_chips_and_condenses_inactive_at_140x40` `:51-113`）

---

## 1. 根因定位

### 1.1 [`P2-06a`] `140x40` 下 Log 筛选栏激活长分支 chip 后挤掉 `路径▾` 与 `[排除合并]`
按默认三栏比例（左 `1/6`、中右 `0.6`），`140x40` 视口下中部 Log 面板宽约 `69` 列：
- 右侧齿轮 ` ⚙ ` 占 `3` 列（`views/log.rs:343-357`）；
- 未聚焦空搜索框占 `(69 / 4).clamp(10, 20) = 17` 列 + `[.*]`（4）+ `[Cc]`（4）= `25` 列（`:387-388`）；
- 激活的分支 chip（如 `feature/linear-chain`）在 `:541-555` 硬编码 `truncate_to_width(&label, 18)` 且不预留后续未激活按钮宽度，独占 `" 分支: feature/linear-ch… ▾"`（27）+ `"×"`（1）= `28` 列，消耗到 `x = 53`，仅剩 `13` 列；
- 剩余未激活触发器需要 `" 作者▾"`（6）+ `" 日期▾"`（6）+ `" 路径▾"`（6）+ `" [排除合并] "`（12，且从未压缩）= `30` 列；放完 `作者▾`、`日期▾` 后，`路径▾` 与 `[排除合并]` 在 `:660-681` 因 `put() == false` 被静默丢弃。

### 1.2 [`P2-06b`] 作者筛选弹窗（`按作者筛选`）按 `Ctrl+U` 无法清空输入
- `crates/jbgit-tui/src/input/keyboard.rs:213-387`（`ModalLayer::FilterEditor | GoToRevision | ChooseParent | ChooseChild | CommitHistory`）处理了 `Backspace`（`:354`）与字符输入（`:374`），但**缺少 `KeyCode::Char('u') if CONTROL` 分支**，导致 `Ctrl+u` 落入 `_ => Vec::new()`（而 `FilterText`、`BranchesSearch`、`DiffSearch`、`ShortcutsHelp`、`BranchesPopup` 均已支持 `Ctrl+u` 清空）。

### 1.3 [`P2-13`] 新建 Worktree 第二步弹窗在 `140x40` 下截断 `--force` 复选框提示
- `crates/jbgit-tui/src/views/dialogs.rs:1375` 在 `render_input` 中**硬编码弹窗总宽度为 `64` 列**（`let rect = centered(screen, 64, h);`，内部宽仅 `62` 列）；
- `:1404-1412` 直接拼接 `format!(" [{}] {label}   ({})", ..., ctx.t("dialog.checkbox.tab_hint"))`：
  - `zh-CN`：`5 + 46 + 4 + 8 + 1 = 64 > 62` → 截断为 `…(Tab 切`；
  - `en-US`：`5 + 62 + 4 + 3 + 1 = 75 > 62` → 截断更严重。

---

## 2. 任务清单

- [x] **动态预算分配修复 Log 筛选栏 `140x40` 溢出（`P2-06a`）**：
  - 在 `crates/jbgit-tui/src/views/log.rs:render_filter_bar` 中，绘制激活 chip 前先计算剩余未激活触发器所需的紧凑最小宽度（压缩后的各 `" {title}▾"` + 紧凑版排除合并按钮，如 `condense_inactive` 时去首尾多余空格 `"[排除合并]"` / `"[No Merges]"`）；
  - 当存在激活 chip 且空间偏紧时：
    1. 未聚焦空搜索框允许进一步收缩（如 `clamp(8, 12)`）；
    2. 激活 chip 的标签截断上限根据 `available - reserved_inactive` 动态计算（保留可读最小宽度如 `8` 列，而非固定 `18` 列），确保 `140x40` 下单个长分支筛选激活时 `作者▾`、`日期▾`、`路径▾`、`[排除合并]` 全部可见且可点击；
  - 若极窄视口（如 `80x24`）下仍放不下，将 `排除合并` 等入口同时保证在 `⚙` 齿轮菜单中可达。
- [x] **为 `FilterEditor` / `GoToRevision` / `CommitHistory` 弹窗补齐 `Ctrl+U` 清空（`P2-06b`）**：
  - 在 `crates/jbgit-tui/src/input/keyboard.rs:354` 附近增加 `KeyCode::Char('u') if ctrl && !matches!(kind, PopupKind::ChooseParent | PopupKind::ChooseChild)` 分支，派发 `Action::PopupSetState { query: String::new(), selected: 0 }`。
- [x] **使 `render_input` 宽度随内容自适应并安全截断复选框文案（`P2-13`）**：
  - 在 `crates/jbgit-tui/src/views/dialogs.rs:1367-1435` 动态计算弹窗宽度：`width = 64.max(title_w).max(prompt_w).max(checkbox_w).max(secondary_checkbox_w) + 4`，传入 `centered(screen, width, h)`（内部自动夹紧至 `screen.width - 2`）；
  - 当终端本身较窄导致仍超过 `inner.width` 时，仿照 `render_choice`（`:1503-1507`）用 `truncate_to_width` 截断复选框正文并完整保留末尾 `({hint})` 提示，确保不贴边裁剪提示语。

---

## 3. 测试与验收

- 扩展 `crates/jbgit-tui/src/tests/pilot_findings_tests.rs:51-113`：在 `140x40` 下激活 `Branch = feature/linear-chain`，断言渲染文本同时包含 `路径` 与 `排除合并` 且两者均注册了有效 `HitTarget`；
- 新增 `FilterEditor`（Author / Branch / Date / Paths）按键测试：输入 `@me` 后按 `Ctrl+u`，断言 `query` 清空为 `""`；
- 新增 `render_input` `TestBackend` 测试：在 `140x40` 下以 `zh-CN` 和 `en-US` 渲染 `WorktreeCreate::path_dialog`，断言完整显示 `(Tab 切换)` / `(Tab)` 无截断；在 `50x16` 窄屏下断言仍保留 `({hint})` 且不越界。

## Comments

- **2026-11-25**: Resolved in `0cbafbf`; workspace gates (`fmt`, `clippy`, `test --workspace --locked`) and `jbgit-pilot` ConPTY E2E re-verification (`full` `P2-06`, `multi-repo` `P2-13`) passed.
