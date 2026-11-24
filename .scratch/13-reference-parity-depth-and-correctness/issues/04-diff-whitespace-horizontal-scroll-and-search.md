# 04: Diff 忽略空白、水平滚动/软换行、内联搜索与二进制元数据

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M10 — reference 深度对齐与正确性加固
Priority: P1

- **参考源码**：
  - `reference/platform/vcs-impl/src/com/intellij/openapi/vcs/changes/actions/diff/`
  - `reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/table/VcsLogGraphTable.java`（`:691-711` `CopyProvider`）
- **关联文件**：
  - `crates/jbgit-core/src/details.rs`（`FileDiffKey`、`compute_diff_blocks`、`side_by_side_rows`、`word_diff`）
  - `crates/jbgit-engine/src/details/file_diff.rs`、`crates/jbgit-engine/src/stage.rs`（`StageDiffKey`）
  - `crates/jbgit-tui/src/{diff_session.rs,syntax.rs}`、`crates/jbgit-tui/src/views/{commit_diff.rs,log_details/diff_view.rs}`
  - `crates/jbgit-tui/src/menus/log.rs`、`crates/jbgit-tui/src/keymap/catalog.rs`

---

## 1. 背景与目标

Diff 视图是终端 Git 客户端停留时间最长的面板。当前 `jbgit` 的 Diff 仅支持上下文行数和 Unified/Split 切换，存在 4 个高频痛点：
1. 无法忽略空白变动（`-b` / `-w`），代码缩进调整后满屏红绿；
2. 超宽代码行直接截断加 `…`，尤其在 Split 双栏下完全看不到行尾；
3. 无法在 Diff 内用 `/` 搜索关键字并按 `n`/`N` 跳转命中行；
4. 二进制文件仅打印静态 `"Binary file"`，未利用已有字节数与 OID 展示体积增量；此外 Tab 展开用固定空格数而非 tab-stop 对齐，Log 表也缺少复制提交消息与行摘要的动作。

## 2. 任务清单

- [ ] **空白忽略模式（`WhitespaceMode`）**：
  - 在 `jbgit-core::details` 新增 `WhitespaceMode { Default, IgnoreChange /* -b */, IgnoreAll /* -w */ }`，纳入 `FileDiffKey`、`StageDiffKey` 与 `DiffCacheStore` 缓存键；
  - `compute_diff_blocks` 与三文档行比对按 `WhitespaceMode` 对比较行做规范化（显示仍保留原始文本，避免局部暂存丢空白；`IgnoreChange` / `IgnoreAll` 模式下禁用或提示局部暂存按原始行执行）；
  - 在 Diff 工具栏与 `Diff` 键位作用域新增切换快捷键（如 `b` 循环 `Default → -b → -w`，默认写入 `[diff] whitespace` 配置）。
- [ ] **水平滚动、软换行与真 Tab-Stop 对齐**：
  - `DiffSession` 增加 `horiz_scroll: u16` 与 `soft_wrap: bool`（由 `[diff] soft_wrap` 初始化，`W` 切换）；
  - 未开启软换行时，支持 `h`/`l`、`Left`/`Right`（或 `Shift+Left/Right`）按列水平滚动并在标题栏显示 `Col +N` 指示；开启软换行时按显示宽度（CJK 宽字符安全）折行并同步更新 `ScrollViewport` 行数；
  - `syntax.rs` 修正 Tab 展开为按列对齐到下一个 `tab_size` 倍数；修复 `side_by_side_rows` 在块内插行时词级 diff 错位的问题（按相似度对削除/新增行做贪心配对而非严格 1-to-1 下标配对）。
- [ ] **Diff 内搜索（`/`、`n`、`N`）**：
  - 在所有复用 `DiffSession` 的 Diff 面板（Log Diff、Stage Diff、File History Diff、Stash Diff）支持 `/` 唤起底栏内联搜索框（忽略大小写，支持增量匹配计数 `[3/12]`），高亮命中文本片段，`n`/`N` 跳转下一/上一命中行并驱动 `ScrollViewport` 居中滚动，`Esc` 退出搜索。
- [ ] **二进制文件元数据卡片 & Log 复制增强**：
  - 二进制 Diff（`is_binary == true`）展示 Old/New 字节大小、人类可读体积变化（如 `12.4 KiB → 14.8 KiB (+2.4 KiB)`）及 Old/New 短 blob OID；
  - Log 菜单与 `CommandCatalog` 新增 **Copy Commit Message**（完整消息体）与 **Copy Row Summary**（`hash subject — author (date)`，支持多选逐行复制）。

## 3. 测试

- **单元测试**：`WhitespaceMode::{IgnoreChange, IgnoreAll}` 对缩进/尾随空格差异的块合并测试；Tab-stop 对齐与 CJK 字符水平偏移切片测试；词级 diff 块内错位重排配对测试；Diff 搜索匹配定位测试。
- **TUI 会话测试**：
  - 打开超宽行文件的 Split Diff → 按 `l` 水平滚动验证行尾字符可见 → 切 `W` 软换行验证无截断；
  - 按 `/` 搜索关键词 → `n`/`N` 跳转命中并夹紧滚屏；
  - 切换 `WhitespaceMode` 验证纯缩进改动变为无差异。
