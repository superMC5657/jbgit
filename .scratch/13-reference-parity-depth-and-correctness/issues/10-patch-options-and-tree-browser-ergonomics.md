# 10: Apply/Create Patch 深度选项与 Tree Browser 交互增强

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M10 — reference 深度对齐与正确性加固
Priority: P2

- **参考源码**：
  - `reference/platform/vcs-impl/src/com/intellij/openapi/vcs/changes/patch/{ApplyPatchDifferentiatedDialog.java,CreatePatchConfigurationPanel.kt,Strippable.java,MatchPatchPaths.java,PatchDiffViewer.kt}`
  - `reference/plugins/git4idea/backend/src/index/actions/GitStageCreatePatchActionProvider.kt`
  - `reference/platform/vcs-impl/src/com/intellij/openapi/vcs/impl/RepositoryBrowserPanel.kt`
- **关联文件**：
  - `crates/jbgit-core/src/{patch.rs,tree.rs,git_ops/ops.rs}`
  - `crates/jbgit-engine/src/{ops.rs,details/patch.rs,details/tree.rs}`
  - `crates/jbgit-tui/src/{patch_view.rs,tree_browser.rs,menus/stage.rs,menus/log.rs,views/dialogs.rs}`

---

## 1. 背景与目标

Spec 12 Issue 13 落地了基础版 Apply Patch 与 Browse Repository at Revision，但在实际补丁交换与历史树浏览中仍缺少关键实用选项：
- Apply Patch 无法反向应用（`-R`）、无法调整路径剥离层级（`-p<n>`）或目标子目录（`--directory`）、无法只勾选补丁中的部分文件应用，也无法在应用前查看单文件实际 Diff；
- Create Patch 从 Commit 导出写死 `-o .` 污染仓库根目录，从 Stage 只能复制到剪贴板不能保存为文件，且跨 Staged+Unstaged 混合多选或含未追踪文件时会丢失改动；
- Tree Browser 在大型仓库中缺少 `/` 模糊查找、无法对选中子目录局部 Compare with Local，且 `GetVersion` 用 `git checkout` 会无声覆盖暂存区。

## 2. 任务清单

- [ ] **Apply Patch 深度选项（`patch.rs` / `details/patch.rs` / `patch_view.rs`）**：
  - 扩展 `ApplyOptions { mode: ApplyMode, reverse: bool /* -R */, strip: Option<u8> /* -p<n> */, directory: Option<String> /* --directory=<dir> */, include_paths: Vec<String> /* --include=... */ }`；
  - `ApplyPatch` 模态文件列表升级为可勾选列表（`Space` 切换单文件、`a` 全选/反选），按 `d`/`Enter` 在右侧/弹窗预览该补丁文件的实际 diff 内容（从 patch 文本中解析对应 file section 并走 `syntect` 高亮）；
  - 选项变更（`r` 切换 Reverse、`+`/`-` 调整 `-p` 剥离层数、`D` 设置目录前缀、勾选文件变动）自动触发防抖 `--check --numstat` 重新预检；
  - 对无 `index <sha>..<sha>` 头的普通 unified patch 在 `--3way` 预检失败时给出明确降级提示（建议切换 `WorkingTree` 或调整 `-p`）。
- [ ] **Create Patch 配置弹窗与 Stage 完整支持**：
  - Commit 与 Stage 的「Create Patch File…」统一弹出配置框：目标文件/目录路径（默认用户上次目录或 `<repo>/.git/../` 外置路径，禁止无确认直接落盘到工作区根 `.` 污染 `git status`）、**Reverse Patch (`-R`)** 开关、输出去向（文件 / 剪贴板）；
  - 修复 Stage 侧 `PatchSource::StagePaths`：支持同时选中 `Staged` + `Unstaged` 混合生成完整补丁，并将选中的 `Untracked` 文件经 `git diff --no-index -- /dev/null <path>`（或纯 Rust 生成标准 unified new-file hunk）纳入补丁输出。
- [ ] **Browse Repository at Revision（`tree_browser.rs`）增强**：
  - 新增 `/` 模糊搜索（调用 `git ls-tree -r --name-only <oid>` 异步可取消索引当前版本全部路径并用 `nucleo-matcher` 过滤，选中即自动展开并定位到该文件）；
  - 对目录节点执行 `Compare with Local` 时改为限定该子目录路径（而非全仓库）；
  - 新增 `Copy File Content` 与 `Export to File…` 预览动作，明确标注符号链接目标（`symlink → <target>`）；
  - `Get Version` 确认框支持选择「仅恢复到工作区（`git restore --source=<oid> --worktree -- <paths>`）」或「同时更新暂存区（`--staged --worktree`）」，并支持多选文件批量恢复。

## 3. 测试

- **单元测试**：`ApplyOptions`（`-R`/`-p2`/`--directory`/`--include`）参数编译与单文件 patch 切片提取测试；未追踪文件补丁生成测试。
- **引擎集成测试**：生成子目录前缀 patch → 用 `-p2 --directory=sub` 预检并仅勾选 2 文件中的 1 个成功应用 → 用 `-R` 反向撤回；Stage 选中 1 个 Staged + 1 个 Untracked 文件导出补丁可干净应用。
- **TUI 会话测试**：Tree Browser 按 `/` 搜深层嵌套文件并回车定位预览；Apply Patch 模态取消勾选一个文件并预览另一个文件的 diff。
