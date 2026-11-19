# 01: 实现 Yazi 风格多文件配置系统、层级发现引擎与 CLI 辅助命令

Status: resolved
Blocked by: 00-architecture-and-git-engine/issues/01-workspace-tui-runtime-and-hit-test.md
Spec: [../spec.md](../spec.md)
Milestone: M6 — 配置体系与国际化

- **参考架构**：
  - [Yazi Configuration](https://yazi-rs.github.io/docs/configuration/overview)

---

## 1. 背景与目标

建立统一的配置层，解耦硬编码参数。支持 `jgit.toml`、`theme.toml`、`keymap.toml` 多文件结构，提供嵌入式默认配置与三级覆盖机制（环境变量 > 仓库局部 > 全局用户目录 > 内置默认），并提供 CLI 模板生成辅助命令。

---

## 2. 核心任务清单

- [x] **依赖集成**：
  - 在根 `Cargo.toml` 与 `crates/jgit-tui/Cargo.toml` 中引入 `serde`（带 `derive` 特性）与 `toml`。
- [x] **配置路径发现引擎 (`ConfigDiscovery`)**：
  - 实现层级发现算法：
    1. `JGIT_CONFIG_DIR` 环境变量
    2. 项目根目录 `.jgit/jgit.toml`（或 `.jgit/config.toml`）
    3. 系统标准用户目录（Windows: `%APPDATA%\jgit\config\`；Unix: `~/.config/jgit/`）
    4. 二进制内嵌默认值（通过 `include_str!` 嵌入 `defaults/`）
- [x] **配置结构定义与反序列化**：
  - `jgit.toml`：
    - `[general]`：`language` ("zh-CN" / "en-US" / "auto")、`default_tab`、`flavor`
    - `[graph]`：`backend` ("auto" / "vector" / "cell")、`glyph_mode` ("nerd_font" / "unicode")、`compact_refs`
    - `[diff]`：`syntax_highlight`、`tab_size`、`context_lines`
    - `[layout]`：`log_ratio`、`commit_ratio`、`stash_ratio`、`console_ratio`（类似 Yazi 的栏位权重比例，接入 `SplitterState`）
- [x] **层级合并算法 (Layered Merge)**：
  - 将用户指定的非空配置层叠应用到底层默认配置上。
- [x] **CLI 辅助命令扩展**：
  - `jgit --print-default-config [jgit|theme|keymap]`：将内置默认配置打印至 stdout。
  - `jgit --init-config`：在当前系统的用户配置目录下自动生成带详尽中文注释的标准模板文件。

---

## 3. 验收标准

- [x] 在无任何外部配置文件的情况下，`jgit` 启动完全正常，默认加载内置配置。
- [x] 运行 `jgit --print-default-config jgit` 正确输出格式良好的默认 `jgit.toml` 内容。
- [x] 运行 `jgit --init-config` 能在用户配置目录下安全创建初始配置文件（若已存在则提示而不覆盖）。
- [x] 在用户目录或环境变量中配置 `language = "zh-CN"` 能够被配置引擎正确解析并装载入 `AppState`。

---

## Comments

- 2026-11-18 (agent): Issue 01 已完成。
  - 引入 `serde = { version = "1.0", features = ["derive"] }` 与 `toml = "0.8"`。
  - 建立 `crates/jgit-tui/src/config/` 模块：
    - `defaults/`: 内嵌默认带详尽中文注释的 `jgit.default.toml`、`theme.default.toml`、`keymap.default.toml`。
    - `jgit_config.rs`、`theme_config.rs`、`keymap_config.rs`: 完整反序列化与局部叠加合并（`merge_overlay`）。
    - `discovery.rs`: 实现三级发现链条（`JGIT_CONFIG_DIR` > `.jgit/` > `%APPDATA%\jgit\config` 或 `~/.config/jgit` > 内嵌默认）。
    - `mod.rs`: `AppConfig::load`、`print_default_config`、`init_config`。
  - `crates/jgit-tui/src/main.rs`: 支持 `--help`、`--print-default-config [NAME]` 及 `--init-config` 参数分发。
  - `crates/jgit-tui/src/app.rs`: `AppState` 接入 `config: AppConfig` 并根据配置项动态初始化默认 tab、glyph 模式等。
  - 单元测试与端到端运行验收全部通过（116+ 单元测试 pass）。
- 2026-11-19 (agent): 完成 Review 修复与增强：
  - 新增 Yazi 风格 `[layout]` 配置节（`log_ratio` / `commit_ratio` / `stash_ratio` / `console_ratio`），并通过强类型校验值对象 `TripleRatio` / `PairRatio` 与 `LayoutSection::splitter_defaults()` 解耦注入 `SplitterState::with_default_ratios`（支持双击分割线恢复配置比例，无效或零和覆写安全回退保留下层合法比例）。
  - 补充跨层配置回退、非法比例保留及 `SplitterState` 集成单元测试；新增并校准项目根目录 `README.md` 与 `jgit.default.toml` 注释。
