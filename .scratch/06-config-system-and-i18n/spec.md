# Spec 06: Yazi 风格配置体系与全量中文/国际化支持规约

- **目录**：`.scratch/06-config-system-and-i18n/`
- **下属 Issues**：
  - [`01-config-discovery-and-yazi-multi-file-system.md`](./issues/01-config-discovery-and-yazi-multi-file-system.md)
  - [`02-i18n-localization-engine-and-cjk-support.md`](./issues/02-i18n-localization-engine-and-cjk-support.md)
  - [`03-theme-flavors-and-dynamic-keymaps.md`](./issues/03-theme-flavors-and-dynamic-keymaps.md)
- **设计共识文档**：
  - `jgit-i18n-config-design.md`（经用户确认批准的设计架构）
- **参考架构**：
  - [Yazi Configuration](https://yazi-rs.github.io/docs/configuration/overview)（`yazi.toml`、`theme.toml`、`keymap.toml`、`include_str!` 内置默认配置与用户分层覆盖机制）

---

## 1. 架构目标与职责边界

将原先硬编码于代码内部的配置项（文字文案、配色方案、快捷键映射、显示模式）解耦为基于 TOML 的配置引擎与多语言（i18n）系统：

```
~/.config/jgit/ (或 %APPDATA%/jgit/config/)
├── jgit.toml          # 通用与行为配置 (语言/图表后端/Diff行为)
├── theme.toml         # 主题配色 (Flavors: darcula/catppuccin/gruvbox + 语义覆写)
├── keymap.toml        # 快捷键映射 (Yazi 格式的 key_bindings 数组)
└── locales/           # [可选] 用户自定义语言扩展包
    └── zh-CN.toml
```

---

## 2. 详细规范分解

### 2.1 配置发现与多层覆盖 (`01-config-discovery-and-yazi-multi-file-system`)
1. **优先级链**：
   - `JGIT_CONFIG_DIR` 环境变量
   - `.jgit/jgit.toml`（仓库根目录局部配置）
   - 全局用户配置（Windows: `%APPDATA%\jgit\config`，Unix: `~/.config/jgit`）
   - 二进制嵌入的默认配置（`include_str!`）
2. **CLI 命令**：
   - `jgit --print-default-config [file]`
   - `jgit --init-config`

### 2.2 国际化引擎与 CJK 支持 (`02-i18n-localization-engine-and-cjk-support`)
1. **多语言词条**：
   - 内置 `locales/zh-CN.toml` 与 `locales/en-US.toml`。
   - `I18nManager` 提供类型安全或宏驱动的文本查找与插值。
   - 替换全局所有界面文案：顶部菜单、分支浮窗、上下文菜单、提交工作台、状态提示。
2. **CJK 等宽与输入法**：
   - 确保 `unicode-width` 跨栏对齐，文本框支持输入法与中文粘贴。

### 2.3 主题 Flavors 与动态 Keymap (`03-theme-flavors-and-dynamic-keymaps`)
1. **主题系统**：
   - 内置 `darcula`（默认）、`catppuccin-mocha`、`gruvbox-dark`。
   - 支持在 `theme.toml` 中使用 16 进制颜色微调语义调色板。
2. **按键映射系统**：
   - 对齐 Yazi 的 `key_bindings`，支持 `<C-x>`、`<A-x>` 及连续按键序列（如 `g g`）。
   - 按 `?` 时动态显示带当前快捷键与说明的帮助窗口。
