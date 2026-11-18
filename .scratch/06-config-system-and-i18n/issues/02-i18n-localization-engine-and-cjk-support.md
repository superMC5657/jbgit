# 02: 实现 i18n 国际化引擎、TOML 语言包与全量中文/CJK 支持

Status: resolved
Blocked by: 06-config-system-and-i18n/issues/01-config-discovery-and-yazi-multi-file-system.md
Spec: [../spec.md](../spec.md)
Milestone: M6 — 配置体系与国际化

---

## 1. 背景与目标

全面解决“增加中文支持”需求。构建多语言（i18n）抽象层，支持中英文双语包（内置 + 用户目录外部扩展），全面汉化现有 UI 界面硬编码文案（菜单、弹窗、状态栏、工作台等），并验证 CJK 字符对齐与输入法在终端中的流畅使用。

---

## 2. 核心任务清单

- [x] **多语言引擎设计 (`I18nManager`)**：
  - 支持 `zh-CN`、`en-US`、`auto`（自动检测当前操作系统的 locale 或环境变量 `LC_ALL`/`LANG`/Windows UI Language）。
  - 支持从二进制内置的 `locales/zh-CN.toml` 与 `locales/en-US.toml` 加载，同时支持加载用户配置目录 `<config_dir>/locales/` 下的外部覆盖包。
  - 提供简单易用的词条查询接口与格式化参数插值支持（如 `{name}`、`{count}`）。
- [x] **全量语言包编制与文案整理**：
  - 编制涵盖所有 UI 模块的词条清单：
    - `menu.*`：分支菜单、提交菜单、暂存菜单、Stash 菜单等
    - `dialog.*`：新建分支、变基/合并确认、删除确认、Squash、交互式变基等弹窗
    - `view.*`：顶栏、状态栏、提交工作台标题、文件树、Diff 面板、冲突横幅等
    - `toast.*` / `action.*`：各类操作反馈与错误提示
- [x] **UI 界面文案全量替换**：
  - 改造 `crates/jgit-tui/src/menus/`（`branch.rs`, `log.rs`, `stage.rs`, `stash.rs`）。
  - 改造 `crates/jgit-tui/src/dialogs.rs` 与 `views/dialogs.rs`。
  - 改造 `crates/jgit-tui/src/views/`（`header.rs`, `banner.rs`, `commit.rs`, `log.rs` 等）。
- [x] **CJK 宽字符与输入法支持保障**：
  - 确保文本截断与渲染统一基于 `unicode-width` 进行列宽计算（避免中文字符导致表格线或边框破损）。
  - 确保输入框（`InputDialog`、`CommitMessageEditor`、分支模糊速查）在各终端平台（Windows Terminal、Alacritty 等）下支持正常输入中文与粘贴中文。

---

## 3. 验收标准

- [x] 配置 `language = "zh-CN"` 时，启动 `jgit` 界面全中文呈现，词条无硬编码英文残留。
- [x] 配置 `language = "en-US"` 时，界面还原纯正标准英文。
- [x] 在提交说明编辑器中输入与粘贴中文，光标位置与字符显示无错位。
- [x] 包含中文字符的分支名、提交注释及文件路径在 Commit Graph、表格与文件树中对齐整齐，无视觉错位。

---

## Comments

- 2026-11-18 (agent): Issue 02 已完成。
  - 实现了 `crates/jgit-tui/src/i18n/` 模块与 `I18n` 结构：
    - 编制了 `locales/zh-CN.toml` 与 `locales/en-US.toml` 完整词条（common, tabs, menu, dialog, banner, view, toast）。
    - 支持自动检测系统语言 (`auto`)、指定语言 (`zh-CN` / `en-US`) 以及从 `<config_dir>/locales/` 覆盖自定义文案。
    - 提供了 `t(&str)` 与 `t_fmt(&str, &[(&str, &str)])` 插值方法。
  - 将 `i18n: Arc<I18n>` 接入 `AppState` 与 `RenderContext`。
  - 全面汉化并解耦硬编码英文字符串：
    - `menus/branch.rs`: 检出、新建分支、变基、合并、对比、更新、推送、重命名、删除等。
    - `menus/log.rs`: 版本对比、拣选、撤销、重置、Reword、Squash、Drop、补丁与导航等。
    - `menus/stage.rs`: 暂存/取消暂存、冲突解决、Stash、.gitignore 等。
    - `menus/stash.rs`: Apply、Pop、创建分支、清空等。
    - `views/header.rs`: 顶部标签页 (Commit/Log/Stash/Console) 与进行中文态。
    - `views/dialogs.rs`: 统一使用 `ctx.ok()` / `ctx.cancel()` 多语言按钮。
    - `views/commit.rs`: 提交按钮 (追加提交/提交/推送)、选项行等。
    - `branches_popup.rs`: 分支浮窗分组标头与操作项。
  - 编写了 5 个 i18n 专项单元测试，验证了中文语言包、英文语言包、外部覆盖及中文分支操作项生成。全工作区测试 100% 通过（123 + 59 = 182 passed）。
