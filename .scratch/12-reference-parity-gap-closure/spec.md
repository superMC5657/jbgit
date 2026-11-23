# Spec 12: 对齐 reference 基线的功能缺口补齐路线图（Phase 7）

- **目录**：`.scratch/12-reference-parity-gap-closure/`
- **下属 Issues**（按建议执行顺序，P0 → P3）：
  - [`01-blame-annotate-engine-and-view.md`](./issues/01-blame-annotate-engine-and-view.md)（P0 · `git blame --porcelain` 引擎与全文件 Annotate 视图）
  - [`02-show-history-for-selection.md`](./issues/02-show-history-for-selection.md)（P1，Blocked by `01` · `git log -L` 行区间历史）
  - [`03-tag-management.md`](./issues/03-tag-management.md)（P0 · 删除本地/远端 Tag、推送单个 Tag、Tag 菜单）
  - [`04-push-dialog-commit-preview-and-targets.md`](./issues/04-push-dialog-commit-preview-and-targets.md)（P0 · 完整 Push 对话框：待推提交预览、目标编辑、Tag 推送模式、被拒后更新重试）
  - [`05-merge-dialog-options.md`](./issues/05-merge-dialog-options.md)（P1 · 合并选项：`--no-ff` / `--ff-only` / `--squash` / `--no-commit` / 自定义消息 / 策略）
  - [`06-update-project-flow-and-update-info.md`](./issues/06-update-project-flow-and-update-info.md)（P1 · Update Project：保存改动 → 拉取全部跟踪分支 → 恢复 → 更新摘要）
  - [`07-remote-management.md`](./issues/07-remote-management.md)（P1 · 远端增删改与 `RepoSnapshot.remotes` 模型）
  - [`08-askpass-credential-bridge.md`](./issues/08-askpass-credential-bridge.md)（P1，Blocked by `07` · HTTPS 用户名/密码与 SSH 口令的 `GIT_ASKPASS` 回调桥）
  - [`09-repository-session-rebind-and-switcher.md`](./issues/09-repository-session-rebind-and-switcher.md)（P1 · 进程内切换仓库与最近仓库切换器，Worktree/多仓库的地基）
  - [`10-working-trees.md`](./issues/10-working-trees.md)（P1，Blocked by `09` · Worktree 列表/创建/打开/锁定/删除/清理）
  - [`11-multi-root-model-decision.md`](./issues/11-multi-root-model-decision.md)（wontfix · 已决策：单活动仓库 + 快速切换，不做多根模型）
  - [`12-graph-collapse-linear-branches.md`](./issues/12-graph-collapse-linear-branches.md)（P2 · 提交图线性分支折叠/展开）
  - [`13-apply-patch-and-browse-at-revision.md`](./issues/13-apply-patch-and-browse-at-revision.md)（P2 · 应用 Patch 与按版本浏览仓库树）
  - [`14-repo-capabilities-shallow-submodule-lfs-signature.md`](./issues/14-repo-capabilities-shallow-submodule-lfs-signature.md)（P3 · 签名状态细分、Submodule 只读标注、LFS 指针占位、Unshallow，四项全做）
- **参考源码**：
  - `reference/plugins/git4idea/backend/src/{annotate,push,merge,update,remote,workingTrees,repo,commands,branch,actions/tag,actions/ref}/`
  - `reference/plugins/git4idea/shared/src/git4idea/push/GitPushTagMode.kt`
  - `reference/platform/vcs-impl/src/com/intellij/openapi/vcs/{annotate,actions,history/impl,update,changes/patch}/`
  - `reference/platform/vcs-log/graph/src/com/intellij/vcs/log/graph/collapsing/`、`reference/platform/vcs-log/impl/src/com/intellij/vcs/log/ui/actions/{Collapse,Expand}GraphAction.java`
- **关联源码**：
  - `crates/jbgit-core/src/{repo.rs,refs.rs,details.rs,branch_tree.rs}`、`crates/jbgit-core/src/git_ops/{ops,outcome}.rs`、`crates/jbgit-core/src/graph/{permanent,visible,print_elements}.rs`
  - `crates/jbgit-engine/src/{cli,ops,probe,supervisor,watcher}.rs`、`crates/jbgit-engine/src/details/{mod,file_history,commit_changes}.rs`、`crates/jbgit-engine/src/pool/{snapshot,worker_pool}.rs`
  - `crates/jbgit-tui/src/{session.rs,dialogs.rs,branches_popup.rs}`、`crates/jbgit-tui/src/app/{action,dispatch,git_workflows,modal_stack,types}.rs`
  - `crates/jbgit-tui/src/menus/{branch,log,stage}.rs`、`crates/jbgit-tui/src/keymap/catalog.rs`、`crates/jbgit-tui/src/log_view/file_history.rs`、`crates/jbgit-tui/src/views/{file_history,dialogs}.rs`
  - `crates/jbgit-tui/src/i18n/locales/{en-US,zh-CN}.toml`、`docs/manual.md`

---

## 1. 背景：与 reference 的差距评估（2026-11-23）

以 `reference/`（`git4idea` ≈ 15.6 万行、`vcs-log` ≈ 6 万行、`vcs-impl` ≈ 14.8 万行）为基线，对 `jbgit`（≈ 7 万行 Rust，含 ≈ 1 万行测试）按**功能面**逐项比对（剔除编辑器 gutter、VFS、托管平台集成、统计遥测、Swing 设置页等 IDE 专属能力后）：

| 维度 | 覆盖度估计 |
| :--- | :---: |
| 日常单仓库工作流（看历史 / 暂存 / 提交 / 分支 / 改写历史 / 冲突） | ≈ 85%–90% |
| `git4idea` + `vcs-log` 中终端可适用功能面 | ≈ 55%–60% |
| reference 全量功能面（含 IDE 专属） | ≈ 30% |

### 1.1 已对齐（本 Spec 不再涉及）
VCS Log 永久图 / BEK / 非交叉投影 / 双渲染后端；四维筛选；提交详情与 Diff 联动；文件 / 目录历史（`--follow`）；文件 / Hunk / 行级暂存与回滚；Amend / Sign-off / Hooks / 作者 / Uncommit / 提交并推送；分支全套操作与 Smart Checkout（`run_smart`）；Reword / Squash / Fixup / Drop / 交互式 Rebase（`RebaseTodoDialog`）；Cherry-pick / Revert / Reset（四模式）；三方冲突合并与 mergetool；Stash 全套；Patch 导出；控制台语义分级；**提交签名验证**（`verify_commit_signature_sync` 经 `git log --format=%G?` 异步验证，仅状态粒度偏粗，见 Issue 14）。

### 1.2 缺口清单（本 Spec 范围）

| 缺口 | 现状 | reference 依据 | Issue |
| :--- | :--- | :--- | :---: |
| Annotate / Blame | ❌ 无 | `git4idea/backend/src/annotate/`、`vcs-impl/.../vcs/annotate/`、`AnnotateToggleAction.java` | 01 |
| 按选区查看历史 | ❌ 无 | `vcs-impl/.../history/impl/VcsSelectionHistoryDialog.java` | 02 |
| Tag 删除 / 推送 | 🟡 仅创建与复制名称 | `GitDeleteTagOperation.java`、`actions/tag/GitPushTagsAction.kt`、`actions/ref/GitDeleteRefAction.kt` | 03 |
| Push 对话框 | 🟡 仅确认框（`prompt_push`） | `push/GitPushSupport.java`、`GitPushOperation.java`、`GitPushTargetPanel.java`、`GitPushTagMode.kt` | 04 |
| Merge 选项 | 🟡 固定 `merge --no-edit <rev>` | `merge/GitMergeDialog.kt`、`merge/GitMergeOption.kt` | 05 |
| Update Project | 🟡 仅 `Pull` / 单分支 `UpdateBranch` | `update/GitUpdateProcess.java`、`GitUpdateSession.kt`、`GitUpdateInfoAsLog.kt` | 06 |
| 远端管理 | ❌ 无（`default_remote()` 推断） | `remote/GitConfigureRemotesDialog.kt` | 07 |
| HTTPS / SSH 凭据交互 | ❌ `GIT_TERMINAL_PROMPT=0`，无 askpass | `commands/GitHttpAuthenticator.java`、`GitHttpAuthServiceImpl.java` | 08 |
| 进程内切换仓库 | ❌ 启动时一次性 `RepoLocation::discover` | `repo/GitRepositoryManager.java` | 09 |
| Working Trees | ❌ 仅 `probe.rs` 能识别 linked worktree | `workingTrees/`（≈ 4.9k 行） | 10 |
| 多仓库（multi-root） | ❌ 单仓库 | `repo/GitRepositoryManager.java`、`DvcsSyncSettings` | 11 |
| 线性分支折叠 | ❌ 无 | `vcs-log/graph/.../collapsing/CollapsedGraph.java` | 12 |
| 应用 Patch / 按版本浏览 | ❌ 无 | `changes/patch/ApplyPatchAction.java`、`log/GitBrowseRepoAtRevisionAction.kt` | 13 |
| Shallow / Submodule / LFS / 签名粒度 | ❌ / 🟡 | `actions/GitUnshallowRepositoryAction`、`log/GitCommitSignatureStatusProvider.kt` | 14 |

---

## 2. 设计原则与约束

1. **守住混合引擎边界（Spec 00 / 11）**：新增只读查询（blame、`log -L`、`worktree list`、`remote -v`、`ls-tree`）走引擎读路径并通过 `RequestSlot` 单飞取消；所有写操作与联网一律新增**纯声明式 `GitOp` 变体**，在 `jbgit-engine/src/ops.rs` 统一编译为 `CompiledStep`，禁止 TUI 层直发 `Effect::RunCli` 或拼接原生参数。
2. **命令统一进 `CommandCatalog`（Spec 11 Issue 01）**：每个新动作同时具备键位、菜单项、底栏提示、`?` 帮助元数据与 `Result<(), DisabledReason>` 可用性判定；新增 i18n 键须中英双语齐备（`i18n/mod.rs` 的占位符一致性测试会校验）。
3. **新模态全部进 `ModalStack`**：新视图（Annotate、Push、Worktrees、Repo Switcher、Tree Browser）以强类型 `ModalLayer` 变体承载，出栈即清理，滚动统一使用自夹紧 `ScrollViewport`。
4. **失败语义化**：新增的可识别失败（Tag 已存在、远端已存在、Worktree 分支已被占用、凭据被拒、`git apply` 冲突）在 `git_ops/outcome.rs` 扩展 `GitFailure` 分类，由 TUI 弹出可恢复对话框，而不是只留在控制台。
5. **可测性**：引擎解析器写纯函数单元测试 + 临时仓库集成测试（参照 `crates/jbgit-engine/tests/engine_integration.rs`）；TUI 交互走 `TuiSession` 确定性回合驱动；关键路径补一个 `tools/jbgit-pilot` 探索场景。

### 2.1 非目标（Out of Scope / wontfix）
- **Shelf 与 Changelists**（`vcs-impl/changes/shelf`、`changes/ui` 的 changelist 模型）：`jbgit` 以 Git 暂存区为唯一模型，Stash（含选中文件贮藏）承担"暂存搁置"职能，不引入第二套本地变更组织模型。
- **In-memory Rebase**（`git4idea/backend/src/inMemory`）：保持原生 `git rebase` 以 100% 兼容 Hooks / GPG / `rebase.autosquash`；仅当未来性能基准证明 CLI rebase 成为瓶颈时再立项。
- **Clone / Init**：`jbgit` 在已有仓库内启动；仓库获取交给 `git clone`。如 Issue 09 的切换器落地后有需求，再单独评估。
- **IDE 专属能力**：编辑器行状态 gutter、VFS 监听、GitHub/GitLab 托管集成、统计与遥测、Swing 设置页（由 TOML 配置体系替代）。
- **多仓库同步模型（multi-root）**：2026-11-23 决策为"单活动仓库 + 快速切换"（Issue 11 Answer）。`RepoSnapshot` / `GitOp` / Console 保持单仓库语义，不引入 `RepoId`。

### 2.2 已拍板的决策（2026-11-23）

| 议题 | 决定 | 落点 |
| :--- | :--- | :--- |
| 多仓库 | 单活动仓库 + 快速切换；完整多根 wontfix | Issue 09 / 11 |
| HTTPS / SSH 凭据 | 做 askpass 回调桥：仅当前用户可访问的命名管道 / Unix socket + 会话随机令牌，jbgit 不存储凭据 | Issue 08 |
| 零碎能力 | 签名细分、子模块只读标注、LFS 占位、Unshallow 四项全做 | Issue 14 |
| jetbrains 预设 `<C-t>` | 保持 `GitPull`；Update Project 统一用 `<A-u>` | Issue 06 |

---

## 3. 方案要点

### 3.1 Annotate 与选区历史（Issue 01、02）
- 引擎新增 `BlameKey { path, rev: Option<Oid> }` → `BlameBundle { lines: Vec<BlameLine>, commits: HashMap<Oid, BlameCommit> }`，解析 `git blame --porcelain -w? -M? -C?`（选项由 `[annotate]` 配置控制），走只读 CLI 与可取消槽位；> 4 MiB 文件沿用 Spec 10 安全护栏。
- TUI 新增 `ModalLayer::Annotate`：全文件语法高亮 + 左侧注解栏（作者 / 相对日期 / 短哈希，按提交年龄着色），支持跳转到 Log 提交、查看该行提交的 Diff、"Annotate Previous Revision"、`Esc` 无损返回。入口：Log 变更文件菜单、Stage 树菜单、File History 视图。
- 在 Annotate 视图中选中行区间后执行 `git log -L <start>,<end>:<path>`，结果复用 `FileHistoryState` 双分屏视图展示。

### 3.2 Tag、Push、Merge、Update（Issue 03–06）
- `GitOp` 新增 `DeleteTag { name }`、`DeleteRemoteTag { remote, name }`、`PushTag { remote, name }`；`Push` 增加 `tags: PushTagMode { None, Follow, All }` 与可编辑的 `remote_branch`。
- Push 对话框以每个待推分支为单位展示 `remote/branch..branch` 的提交列表（复用 Log 行渲染）、可编辑目标、`--force-with-lease` 开关及受保护分支（`[push] protected_branches`，默认 `main`/`master`）拦截；`GitFailure::PushRejected` 后提供"Merge / Rebase 后重试"。
- `GitOp::Merge` 扩展为 `Merge { rev, options: MergeOptions }`，对话框对应 `GitMergeOption` 的互斥规则（如 `--squash` 与 `--no-ff` 互斥）。
- `GitOp::UpdateProject { mode: UpdateMode { Merge, Rebase }, save_changes: bool }`：fetch → 当前分支 merge/rebase upstream → 其余跟踪分支仅快进 → 恢复改动；结束后基于更新前 / 后的 HEAD 与各分支 tip 生成 `UpdateInfo`，可一键打开按范围筛选的 Log。

### 3.3 远端与凭据（Issue 07、08）
- `RepoSnapshot` 新增 `remotes: Vec<RemoteInfo { name, fetch_url, push_url }>`，替换 `default_remote()` 的推断；`GitOp::{AddRemote, RenameRemote, RemoveRemote, SetRemoteUrl}`。
- 凭据桥：`jbgit` 二进制新增隐藏子命令 `jbgit askpass <prompt>`，作为 `GIT_ASKPASS` / `SSH_ASKPASS` 被 git 调用，通过仅当前用户可访问的命名管道 / Unix socket（会话级随机令牌鉴权）把提示转发给 TUI 的密码输入对话框，结果只写回子进程 stdout、**绝不进入控制台日志、草稿或任何持久化**。保留 `GIT_TERMINAL_PROMPT=0`。完整设计见 Issue 08 §2。

### 3.4 仓库会话与 Worktree / 多仓库（Issue 09–11）
- 把 `session.rs::init_engine_and_session` 拆成可重入的 `RepoSession::bind(location)`：在运行中拆除旧 `EngineSupervisor` / watcher / 工作台状态（先 `flush_draft_sync`），再绑定新仓库；最近仓库列表存在用户级配置目录。
- Worktree：`git worktree list --porcelain` 解析、`GitOp::{AddWorktree, RemoveWorktree, LockWorktree, UnlockWorktree, PruneWorktrees}`、"在 Worktree 中打开"（经 Issue 09 切换）；分支浮窗标注"已在其他 worktree 检出"并禁用 Checkout。
- 多仓库：已决策为"单活动仓库 + 快速切换"，由 Issue 09 的切换器承载，不实现 IDEA 式同步多根操作。

### 3.5 图谱与杂项（Issue 12–14）
- 移植 `CollapsedGraph` 思路：在 `VisibleGraph` 投影层把线性链折叠为单行虚边，不改永久图；支持单链折叠 / 展开与全部折叠 / 展开。
- `git apply --check` 预检 → `git apply --3way [--index]`；按版本浏览：`git ls-tree` 树视图 + 查看文件 / 与本地对比 / Get Version / Annotate。
- 浅克隆检测与 `fetch --unshallow`、Submodule 变更在 Stage 树中的只读标注、LFS 指针文件 Diff 占位、签名状态细分（`%G?` 的 `G/U/X/Y/R/E/B/N`）。

---

## 4. 执行顺序与依赖

```text
P0  01 Annotate ──► 02 选区历史 (P1)
P0  03 Tag 管理        P0 04 Push 对话框（相互独立；04 的 Tag 推送模式只用 --follow-tags / --tags）
P1  05 Merge 选项      P1 06 Update Project      P1 07 远端管理 ──► 08 凭据桥
P1  09 仓库会话重绑定 ──► 10 Worktrees
                       （11 多仓库：wontfix，由 09 的切换器承载）
P2  12 线性分支折叠    P2 13 应用 Patch / 按版本浏览
P3  14 签名细分 / Submodule / LFS / Unshallow
```

全部 Issue 落地后，预期"终端可适用功能面"覆盖度由 ≈ 55%–60% 提升至 ≈ 80%+（多根模型已明确不做，不计入分母）。

---

## 5. 验收总则
- 每个 Issue：`cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings` 通过；新 i18n 键中英齐备；`docs/manual.md` 补充对应快捷键与配置项。
- 写操作全部可在 `[4 Console]` 回放完整命令行（凭据内容除外）。
- 不回退 Spec 10 的性能指标：新增只读查询不得阻塞输入事件循环，且在 50k+ 提交仓库中首屏响应不劣化。
