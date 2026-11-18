# 01: 初始化 Cargo Workspace、异步 TUI 运行时与空间矩形 Hit-Test 框架

Status: ready-for-agent
Blocked by: none
Spec: [../spec.md](../spec.md)
Milestone: M1 — 底座、主题与混合引擎

- **参考源码**：
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/frame/MainFrame.java`（`OnePixelSplitter` 比例管理与焦点遍历策略）

---

## 1. 背景与目标

搭建 `jGit` 三层 Cargo Workspace（`jgit-core`、`jgit-engine`、`jgit-tui`），实现基于 `tokio` + `crossterm` + `ratatui` 的非阻塞 60 FPS 终端主循环，并内置两大核心基础设施：
1. **1-Cell 细线可拖拽分屏系统（`OneCellSplitter`）**：还原 IDEA 无冗余外框的极简多分屏视觉与鼠标拖拽缩放体验；
2. **即时模式空间矩形命中注册表（`HitTestRegistry`）与多层模态栈（Z-Order Router）**：让任意自定义控件在渲染时声明点击/悬停/右键热区，支撑后续鼠标右键菜单与轨道节点点击。

---

## 2. 核心实现任务清单

- [ ] **Workspace 脚手架搭建**：
  - 创建根 `Cargo.toml`（Rust 2024 Edition，统一配置 `[workspace.dependencies]`：`ratatui`、`crossterm`、`tokio`、`git2`、`anyhow`、`thiserror`、`tracing`、`unicode-width`、`rustc-hash`）。
  - 创建 `crates/jgit-core`、`crates/jgit-engine`、`crates/jgit-tui`（含二进制入口 `jgit` CLI 参数解析：支持 `jgit [path]` 打开指定目录或向上探查最近 `.git` 根目录）。
- [ ] **终端生命周期与异常安全恢复**：
  - 启用 Alternate Screen、Raw Mode、`EnableMouseCapture`、`EnableBracketedPaste`、`PushKeyboardEnhancementFlags`（按终端支持能力优雅启用以识别 `Ctrl+Enter` 等组合键）。
  - 安装 `std::panic::set_hook` 守护：发生任何 panic 时先还原终端状态再打印堆栈，杜绝终端乱码锁死。
- [ ] **单向状态流（TEA）主事件循环**：
  - 实现 `AppState`、`Action`、`Effect` 分发管道，集成 `crossterm::event::EventStream`、渲染 Tick 定时器（按需脏标记重绘 `needs_redraw` 降低空载 CPU 至 0%）以及后台引擎事件接收通道。
- [ ] **`HitTestRegistry` 空间命中系统**：
  - 定义 `HitTarget`、`HitRegion { rect: Rect, z_index: u8, target: HitTarget }`。
  - 实现按 `z_index` 降序及后注册优先的 `hit_test(col: u16, row: u16) -> Option<&HitRegion>`。
  - 处理鼠标左键按下、拖拽状态跟踪（`DragState`）、双击检测（≤ 300ms 且坐标一致）、滚轮路由（滚轮事件发送给**光标所在面板**而非当前键盘聚焦面板，对齐桌面 IDE 体验！）以及鼠标右键预选中分发。
- [ ] **`OneCellSplitter` 可拖拽分屏容器**：
  - 支持水平分屏（`│` 单列分隔）与垂直分屏（`─` 单行分隔），持久化各分屏比例 `f32`，强制最小列宽/行高约束，支持鼠标拖拽与双击恢复默认比例。
- [ ] **顶栏与工具窗口标签页切换骨架**：
  - 渲染顶部全局栏 + `[1 Commit]` / `[2 Log]` / `[3 Stash]` / `[4 Console]` 标签页切换及底部上下文快捷键提示栏。

---

## 3. 关键接口草案

```rust
pub trait ViewComponent {
    fn render(&self, frame: &mut Frame, area: Rect, ctx: &mut RenderContext);
    fn handle_key(&mut self, key: KeyEvent, state: &mut AppState) -> Handled;
    fn handle_mouse(&mut self, mouse: MouseEvent, target: &HitTarget, state: &mut AppState) -> Handled;
}

pub struct RenderContext<'a> {
    pub theme: &'a ThemePalette,
    pub glyph_mode: GlyphMode,
    pub hit_registry: &'a mut HitTestRegistry,
    pub focused_pane: PaneId,
}
```

---

## 4. 边界情况与不变量

- 终端窗口被缩放到极小尺寸（如 `< 60x16`）时不得发生无符号整数下溢 panic（所有 `Rect` 切分使用饱和算术），应优雅显示窗口过小提示或自动折叠次级分屏。
- 后台无事件且无动画时不得空转重绘，静默状态 CPU 占用率应接近 `0%`。

---

## 5. 验收标准 (Acceptance Criteria)

- [ ] 运行 `cargo run -p jgit-tui` 能正常进入全屏 TUI，`q` 或 `Ctrl+C` 干净退出恢复原终端屏幕。
- [ ] 支持用 `1/2/3/4` 或鼠标点击切换四个顶层标签页，`Tab` 在分屏焦点环间流转并点亮焦点指示条。
- [ ] 鼠标按住任意 `│` 或 `─` 分割线左右/上下拖动可实时丝滑改变面板比例，双击分割线恢复默认比例。
- [ ] 单元测试覆盖 `HitTestRegistry` 的多层 `z_index` 遮挡拦截（例如浮窗遮挡底层表格时点击不会穿透到底层）。

---

## Comments
