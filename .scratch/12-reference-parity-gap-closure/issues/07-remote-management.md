# 07: 远端管理（Configure Remotes）与 `RepoSnapshot.remotes`

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M9 — reference 缺口补齐
Priority: P1

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/remote/GitConfigureRemotesAction.kt`、`GitConfigureRemotesDialog.kt`
  - `reference/plugins/git4idea/backend/src/repo/GitRemote*`
- **关联文件**：
  - `crates/jbgit-core/src/repo.rs`（`RepoSnapshot`）、`crates/jbgit-engine/src/pool/snapshot.rs`
  - `crates/jbgit-core/src/git_ops/ops.rs`、`crates/jbgit-engine/src/ops.rs`
  - `crates/jbgit-tui/src/app/git_workflows.rs`（`default_remote()`）、`crates/jbgit-tui/src/dialogs.rs`、`crates/jbgit-tui/src/branches_popup.rs`

---

## 1. 背景与目标

`jbgit` 没有远端列表模型：Push / Delete Remote Branch 依赖从 upstream 名或 `default_remote()` 推断远端，也无法在 TUI 内添加 fork 远端或修改 URL。

## 2. 任务清单

- [x] `RepoSnapshot` 新增 `remotes: Vec<RemoteInfo { name: String, fetch_url: Option<String>, push_url: Option<String> }>`，在快照构建时用 `git2::Repository::remotes()` 读取（只读、廉价）；`default_remote()` 改为：当前分支 upstream 的远端 → `origin` → 第一个远端。
- [x] 显示时对 URL 中的凭据部分（`https://user:token@host`）做脱敏，模型中保留原值但 `Debug` / 控制台输出脱敏。
- [x] `GitOp::{AddRemote { name, url }, RenameRemote { old, new }, RemoveRemote { name }, SetRemoteUrl { name, url, push: bool }}`；`AddRemote` 完成后提供"立即 Fetch 该远端"动作（`GitOp::FetchRemote { name }`，`Fetch` 保持 `--all`）。
- [x] `ModalLayer::Remotes`：列表 + `a` 添加 / `r` 重命名 / `e` 编辑 URL / `Del` 删除（二次确认，说明会删除其所有远端跟踪分支）；入口：分支浮窗与分支面板 "Remotes" 分组的菜单 "Configure Remotes…"。
- [x] 名称校验复用 `validation` 模块（`git check-ref-format` 规则）；URL 仅做非空与空白校验，不尝试联网验证。

## 3. 测试

- 快照：多远端、仅 push URL、无远端。
- 编译参数与脱敏单元测试（`https://u:secret@h/x.git` → `https://u:***@h/x.git`）。
- 集成：add → fetch → rename 后远端跟踪分支随之改名 → remove。

## Comments

- 2026-11-24 实现说明：
  - `RepoSnapshot.remote_names` 被 `remotes: Vec<RemoteInfo>` 取代（`remote_names()` 保留为迭代器方法）；快照用 `git2` 读取 `url` / `pushurl`，`origin` 优先、其余自然排序。`RemoteInfo` 的 `Debug` 与所有显示路径经 `redact_url` 脱敏（仅遮蔽 `user:password@` 的密码部分；`https://token@host` 这类无冒号的 userinfo 原样显示，因为无法与普通用户名区分）。控制台命令行、流式输出行与 `GitFailure::Other` 文案也统一经 `redact_urls_in` 脱敏。
  - `RepoSnapshot::default_remote()`：upstream 所在远端（按最长已配置远端名前缀匹配，支持含 `/` 的远端名）→ `origin` → 第一个远端；TUI 的 `default_remote()` 改为调用它。
  - 新增 `GitOp::{AddRemote, RenameRemote, RemoveRemote, SetRemoteUrl, FetchRemote}`，编译为 `git remote add|rename|remove|set-url [--push] -- …` 与 `git fetch --prune <name>`；`GitFailure::RemoteAlreadyExists` 以通知提示。
  - `ModalLayer::Remotes`（`remotes_view.rs`）：列表实时读取快照；按键 `a`/`n`/`Insert` 添加（两步：名称 → URL）、`r`/`F2` 重命名、`e`/`Enter`/双击 编辑获取 URL、`p` 编辑推送 URL、`d`/`Del` 删除（危险确认，列出将删除的跟踪分支数与失去 upstream 的本地分支数）、`f` 仅获取该远端。AddRemote 成功后通知栏提供「获取 '<name>'」。
  - 入口：全局 `<A-r>`（`configure_remotes`，进入 `?` 帮助与底栏元数据）、分支浮窗 `⚙ Configure Remotes…` 行、分支树 `Remote` 分组 / `<remote>` 节点菜单（附 `Fetch '<remote>'`）。
  - 测试：`jbgit-core` 脱敏 / 默认远端 / 失败分类单测；`jbgit-engine` 编译参数单测与真实仓库集成测试（多远端 + 仅 push URL + 无远端快照；add → 重复 add 失败 → fetch → rename 后跟踪分支随之改名 → set-url --push → remove）；`TuiSession` 覆盖 Remotes 模态全部操作与入口。
