# 03: 实现主题预设 (Flavors)、细粒度调色板与动态按键映射系统

Status: resolved
Blocked by: 06-config-system-and-i18n/issues/01-config-discovery-and-yazi-multi-file-system.md
Spec: [../spec.md](../spec.md)
Milestone: M6 — 配置体系与国际化

- **参考架构**：
  - [Yazi theme.toml](https://yazi-rs.github.io/docs/configuration/theme)
  - [Yazi keymap.toml](https://yazi-rs.github.io/docs/configuration/keymap)

---

## 1. 背景与目标

通过 `theme.toml` 与 `keymap.toml` 分别解耦外观配色与按键交互。支持主流配色预设（Darcula、Catppuccin、Gruvbox）与细粒度色值覆盖，按键绑定完全支持 Yazi 规范的按键序列（含修饰键与 `g g` 序列），并提供基于当前生效 Keymap 的快捷键帮助视窗。

---

## 2. 核心任务清单

- [x] **主题预设与调色板扩展 (`theme.toml`)**：
  - 支持 `flavor` 预设选择：
    - `darcula`（原生 JetBrains 暗色，默认）
    - `catppuccin-mocha`
    - `gruvbox-dark`
  - 支持 `[palette]` 节点下的 16 进制颜色覆写（如 `bg_canvas = "#1e1f22"`）。
  - 支持 `[glyphs]` 节点配置 `mode = "nerd_font" | "unicode"`。
  - 将 `ThemePalette` 构建流程对接 `ThemeConfig`。
- [x] **动态按键映射引擎 (`keymap.toml`)**：
  - 定义 `KeyBinding` 结构，支持 `on = ["<C-d>"]`、`run = "compare_versions"`、`desc = "..."`。
  - 支持解析组合修饰键（`<C-x>`, `<A-x>`, `<S-x>`）与多键连续序列（如 `["g", "g"]`）。
  - 分模块定义作用域绑定：`[global]`、`[log]`、`[commit]`、`[branch]`、`[diff]`。
  - 重构 `crates/jgit-tui/src/input.rs` 的键盘事件分发逻辑，优先匹配动态注册的按键映射。
- [x] **动态快捷键帮助与 i18n 联动**：
  - 按 `?` 呼出的快捷键帮助弹窗从当前加载的 Keymap 动态生成内容，结合当前语言包显示本地化的操作说明。

---

## 3. 验收标准

- [x] 在 `theme.toml` 中配置 `flavor = "catppuccin-mocha"` 能正确切换全套界面配色。
- [x] 在 `theme.toml` 中单独覆写 `bg_canvas = "#000000"` 能够生效。
- [x] 在 `keymap.toml` 中将某个操作（如刷新）由默认的 `r` 改为 `F5` 或自定义快捷键，按键触发正确。
- [x] 按 `?` 打开的帮助视窗中显示的是当前生效的快捷键映射与对应的多语言说明。

---

## Comments

- 已完成 `ThemePalette::catppuccin_mocha` 与 `ThemePalette::gruvbox_dark` 全套 14 轨分支配色及语义色彩实现。
- `ThemePalette::from_config` 支持 `flavor` 预设选择与 `[palette]` 节点细粒度十六进制色值覆盖（如 `bg_canvas = "#000000"`）。
- 在 `crates/jgit-tui/src/keymap.rs` 中实现了符合 Yazi 规范的按键解析引擎（`<C-x>`, `<A-x>`, `<Space>`, `<F1..12>`, `g g` 多键序列与 500ms 超时和取消机制）。
- 重构了 `crates/jgit-tui/src/input.rs`，在底层优先接入动态按键调度，支持将刷新等动作重映射至 `F5` 等自定义按键并正确触发。
- 动态重构了快捷键帮助弹窗（`PopupKind::ShortcutsHelp`），实时读取当前激活的 Keymap 与 i18n 语言包动态渲染按键与说明，并支持上下方向键平滑滚动。
- 全工作区 127 项测试全部通过 (`cargo test --workspace` 100% passed)。
