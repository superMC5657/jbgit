# 09: 进程内仓库会话重绑定与最近仓库切换器

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M9 — reference 缺口补齐
Priority: P1

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/repo/GitRepositoryManager.java`、`GitRepositoryImpl.kt`
- **关联文件**：
  - `crates/jbgit-tui/src/session.rs`（`init_engine_and_session` 约 `:528-560`，以及会话关闭时的 `flush_draft_sync` / `graph_renderer.shutdown_sequence`）
  - `crates/jbgit-engine/src/{supervisor,watcher,probe}.rs`
  - `crates/jbgit-tui/src/app/{state,events}.rs`（`load_settings(snapshot.common_dir.join("jbgit-branches-panel"))`）
  - `crates/jbgit-tui/src/config/{mod,discovery}.rs`

---

## 1. 背景与目标

`jbgit` 在启动时对目标路径执行一次 `RepoLocation::discover` 并启动 `EngineSupervisor`，之后无法切换仓库。Worktree 的"打开"（Issue 10）与任何形式的多仓库支持（Issue 11）都需要在运行中更换仓库，因此先把会话绑定做成可重入的。

## 2. 任务清单

- [ ] **`RepoSession::bind(location) / unbind()`**：从 `init_engine_and_session` 中抽出；`unbind` 依次执行：取消全部 `RequestSlot`、`stage_workbench.flush_draft_sync()`、停止 watcher 与 `EngineSupervisor`（等待在途写操作完成，超时 5s 后提示）、图形后端 `shutdown_sequence`；`bind` 重新加载按仓库叠加的配置层（`.jbgit/`）、分支面板设置与提交草稿。
- [ ] **保护**：存在进行中的写操作或未完成的 rebase / merge 模态流程时，切换需确认；未保存的冲突合并编辑必须先保存或放弃。
- [ ] **最近仓库**：用户级配置目录下 `recent-repos.toml`（路径、显示名、最后打开时间，上限 20，自动剔除已不存在的路径）。
- [ ] **`ModalLayer::RepoSwitcher`**：速查过滤（复用 `nucleo-matcher`）列出：最近仓库、当前仓库的 linked worktrees（Issue 10 落地前可只读列出 `git worktree list`）、当前仓库的子模块；`Enter` 切换，`Ctrl-o` 输入任意路径。
- [ ] 头部仓库名可点击打开切换器；默认全局键位 `Alt-o`（`CommandCatalog` 注册）。
- [ ] `tools/jbgit-pilot` 新增双仓库沙箱场景，验证切换后 Log / Stage / Stash / Console 全部指向新仓库且旧仓库的晚到事件被丢弃（事件需携带会话代号 `session_generation` 并在 TUI 侧过滤）。

## 3. 测试

- `TuiSession` 测试：A 仓库发起慢 Diff 请求 → 切换到 B → A 的结果到达后被丢弃，不污染 B 的状态。
- 切换前后草稿各自保存与恢复。
- 不存在的最近仓库路径被剔除并提示。
