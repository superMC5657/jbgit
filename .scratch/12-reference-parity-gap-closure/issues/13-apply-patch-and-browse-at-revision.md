# 13: 应用 Patch 与按版本浏览仓库

Status: ready-for-agent
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
- [ ] `GitOp::ApplyPatch { source: PatchInput { File(PathBuf), Clipboard }, mode: ApplyMode { WorkingTree, Index, ThreeWay } }` → `git apply [--index | --3way] [--whitespace=nowarn] -`（剪贴板内容走 stdin）。
- [ ] 预检：先 `git apply --check --numstat` 只读运行，对话框展示将受影响的文件与增删行数；预检失败时显示首条错误并建议 `ThreeWay`。
- [ ] `ThreeWay` 产生冲突时进入现有冲突横幅与三栏合并流程。
- [ ] 入口：Stage 菜单 "Apply Patch…"（路径输入支持相对仓库根和 Tab 补全）、"Apply Patch from Clipboard"。

### 2.2 按版本浏览（Browse Repository at Revision）
- [ ] 只读查询 `load_tree_sync(loc, oid, dir) -> Vec<TreeEntry { name, kind: Blob|Tree|Submodule, mode, size }>`（`git2` 直接读树，按目录懒加载）。
- [ ] `ModalLayer::TreeBrowser`：目录树 + 右侧文件内容预览（语法高亮，复用 Spec 10 护栏）；动作：Compare with Local、Get Version（`CheckoutPaths`）、Annotate（Issue 01）、Show History（Spec 09）、Copy Path。
- [ ] 入口：Log 提交菜单 "Browse Repository at Revision"、Go to Revision 弹窗的二级动作。

## 3. 测试

- 集成：导出 Patch → 回退 → 应用成功；冲突 Patch 在 `ThreeWay` 下进入冲突状态；`--check` 失败不修改工作区。
- 树浏览：子目录懒加载、子模块条目只读显示、二进制文件显示占位。
