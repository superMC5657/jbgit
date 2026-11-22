# 05: 声明式 `GitOp`、统一 CLI 子进程接缝与保留原始 Tab 的 Hunk 构建器

Status: resolved
Spec: [../spec.md](../spec.md)
Blocked by: 02
Milestone: M8 — 深模块架构治理与测试面收敛 (P1)

- **关联文件**：
  - `crates/jbgit-core/src/git_ops/{mod,ops,outcome,rebase,refs}.rs`
  - `crates/jbgit-core/src/{details,stage_tracker}.rs`
  - `crates/jbgit-engine/src/{ops,cli,stage,supervisor}.rs`
  - `crates/jbgit-engine/src/details/{mod,commit_changes,file_diff,file_history,path_matches}.rs`
  - `crates/jbgit-engine/tests/git_ops_integration.rs`
  - `crates/jbgit-tui/src/menus/{log,stage}.rs`
  - `crates/jbgit-tui/src/app_stage/{mod,events}.rs`
  - `crates/jbgit-tui/src/syntax.rs`

---

## 1. 背景与目标

在 `jbgit-core` 与 `jbgit-engine` 的职责交界处，当前存在两组因模块职责切分不当引发的泄漏与生产缺陷：

1. **`GitOp` CLI 编译横跨三层且多处旁路**：
   - `jbgit-core::GitOp::simple_steps`（`crates/jbgit-core/src/git_ops/ops.rs:379-627`）在纯领域库内拼接了约 25 种原生 `git` CLI 参数，唯独当 `CheckoutPaths` 路径数 `> 32` 时返回 `None` 退回 `jbgit-engine/src/ops.rs:209-219` 使用 `prepare_pathspec_args`（`--pathspec-from-file`）；而同样接收路径列表的 `AcceptSide`、`IntentToAdd` 与 `StashPush` 留在 `jbgit-core` 中直接拼命令行参数，在 Windows 大批量文件操作下存在超长命令行风险。
   - `GitOp::PatchToClipboard { args: Vec<String> }` 把原生 `git diff` 参数拼接泄漏到了 `jbgit-tui/src/menus/*.rs`。
   - TUI 中的 `StashAll`（`app_stage/mod.rs:266`）与「提交并推送」（`app_stage/events.rs:249`）绕过 `GitOp` 直接发送裸 `Effect::RunCli`，丢失了 `GitOpOutcome` 的结构化错误恢复能力（如 `PushRejected` 触发 `--force-with-lease` 引导对话框）。
   - `cli.rs` 与 `details/mod.rs:39-55` 重复实现 `git` 二进制路径发现、Windows `CREATE_NO_WINDOW` 标志与非交互环境变量设置；且 `run_git_op`（`ops.rs:89-99`）与 `spawn_execute_commit`（`supervisor.rs:678-742`）静默丢弃了 `GitCliOutput::index_lock_conflict`，未像 `spawn_run_git_cli` 那样发出 `EngineEvent::IndexLockDetected`。
2. **Hunk 构建与体积/二进制读取重复，且提前硬编码替换 `'\t'` 导致 `tab_size` 配置失效**：
   - `jbgit-engine/src/details/file_diff.rs::build_imara_hunks` 与 `jbgit-core/src/stage_tracker.rs::build_interactive_hunks` 重复实现了 `imara-diff` Histogram 差分、`context_len * 2` Hunk 合并、`20,000` 行截断预算、`@@ -a,b +c,d @@` 头部格式化及 `word_diff` 预算调用；`file_diff.rs::ReadBytes` 与 `stage.rs::StageReadSide` 重复实现 4 MiB ODB/工作区预检，但二进制 NUL 检测范围不一致（`file_diff.rs:100` 扫全文 4 MiB vs `stage.rs:238` 扫前 8 KiB）。
   - **真实缺陷**：`file_diff.rs:156`、`stage_tracker.rs:121` 与 `stage.rs:465` 全部将 `'\t'` 硬编码替换为 4 个空格 `.replace('\t', "    ")`，导致下游 `syntax.rs:243`（`render_diff_line_spans_with_tab`）永远收不到制表符，用户在 `jbgit.toml` 配置的 `[diff] tab_size` 对真实仓库文件完全不生效。

本 Issue 收敛出纯声明式的 `GitOp` 领域模型、统一的 `jbgit-engine` CLI 编译与子进程适配器接缝，以及单一保留原始 `'\t'` 的 Hunk 构建与读取管线。

---

## 2. 核心任务清单

- [ ] **纯化 `jbgit-core::GitOp` 并消除裸 CLI 旁路（`jbgit-core/src/git_ops/ops.rs`, `jbgit-tui/src/menus/*`, `jbgit-tui/src/app_stage/*`）**：
  - 将 `GitOp::PatchToClipboard { args: Vec<String> }` 替换为纯领域枚举/结构（如 `PatchSource::CommitSingle { oid, parent_idx, path }` / `CommitRange { base, target, path }` / `StageSelection { ... }`），TUI 菜单不再拼接任何 `git diff` 命令行标志。
  - 将 TUI `StashAll`（`app_stage/mod.rs:266`）与提交完成后的自动推送（`app_stage/events.rs:249`）统一改走类型化 `GitOp`（如 `GitOp::StashPush` 与 `GitOp::Push`），确保推送被拒、钩子报错与锁冲突均走统一的 `GitOpOutcome` 处理闭环。
  - 将 `GitOp::simple_steps` 从 `jbgit-core` 迁移至 `jbgit-engine/src/ops.rs`，使 `jbgit-core` 仅表达领域意图与结果解析规则。
- [ ] **在 `jbgit-engine` 构建统一的 Git 命令编译与子进程执行深模块（`ops.rs`, `cli.rs`, `details/mod.rs`, `stage.rs`, `supervisor.rs`）**：
  - 在 `jbgit-engine/src/ops.rs` 统一所有接收路径集合的操作（`CheckoutPaths`、`AcceptSide`、`IntentToAdd`、`StashPush` 等）共用 `prepare_pathspec_args`（超过阈值或包含特殊路径时统一走 `--pathspec-from-file` + `--pathspec-file-nul` 临时文件守卫）。
  - 合并 `cli.rs` 与 `details/mod.rs` 的 `git` 可执行文件探测、`GIT_OPTIONAL_LOCKS=0` / `GIT_TERMINAL_PROMPT=0` / `LC_ALL=C` 环境隔离及 Windows `CREATE_NO_WINDOW` 进程构造逻辑；将 `supervisor.rs::spawn_execute_commit` 收拢进 `stage.rs` / `ops.rs` 统一执行路径。
  - 统一 `run_git_op`、提交流与 `run_git_cli` 对 `index_lock_conflict` 的检测与上报语义，不再静默吞掉重试耗尽后的锁冲突标志。
  - 在子进程执行接缝处引入窄端口与两个适配器：生产环境真实 `git` 子进程适配器 + 测试用命令录制/预设输出替身适配器，使全部 ~30 种 `GitOp` 的参数编译、路径分块与错误分类可在毫秒级无子进程开销下完整验证。
- [ ] **统一双文档/三文档 Hunk 构建器与体积守卫读取器，修复 `[diff] tab_size` 失效（`jbgit-core/src/stage_tracker.rs`, `jbgit-engine/src/details/file_diff.rs`, `jbgit-engine/src/stage.rs`, `jbgit-tui/src/syntax.rs`）**：
  - 在 `jbgit-core` 提取共享的 `imara-diff` Hunk 组装原语（基于零拷贝 `SlicedDocument`，统一 `context_len * 2` 合并窗口、`MAX_DIFF_LINES = 20,000` 截断预算、`@@` 头生成、按预算 `word_diff` 及协作取消谓词），供双文档 `FileDiff` 与三文档 `StageLineTracker` 共同调用。
  - 在 `jbgit-engine` 合并 `ReadBytes`（`file_diff.rs`）与 `StageReadSide`（`stage.rs`）为统一的 Blob/工作区安全读取器，统一 4 MiB 体积上限与前 `8 KiB` NUL 字节二进制判定标准。
  - **移除 `file_diff.rs:156`、`stage_tracker.rs:121` 与 `stage.rs:465` 中的 `.replace('\t', "    ")`**，在 `DiffLine.text` 中保留原始 `'\t'`（仅裁剪行尾 `\r`/`\n`）；核对 `syntax.rs`（含启用与禁用语法高亮两条分支、词级高亮切分、搜索高亮与行宽截断）均统一按配置的 `config.jbgit.diff.tab_size` 正确展开列宽，且局部暂存使用原始 `SlicedDocument` 字节回写不受任何影响。

---

## 3. 验收标准 (Acceptance Criteria)

- [ ] `cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
- [ ] 构造包含真实 `\t` 缩进的仓库提交与工作区修改，在 `jbgit.toml` 分别设置 `[diff] tab_size = 2` 与 `tab_size = 8` 时，`VCS Log`、`Stash`、`File History` 与 `Staging Workbench`（无论语法高亮开启或关闭、Unified 或 Split 模式）均精确渲染为 2 列与 8 列空格宽度，且行内词级差异高亮范围不偏移。
- [ ] 对含 `\t` 文件执行逐行/Hunk 暂存与反暂存后，索引与工作区字节中的原始 `\t` 100% 保持原样，不发生意外空格化。
- [ ] `jbgit-core` 中不再包含 `simple_steps` 命令行拼接或裸 `Vec<String>` 补丁参数；`AcceptSide`、`IntentToAdd`、`StashPush`、`CheckoutPaths` 在传入 >50 个路径时统一走 `--pathspec-from-file`。
- [ ] 提交并推送（`SubmitCommit { and_push: true }`）遭遇非快进拒绝时，与手动触发 `Push` 一样弹出 `PushRejected` 处理引导。

---

## Comments

- 2026-11-22 (agent): 基于架构审查 Candidate 05 及 Candidate 06 中的 Hunk/Tab 跨模块编排缺陷创建本 P1 工单，统一 CLI 编译接缝并彻底修复生产环境 `[diff] tab_size` 失效问题。
- 2026-11-22 (agent): 已完成实现与验证：引入纯声明式 `PatchSource` 替代 `PatchToClipboard { args }`，将 `Action::StashAll` 与 `SubmitCommit { and_push: true }` 统一纳入 `GitOp`；将全部 CLI 参数编译迁入 `jbgit-engine/src/ops.rs`，对 `CheckoutPaths`/`AcceptSide`/`IntentToAdd`/`StashPush` 统一走 `prepare_pathspec_args`；建立 `GitCliRunner` 子进程接缝与 `RecordingCliRunner` 测试替身，统一非交互命令构造与 `index.lock` 冲突上报；提取共享 `assemble_hunks`/`build_diff_hunks` 与 `blob_reader`（统一 4 MiB + 8 KiB 二进制守卫）并移除上游 `.replace('\t', "    ")`，彻底修复真实仓库 `[diff] tab_size` 配置；全仓测试与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
