# Spec 13: 对齐 reference 基线的深度补齐与正确性加固（Phase 8）

- **目录**：`.scratch/13-reference-parity-depth-and-correctness/`
- **下属 Issues**（按建议执行顺序，P0 → P3）：
  - [`01-commit-sequencer-and-stage-correctness.md`](./issues/01-commit-sequencer-and-stage-correctness.md)（P0 · 修复 Merge/Sequencer 提交走 `git2` 丢父提交、Symlink/模式处理与 worktree hooks 路径缺陷）
  - [`02-conflict-semantics-crlf-and-delete-modify.md`](./issues/02-conflict-semantics-crlf-and-delete-modify.md)（P0 · 修复 Rebase 冲突 Ours/Theirs 反转、Delete/Modify `AcceptSide` 失败、CRLF 破坏与二进制冲突护栏）
  - [`03-log-non-contiguous-selection-and-compare.md`](./issues/03-log-non-contiguous-selection-and-compare.md)（P0 · Log 非连续多选解锁任意两提交对比 + Changes 树「仅显示受影响文件」）
  - [`04-diff-whitespace-horizontal-scroll-and-search.md`](./issues/04-diff-whitespace-horizontal-scroll-and-search.md)（P1 · Diff 忽略空白 `-b`/`-w`、水平滚动/软换行、Diff 内 `/` 搜索与二进制元数据）
  - [`05-pull-and-rebase-options-dialogs.md`](./issues/05-pull-and-rebase-options-dialogs.md)（P1 · Pull 选项对话框、标准 Rebase 对话框与交互式 Rebase `edit` / `fixup!` / 非连续 Squash）
  - [`06-branch-upstream-cleanup-and-cherry-stash-options.md`](./issues/06-branch-upstream-cleanup-and-cherry-stash-options.md)（P1 · Set/Unset Upstream、已合并分支清理、任意双分支对比、Cherry-Pick/Revert `-n`/`-m` 与 Stash `--keep-index`/`--index`）
  - [`07-commit-workflow-templates-merge-msg-and-push-polish.md`](./issues/07-commit-workflow-templates-merge-msg-and-push-polish.md)（P1，Blocked by `01` · `commit.template`、自动加载 `MERGE_MSG`/`SQUASH_MSG`、Commit & Push 直通 Push 对话框与 Push/Update 健壮性加固）
  - [`08-file-history-modal-actions-and-annotate-depth.md`](./issues/08-file-history-modal-actions-and-annotate-depth.md)（P1 · FileHistory 模态内右键菜单/分支筛选/分页与 Annotate 拆分 `-M`/`-C`、列定制及缓存）
  - [`09-true-three-way-merge-and-conflict-queue.md`](./issues/09-true-three-way-merge-and-conflict-queue.md)（P2，Blocked by `02` · 基于 `:1/:2/:3` 的真正 3-way 自动合并、Result 内联编辑与多文件冲突队列视图）
  - [`10-patch-options-and-tree-browser-ergonomics.md`](./issues/10-patch-options-and-tree-browser-ergonomics.md)（P2 · Apply Patch `-R`/`-p<n>`/部分勾选/Diff 预览、Create Patch 路径选择与 Tree Browser 搜索）
  - [`11-vcs-log-highlighters-graph-fidelity-and-reflog.md`](./issues/11-vcs-log-highlighters-graph-fidelity-and-reflog.md)（P2，Blocked by `03` · My Commits / Already Cherry-Picked 高亮器、LinearBek、No-Merges、表内 Speed Search 与 Reflog 视图）
  - [`12-incremental-log-refresh-and-pagination.md`](./issues/12-incremental-log-refresh-and-pagination.md)（P2 · 轻量增量头部刷新与突破 50k 渐进分页，不引入 SQLite）
  - [`13-clone-init-git-executable-and-crlf.md`](./issues/13-clone-init-git-executable-and-crlf.md)（P2 · Clone / Init 仓库生命周期、`[git] executable` 配置与 Windows CRLF 预警）
  - [`14-gpg-bisect-and-repo-ergonomics.md`](./issues/14-gpg-bisect-and-repo-ergonomics.md)（P2 · GPG 签名与 SSH Host-Key 交互、Bisect 控制、Submodule gitlink 暂存、`.worktreeinclude` 与灵活 `.gitignore`）
- **参考源码**：
  - `reference/plugins/git4idea/backend/src/{branch,rebase,pull,push,update,cherrypick,revert,stash,annotate,conflicts,merge,index,checkin,commit,crlf,config,commands,ignore,workingTrees}/`
  - `reference/platform/vcs-log/{graph,impl}/src/com/intellij/vcs/log/`
  - `reference/platform/vcs-impl/src/com/intellij/{vcs/commit,openapi/vcs/{changes/patch,merge,history,annotate}}/`
- **关联源码**：
  - `crates/jbgit-core/src/{conflict.rs,details.rs,filter.rs,patch.rs,repo.rs,stage.rs,stage_tracker.rs,tree.rs,branch_tree.rs}`、`crates/jbgit-core/src/git_ops/`、`crates/jbgit-core/src/graph/`
  - `crates/jbgit-engine/src/{askpass.rs,blob_reader.rs,cli.rs,ops.rs,probe.rs,stage.rs,supervisor.rs}`、`crates/jbgit-engine/src/{details,pool}/`
  - `crates/jbgit-tui/src/{branches_popup.rs,dialogs.rs,diff_session.rs,patch_view.rs,syntax.rs,tree_browser.rs}`、`crates/jbgit-tui/src/{app,graph,input,keymap,log_view,menus,stage_view,views}/`

---

## 1. 背景：Spec 12 落地后的二次差距评估（2026-11-24）

Spec 12（`#01`–`#14`，其中 `#11` 多根模型 `wontfix`）已全部合入主干。对比当前 `jbgit`（≈ 10.2 万行 Rust，含测试与 `pilot`）与 `reference/` 生产代码（`git4idea` ≈ 11.3 万行、`vcs-log` ≈ 5.2 万行、`vcs-impl` ≈ 13.3 万行，合计 ≈ 29.8 万行 Java/Kotlin）：

| 维度 | Spec 12 前 | 当前（Spec 12 后） | 本 Spec 落地后目标 |
| :--- | :---: | :---: | :---: |
| 日常单仓库高频工作流 | ≈ 85%–90% | ≈ 90%–95% | ≈ 98%（消除 P0 正确性瑕疵） |
| 终端可适用功能面（剔除 IDE 专属、Shelf/Changelists、多根同步） | ≈ 55%–60% | ≈ 75%–82% | ≈ 92%+ |
| `reference/` 全量功能面（含 IDE 专属） | ≈ 30% | ≈ 40%–45% | ≈ 50%+（其余为明确裁剪项） |

### 1.1 审计暴露出的三大剩余缺口类型
1. **P0 正确性边界缺陷（Issue 01–03）**：
   - 无 hook / 无 GPG 仓库在 Merge/Cherry-Pick/Revert 状态通过 Commit 面板提交时，`execute_commit_git2_sync` 生成单父提交并残留 `MERGE_HEAD`；
   - 冲突解决在 Rebase 状态下 Ours/Theirs 未翻转、Delete/Modify 冲突执行 `AcceptSide` 失败、`ConflictDocument` 剥离 `\r` 破坏 CRLF、二进制冲突被当 UTF-8 打开；
   - 符号链接按目标内容读取且悬空链接暂存时被删除；linked worktree 检查默认 hooks 时误用 `git_dir` 而非 `common_dir`；
   - Log 选区仅支持连续区间，导致 `Compare Versions` 无法对比任意两个不相邻提交。
2. **P1 高频工作流选项深度（Issue 04–08）**：
   - Diff 缺少忽略空白（`-b`/`-w`）、横向滚动/软换行与 `/` 内搜；
   - Pull 与标准 Rebase 缺少选项对话框，交互式 Rebase 缺少 `edit` 与独立 `fixup!`/`squash!`；
   - 分支缺少显式 Set/Unset Upstream、已合并分支批量清理与双分支对称对比；Cherry-Pick/Revert 缺 `-n`/`-m`，Stash 缺 `--keep-index`/`--index`；
   - Commit 面板未加载 `commit.template` 与全部 sequencer 消息、Commit & Push 未衔接 Push 对话框；Push Rejected 重试未套 `GitOp::Smart`；
   - FileHistory 模态缺少右键菜单与分支筛选，Annotate 未拆分 `-M`/`-C` 且无缓存。
3. **P2 专业能力、大仓扩展与生命周期（Issue 09–14）**：
   - 真正基于 `:1/:2/:3` 的 3-way 自动合并、内联编辑与多文件冲突队列；
   - Apply/Create Patch 的 `-R`、`-p<n>`、单文件勾选、Diff 预览与输出路径选择；
   - `My Commits` / `Already Cherry-Picked` 高亮器、`LinearBek`、No-Merges 筛选、Reflog 视图；
   - 轻量增量刷新与 >50k 渐进分页、Clone/Init、`[git] executable`、Windows CRLF 预警、GPG/SSH Host-Key 交互、Bisect 与 Submodule/Worktree/Ignore 增强。

---

## 2. 设计原则与约束

1. **P0 正确性先行**：Issue 01–03 不引入新框架负担，先消除会造成提交历史损坏（单父合并提交）、数据破坏（CRLF 洗毁、悬空符号链接误删）或误操作（Rebase 冲突方向相反）的缺陷。
2. **守住混合引擎与深模块边界（Spec 00 / 11 / 12）**：
   - 所有新增写操作与新参数一律通过纯声明式 `GitOp`（`jbgit-core/src/git_ops/ops.rs`）在 `jbgit-engine/src/ops.rs` 统一编译；
   - 新增只读查询（Reflog、3-way `:1` 祖先、`git cherry`、Blame 完整消息等）走独立 `RequestSlot` 单飞取消；
   - 新命令统一登记进 `CommandCatalog`，新模态进 `ModalStack`，滚动统一走自夹紧 `ScrollViewport`，i18n 中英双语齐备。
3. **继续明确不做（Out of Scope / wontfix）**：
   - Shelf 与 Changelists（保持 Git Staging Area + Stash 单一模型）；
   - 多仓库同步模型（Multi-Root Sync，保持单活动仓库 + `Ctrl+O` 切换）；
   - **SQLite 持久化索引**（2026-11-24 拍板 `wontfix`：采用无 SQLite 依赖的增量头部刷新 + 渐进分页，见 Issue 12）；
   - **In-Memory Rebase**（2026-11-24 拍板 `wontfix`：继续保持原生 `git rebase` 以保证 Hooks / GPG / `rebase.autosquash` 100% 兼容）；
   - IDE 专属能力（编辑器行状态 gutter、VFS/PSI、GitHub/GitLab 托管 PR 集成、IDE Commit Checks、Swing 设置与遥测）。

### 2.1 已拍板的决策（2026-11-24）

| 议题 | 决定 | 落点 |
| :--- | :--- | :--- |
| 大仓扩展性 | 轻量级：增量头部刷新 + 突破 50k 渐进分页；**不引入 SQLite（wontfix）** | Issue 12 |
| 仓库生命周期与环境 | **全部纳入**：Clone / Init、`[git] executable` 扩展探测、Windows CRLF 提交前检测 | Issue 13 |
| GPG / SSH / Bisect / 细节三件套 | **全部纳入**：单次 `-S`/`--no-gpg-sign`、签名 Tag `-s`、SSH host-key 确认、Bisect 控制、Submodule gitlink 暂存、`.worktreeinclude`、灵活 `.gitignore` | Issue 14 |
| In-Memory Rebase | **不做（wontfix）**：继续走原生 `git rebase` | §2 |
| 残留空目录 `13-terminal-native-ergonomics-and-pruning/` | 删除，由本 Spec 独占 `#13` 编号 | `.scratch/` |

---

## 3. 执行顺序与依赖

```text
P0  01 Commit/Stage 正确性 ──► 07 Commit 模板 / MERGE_MSG / Push 闭环 (P1)
P0  02 冲突语义与 CRLF 修复 ──► 09 真正 3-Way 自动合并与多文件冲突队列 (P2)
P0  03 Log 非连续多选与对比 ──► 11 高亮器 / LinearBek / Reflog (P2)

P1  04 Diff 空白/横滚/搜索        （独立，可并行）
P1  05 Pull 与 Rebase 选项弹窗     （独立，可并行）
P1  06 Upstream / 清理 / 选项补齐  （独立，可并行）
P1  08 FileHistory 菜单与 Annotate （独立，可并行）
P2  10 Patch 深度选项与 Tree 搜索  （独立，可并行）
P2  12 增量刷新与 >50k 渐进分页    （独立，可并行）
P2  13 Clone/Init / git路径 / CRLF （独立，可并行）
P2  14 GPG / Bisect / 细节三件套   （独立，可并行）
```

---

## 4. 验收总则

- 每个 Issue 均须通过 `cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings`；
- 每个 P0 修复必须附带**修复前会失败的回归测试**（真实临时 Git 仓库集成测试或纯函数/TUI 会话测试）；
- 新增快捷键须在 `keymap` 冲突测试中断言 `default` / `jetbrains` / `vim` 三套预设无遮蔽冲突；
- 新增配置项与操作同步更新 `en-US.toml`、`zh-CN.toml` 与 [`docs/manual.md`](../../docs/manual.md)。
