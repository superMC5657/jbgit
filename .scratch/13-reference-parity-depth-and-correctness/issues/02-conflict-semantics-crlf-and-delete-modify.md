# 02: 冲突语义反转（Rebase）、Delete/Modify 采纳、CRLF 保护与二进制护栏

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M10 — reference 深度对齐与正确性加固
Priority: P0

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/merge/GitMergeProvider.java`（`:60-106` `isReverseRoot`、`:126` 二进制检测）
  - `reference/plugins/git4idea/backend/src/merge/GitMergeUtil.java`（`:414-471` Delete/Modify 走 `git rm`、反转语义）
  - `reference/plugins/git4idea/backend/src/conflicts/GitMergeHandler.java`（`:56` 子模块/目录冲突拦截）
- **关联文件**：
  - `crates/jbgit-core/src/conflict.rs`（`ConflictDocument::parse`、`render_resolved`）
  - `crates/jbgit-core/src/git_ops/ops.rs`（`GitOp::AcceptSide`）、`crates/jbgit-engine/src/ops.rs`（`:410-424`）
  - `crates/jbgit-engine/src/pool/snapshot.rs`（`load_conflict_document_sync`）、`crates/jbgit-engine/src/supervisor.rs`（`SaveConflictResolution`）
  - `crates/jbgit-tui/src/app/git_workflows.rs`（`:250-314`）、`crates/jbgit-tui/src/views/conflict_merge.rs`

---

## 1. 背景与目标

`jbgit` 已有三栏冲突合并弹窗与 Stage 快捷 Accept 操作，但对照 `git4idea` 存在 4 个高危正确性缺陷：
1. **Rebase 期间 Ours / Theirs 语义相反**：`git rebase` 时 stage 2（`HEAD`）是上游目标分支，stage 3 是当前正在重放的本地提交，未做语义翻转会导致用户点“Accept Yours”实则丢弃自己的改动；
2. **Delete/Modify 与 Both-Deleted 冲突报错**：`GitOp::AcceptSide` 写死 `git checkout --ours/--theirs` + `git add`，当被选侧删除了文件时直接失败；
3. **CRLF 破坏**：`ConflictDocument::parse` 剥离 `\r`、`render_resolved` 固定用 `\n` 写盘，Windows 仓库解决冲突后整文件被洗成 LF；
4. **二进制 / 子模块冲突与滚屏**：二进制冲突直接 `from_utf8_lossy` 以乱码打开三栏文本合并；切换 `selected_block` 时视口 `scroll` 不跟随，大文件切块后看不到选中块。

## 2. 任务清单

- [ ] **Rebase / Unstash 冲突方向翻转**：
  - 在 `jbgit-core` 定义纯函数 `is_reverse_conflict_state(state: &RepositoryState) -> bool`（`Rebase` / `RebaseInteractive` / `RebaseMerge` 返回 `true`）；
  - `GitOp::AcceptSide` 编译与 `load_conflict_document_sync` 统一按 `is_reverse_conflict_state` 映射 `--ours`/`--theirs` 及 index `:2`/`:3`，确保界面上的 **Yours（你的提交）** 与 **Theirs（上游/对方）** 在 Merge 与 Rebase 下语义一致，并在三栏标题栏显示实际侧名称（如 `Yours (replayed)` / `Theirs (onto)`）。
- [ ] **Delete/Modify 与 Both-Deleted 冲突的正确采纳**：
  - `GitOp::AcceptSide` 执行前（在引擎 `ops.rs` 中）查询目标路径的 `IndexConflict` 状态（`:1`/`:2`/`:3` 存在性）：
    - 被采纳侧存在 blob → `git checkout --ours/--theirs -- <path>` + `git add -- <path>`；
    - 被采纳侧为删除（该 stage 为 `None`）→ `git rm -f -- <path>`；
  - 在三栏冲突弹窗中对 Delete/Modify 冲突提供明确的「保留文件」/「删除文件」决策路径，保存时支持删除路径并暂存删除。
- [ ] **行尾符（CRLF / LF）与末尾换行符无损保留**：
  - `ConflictDocument` 记录原始 `line_ending: LineEnding`（`CrLf` / `Lf`）与 `has_trailing_newline: bool`（基于原文统计，兼容混合换行），`render_resolved()` 按原始行尾还原写盘，不再把 CRLF 文件洗成 LF。
- [ ] **二进制与子模块冲突护栏 + 视口跟随**：
  - `load_conflict_document_sync` 检测 `:2`/`:3` 或工作区内容是否为二进制（复用 8 KiB NUL 嗅探）或子模块 gitlink（`0o160000`）；若是，不打开乱码文本三栏视图，而是弹出专用选择对话框（展示两侧大小/OID，直接选择 Accept Yours / Accept Theirs / 外部 mergetool）；
  - `Action::ConflictSelectBlock`（`n`/`p` 或点击块）自动调整 `ConflictMergeState::scroll` 使选中冲突块首行落入可见区；并在后台快照刷新发现当前路径冲突已被外部解决时给出失效提示。

## 3. 测试

- **引擎集成测试（真实临时仓库，修复前必挂）**：
  - 构造 Rebase 冲突 → 验证 `AcceptSide { ours: true }` 保留的是**被重放的本地提交内容**而非上游内容；
  - 构造 `Deleted by Them` 与 `Deleted by Us` 冲突 → 验证采纳删除侧执行 `git rm` 后冲突清零且文件已从工作区与索引删除；
  - 构造 CRLF 文件合并冲突 → 经 `ConflictDocument` 解决并保存 → 验证输出文件仍为 `\r\n`；
  - 构造二进制文件冲突（含 `\0` 字节）→ 验证加载返回二进制冲突描述符而非乱码文本。
- **TUI 会话测试**：包含 100+ 行的多块冲突文件，按 `n`/`p` 切换冲突块后断言 `scroll` 视口包含当前块。
