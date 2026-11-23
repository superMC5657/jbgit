# 13: 应用 Patch 与按版本浏览仓库

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M9 — reference 缺口补齐
Priority: P2

- **参考源码**：
  - `reference/platform/vcs-impl/src/com/intellij/openapi/vcs/changes/patch/{ApplyPatchAction,ApplyPatchDefaultExecutor,ApplyPatchDifferentiatedDialog}.java`
  - `reference/plugins/git4idea/backend/src/log/GitBrowseRepoAtRevisionAction.kt`、`GitDirectoryVirtualFile.kt`
- **关联文件**：
  - `crates/jbgit-core/src/git_ops/ops.rs`（已有 `CreatePatchFiles` / `PatchToClipboard` / `CheckoutPaths`）
  - `crates/jbgit-engine/src/{ops.rs,blob_reader.rs}`
  - `crates/jbgit-tui/src/menus/{log,stage}.rs`、`crates/jbgit-tui/src/views/`

---

## 1. 背景与目标

`jbgit` 能导出 Patch 却不能应用；也无法查看某个提交时刻的完整仓库文件树（只能看该提交的变更文件）。

## 2. 任务清单

### 2.1 应用 Patch
- [x] `GitOp::ApplyPatch { source: PatchInput { File(PathBuf), Clipboard }, mode: ApplyMode { WorkingTree, Index, ThreeWay } }` → `git apply [--index | --3way] [--whitespace=nowarn] -`（剪贴板内容走 stdin）。
- [x] 预检：先 `git apply --check --numstat` 只读运行，对话框展示将受影响的文件与增删行数；预检失败时显示首条错误并建议 `ThreeWay`。
- [x] `ThreeWay` 产生冲突时进入现有冲突横幅与三栏合并流程。
- [x] 入口：Stage 菜单 "Apply Patch…"（路径输入支持相对仓库根和 Tab 补全）、"Apply Patch from Clipboard"。

### 2.2 按版本浏览（Browse Repository at Revision）
- [x] 只读查询 `load_tree_sync(loc, oid, dir) -> Vec<TreeEntry { name, kind: Blob|Tree|Submodule, mode, size }>`（`git2` 直接读树，按目录懒加载）。
- [x] `ModalLayer::TreeBrowser`：目录树 + 右侧文件内容预览（语法高亮，复用 Spec 10 护栏）；动作：Compare with Local、Get Version（`CheckoutPaths`）、Annotate（Issue 01）、Show History（Spec 09）、Copy Path。
- [x] 入口：Log 提交菜单 "Browse Repository at Revision"、Go to Revision 弹窗的二级动作。

## 3. 测试

- 集成：导出 Patch → 回退 → 应用成功；冲突 Patch 在 `ThreeWay` 下进入冲突状态；`--check` 失败不修改工作区。
- 树浏览：子目录懒加载、子模块条目只读显示、二进制文件显示占位。

## Comments

- 2026-11-24 实现说明：
  - 核心：`jbgit-core::patch`（`PatchInput { File, Clipboard }`、`ApplyMode { WorkingTree, Index, ThreeWay }`、`PatchCheckKey` / `PatchCheck` / `PatchFileStat`，`parse_apply_check` 解析 `--numstat`（`-` = 二进制、C 风格引号路径）与 `--3way` 的 `Applied patch to 'p' with conflicts.`）；`GitOp::ApplyPatch { source, mode }`；`GitFailure::{PatchDoesNotApply { detail }, PatchConflicts { paths }}`。`jbgit-core::tree`（`TreeEntry { name, path, kind: Blob|Tree|Submodule, mode, size, oid }`、`TreeKey`、`TreeListing`（目录在前、名称不区分大小写排序）、`TreeBlobKey` / `TreeBlob` / `TreeBlobContent { Text, Binary, TooLarge, Missing }`）。新请求槽 `PatchCheck` / `TreeBrowser` / `TreeBlob`。
  - 引擎：`git apply [--check --numstat] [--index|--3way] --whitespace=nowarn (-- <file> | -)`；预检为只读查询（`TaskRequest::CheckPatch` → `EngineEvent::PatchChecked`，失败时给出首条 `error:` 行，不产生控制台条目），应用走 `Runner`（剪贴板经 stdin）。`clipboard.rs`：OSC 52 不可读回，改为调用 `Get-Clipboard` / `pbpaste` / `wl-paste` / `xclip` / `xsel`，统一 CRLF 折叠为 LF，`JBGIT_CLIPBOARD_FILE` 作为测试 / 无图形环境的接缝。树浏览直接经 git2 按目录懒加载（`TaskRequest::LoadTree` → `TreeLoaded` / `TreeLoadFailed`），文件预览复用 `blob_reader` 的 4 MiB / 二进制护栏（`LoadTreeBlob` → `TreeBlobLoaded`）。
  - TUI：`ModalLayer::ApplyPatch`（`patch_view.rs`）：来源、三种模式单选（`1/2/3`、`Tab`、点击）、文件 +/- 列表与"冲突"标记、失败时显示错误并提示三方合并、`Apply` 仅在预检通过时可用；`PatchConflicts` 后通知栏"解决冲突"切到 `[1 Commit]` 并（单文件时）打开三栏合并。`Apply Patch…` 路径输入：`InputValidation::ExistingFile`（相对仓库根，`Tab` 补全公共前缀 / 目录加 `/`）。入口：Stage 菜单两项、`<A-p>` / `<A-S-p>`（`apply_patch` / `apply_patch_clipboard`）。
  - `ModalLayer::TreeBrowser`（`tree_browser.rs`）：一次只有一个目录请求在途（排队，避免单飞槽互相取消）；左树右预览（语法高亮走新的 `SyntaxJobKey::TreeBlob` 异步作业）；`c` 与本地对比（文件 → `CompareFileWithLocal`，目录 → `CompareWithLocal`）、`g` 获取此版本（确认后 `CheckoutPaths`）、`a` Annotate（叠在浏览器之上，`Esc` 返回）、`h` 历史、`y` 复制路径；子模块条目只读。入口：Log 提交菜单 `Browse Repository at Revision`、`<A-t>`（`browse_revision`）、Go to Revision 弹窗 `<Alt-Enter>`。
  - 测试：核心解析 / 失败分类 / 标签单测；引擎集成（导出 Patch → 回退 → 预检列出文件且不改工作区 → `--index` 应用 → 冲突提交后预检报错不改文件 → 三方预检标出冲突 → 三方应用进入 unmerged 状态并分类为 `PatchConflicts` → 缺失文件；树：根目录排序、子目录懒加载、子模块 gitlink、文本 / 二进制 / 缺失预览、未知目录失败）；独立测试二进制验证剪贴板 stdin 路径（含 CRLF）；TUI（剪贴板预检失败 → 切三方 → 应用 → 冲突通知；路径补全与校验；树浏览懒加载、预览、Annotate 往返、获取此版本、二进制 / 子模块占位、复制、对比；Go to Revision `Alt+Enter`）。
  - 未验证：真实系统剪贴板读取（各平台工具）未在自动化中覆盖，仅通过 `JBGIT_CLIPBOARD_FILE` 接缝测试。
