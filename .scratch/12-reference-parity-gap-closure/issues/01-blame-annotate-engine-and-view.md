# 01: Annotate / Blame 引擎与全文件注解视图

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M9 — reference 缺口补齐
Priority: P0

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/annotate/GitAnnotationProvider.java`、`GitAnnotationService.kt`、`GitFileAnnotation*`
  - `reference/platform/vcs-impl/src/com/intellij/openapi/vcs/actions/AnnotateToggleAction.java`、`ShowAnnotateOperationsPopup.java`
  - `reference/platform/vcs-impl/src/com/intellij/openapi/vcs/annotate/{TextAnnotationPresentation,AnnotationTooltipBuilder}.java`
- **关联文件**：
  - `crates/jbgit-core/src/details.rs`（新增模型）、`crates/jbgit-core/src/lib.rs`
  - `crates/jbgit-engine/src/details/`（新增 `blame.rs`）、`crates/jbgit-engine/src/supervisor.rs`
  - `crates/jbgit-tui/src/app/{action,dispatch,modal_stack}.rs`、`crates/jbgit-tui/src/menus/{log,stage}.rs`、`crates/jbgit-tui/src/log_view/file_history.rs`
  - `crates/jbgit-tui/src/views/`（新增 `annotate.rs`）、`crates/jbgit-tui/src/syntax.rs`、`crates/jbgit-tui/src/keymap/catalog.rs`、i18n locales

---

## 1. 背景与目标

reference 中 Annotate 是使用频率最高的 VCS 能力之一（逐行显示最后修改的提交、作者、日期），`jbgit` 目前完全缺失（Spec 09 已明确将其列为后续独立工作）。本 Issue 建立可取消的 blame 加载管线和全屏注解视图。

## 2. 任务清单

- [ ] **领域模型（`jbgit-core`）**：
  - `BlameKey { path: String, rev: Option<Oid>, options: BlameOptions }`（`Hash + Eq`），`BlameOptions { ignore_whitespace: bool, detect_moves: bool }`。
  - `BlameLine { final_line: u32, orig_line: u32, oid: Oid, orig_path: String }`；`BlameCommit { oid, author: PersonStamp, summary: String, previous: Option<(Oid, String)> }`。
  - `BlameBundle { key, lines: Vec<BlameLine>, commits: HashMap<Oid, BlameCommit>, content: Arc<str>, uncommitted_oid_present: bool }`。
- [ ] **引擎（`jbgit-engine/src/details/blame.rs`）**：
  - 纯函数 `parse_blame_porcelain(raw: &[u8]) -> Result<Vec<BlameLine>, …>` 加 commit 元数据表，覆盖：同一提交多段、`boundary` 提交、`previous` 行、未提交行（全零 OID → 标注为"Not Committed Yet"）、非 UTF-8 内容（lossy 并保持行号对齐）。
  - `load_blame_sync(loc, key, token)`：`git -c core.quotepath=false blame --porcelain [-w] [-M -C] [<rev>] -- <path>`；`rev == None` 时注解工作区版本。执行前与解析中检查 `token.is_stale()`；> 4 MiB 或二进制文件返回 `EngineError::TooLarge` / `Binary`，由 TUI 显示占位。
  - `TaskRequest::LoadBlame` / `EngineEvent::BlameLoaded`，走独立 `RequestSlot`（消费者 `Annotate`），不写控制台。
- [ ] **TUI 视图（`ModalLayer::Annotate(Box<AnnotateState>)`）**：
  - 左侧注解栏：`短哈希 · 作者 · 相对日期`，宽度可配置；同一提交连续行只显示一次，按提交年龄从新到旧渐变着色（使用 `ThemePalette` 新增 `annotate_age_{0..4}` 令牌，三套主题齐备）。
  - 右侧：`syntect` 语法高亮正文（复用异步高亮槽位与 4 MiB / 2048 B 护栏），使用 `ScrollViewport` 夹紧，支持 `gg`/`G`/翻页/行号跳转。
  - 状态栏 / 浮动详情：当前行提交的完整摘要、作者邮箱、绝对时间。
  - 动作：`Enter` 在主 Log 中定位该提交（复用 `JumpToCommit`）；`d` 显示该行提交对此文件的 Diff；`p` **Annotate Previous Revision**（以 `previous` 的 `(oid, path)` 重新加载）；`w` / `m` 切换忽略空白 / 检测移动；`Esc` 返回原上下文且不丢失 Log 选择。
- [ ] **入口**：Log 变更文件菜单（注解该提交版本）、Stage 树菜单（注解工作区版本）、File History 视图（注解所选历史版本）、`CommandCatalog` 键位（默认 `<A-g>`；注意 `<A-a>` 已被 `toggle_amend` 占用，新键须在 `keymap` 测试中断言三套预设均无遮蔽冲突）。
- [ ] **配置**：`jbgit.toml` 新增 `[annotate] ignore_whitespace = false`、`detect_moves = false`、`gutter_width = 28`；更新 `docs/manual.md`。

## 3. 测试

- 解析器单元测试：多段提交、boundary、previous、未提交行、含空格 / 中文路径。
- 临时仓库集成测试：三次提交修改不同行 → 校验每行归属；`git mv` 后带 `-M -C` 仍追溯到原提交；工作区未提交修改行被标记为未提交。
- `TuiSession` 测试：打开 Annotate → `p` 回到上一版本 → `Enter` 跳转 Log 选中正确行 → `Esc` 恢复原模态栈。
