# 09: 进程内仓库会话重绑定与最近仓库切换器

Status: resolved
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

- [x] **`RepoSession::bind(location) / unbind()`**：从 `init_engine_and_session` 中抽出；`unbind` 依次执行：取消全部 `RequestSlot`、`stage_workbench.flush_draft_sync()`、停止 watcher 与 `EngineSupervisor`（等待在途写操作完成，超时 5s 后提示）、图形后端 `shutdown_sequence`；`bind` 重新加载按仓库叠加的配置层（`.jbgit/`）、分支面板设置与提交草稿。
- [x] **保护**：存在进行中的写操作或未完成的 rebase / merge 模态流程时，切换需确认；未保存的冲突合并编辑必须先保存或放弃。
- [x] **最近仓库**：用户级配置目录下 `recent-repos.toml`（路径、显示名、最后打开时间，上限 20，自动剔除已不存在的路径）。
- [x] **`ModalLayer::RepoSwitcher`**：速查过滤（复用 `nucleo-matcher`）列出：最近仓库、当前仓库的 linked worktrees（Issue 10 落地前可只读列出 `git worktree list`）、当前仓库的子模块；`Enter` 切换，`Ctrl-o` 输入任意路径。
- [x] 头部仓库名可点击打开切换器；默认全局键位 `Alt-o`（`CommandCatalog` 注册）。
- [x] `tools/jbgit-pilot` 新增双仓库沙箱场景，验证切换后 Log / Stage / Stash / Console 全部指向新仓库且旧仓库的晚到事件被丢弃（事件需携带会话代号 `session_generation` 并在 TUI 侧过滤）。

## 3. 测试

- `TuiSession` 测试：A 仓库发起慢 Diff 请求 → 切换到 B → A 的结果到达后被丢弃，不污染 B 的状态。
- 切换前后草稿各自保存与恢复。
- 不存在的最近仓库路径被剔除并提示。

## Comments

- 2026-11-24 实现说明：
  - 会话拆分：`session.rs` 新增 `RepoSession { handle, events, generation }`（`discover` / `bind_location` / `unbind`）与 `TuiSession::{prepare_unbind, rebind, take_repo_switch_request, generation}`。`init_engine_and_session` 改为返回 `(RepoSession, TuiSession)`；`main.rs` 在每批次结束后检查待切换路径：先 `discover` 校验（失败则保持当前仓库并报错、移出最近列表；解析到当前仓库则仅提示）→ `prepare_unbind`（使本地单飞槽位失效、`flush_draft_sync`、图形 `shutdown_sequence`）→ `RepoSession::unbind`（`EngineHandle::shutdown`：推进全部 `RequestSlotId` 使在途读取作废、关闭任务队列从而停止 supervisor 与 watcher、等待 `MutationGuard` 空闲最多 5s，超时返回 false 并在新仓库的通知栏提示"上一个仓库的操作仍在后台运行"）→ `bind_location`（按新 workdir 重新加载 `.jbgit/` 配置层、检测 git 版本）→ `rebind`（携带终端尺寸、图形能力、凭据队列、最近仓库列表；分支面板设置与提交草稿随新仓库首个快照加载）→ `RefreshAll`。
  - 会话代号：`AppState.session_generation` 每次 rebind 递增；新增 `SessionInput::{EngineFrom, UiAsyncFrom}(generation, …)`，代号不符直接丢弃。生产环境引擎事件与后台任务结果（语法高亮 / 包含分支 / 图重建）均打戳；askpass 提示不打戳（属于仍在运行的 git 进程）。
  - 提交草稿改为按工作区保存在 `git_dir`（主工作区 `git_dir == common_dir`，行为不变；linked worktree 各自独立），满足"切换前后草稿各自保存与恢复"。
  - 保护：存在运行中的 Git 操作 / 提交、或打开着 RebaseTodo / Merge / Message / Push 对话框时弹确认（默认"取消"）；冲突合并窗口有未保存选择时直接拒绝。
  - 最近仓库：`recent-repos.toml`（`$JBGIT_CONFIG_DIR` 或全局配置目录，`[[repos]] path/name/last_opened`，上限 20，加载时剔除不存在路径并在下次打开切换器时提示）。
  - `ModalLayer::RepoSwitcher`：打开时采集条目（最近仓库 → 当前仓库其他非失效 worktree → 已初始化子模块，均排除当前仓库），`fuzzy_indices`（nucleo）同时匹配名称与路径；`Enter` / 双击切换、`Ctrl-o` 输入任意路径（相对当前仓库所在目录）、`Del` 移出最近列表、粘贴进入查询。入口：`<A-o>`（`switch_repository`，进入 `?` 帮助与底栏元数据）、点击头部仓库名（`HitTarget::HeaderRepoName`）。
  - `tools/jbgit-pilot` 新增 `--scenario multi-repo`（`sandbox-repo` + linked worktree `sandbox-repo-feature` + 独立仓库 `sandbox-repo-b`，隔离配置目录预置 `recent-repos.toml`）。已用真实二进制验证：切到 B → 头部 / Log / Stage / Console 指向 B 并通知"已切换"；切回主仓库；经 `<A-w>` 打开 worktree 后头部显示 `sandbox-repo-feature` 与 `feature/ui-test`，分支浮窗中 `main` 标注 `⎇ sandbox-repo`。
  - 测试：`repo_switcher` 存储（持久化 / 剔除 / 上限 / 移除）与过滤单测；`TuiSession` 覆盖 A 的晚到快照 / GitOpFinished / 后台结果在切到 B 后被丢弃、草稿按工作区保存与恢复（含头部仓库名与分支更新）、切换保护（不存在路径剔除并提示、运行中操作确认、冲突未保存拒绝）、切换器全部入口与操作、rebind 携带状态与超时提示；引擎集成测试覆盖 `EngineHandle::shutdown`。
