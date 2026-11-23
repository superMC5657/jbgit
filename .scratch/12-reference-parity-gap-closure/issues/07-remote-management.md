# 07: 远端管理（Configure Remotes）与 `RepoSnapshot.remotes`

Status: ready-for-agent
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

- [ ] `RepoSnapshot` 新增 `remotes: Vec<RemoteInfo { name: String, fetch_url: Option<String>, push_url: Option<String> }>`，在快照构建时用 `git2::Repository::remotes()` 读取（只读、廉价）；`default_remote()` 改为：当前分支 upstream 的远端 → `origin` → 第一个远端。
- [ ] 显示时对 URL 中的凭据部分（`https://user:token@host`）做脱敏，模型中保留原值但 `Debug` / 控制台输出脱敏。
- [ ] `GitOp::{AddRemote { name, url }, RenameRemote { old, new }, RemoveRemote { name }, SetRemoteUrl { name, url, push: bool }}`；`AddRemote` 完成后提供"立即 Fetch 该远端"动作（`GitOp::FetchRemote { name }`，`Fetch` 保持 `--all`）。
- [ ] `ModalLayer::Remotes`：列表 + `a` 添加 / `r` 重命名 / `e` 编辑 URL / `Del` 删除（二次确认，说明会删除其所有远端跟踪分支）；入口：分支浮窗与分支面板 "Remotes" 分组的菜单 "Configure Remotes…"。
- [ ] 名称校验复用 `validation` 模块（`git check-ref-format` 规则）；URL 仅做非空与空白校验，不尝试联网验证。

## 3. 测试

- 快照：多远端、仅 push URL、无远端。
- 编译参数与脱敏单元测试（`https://u:secret@h/x.git` → `https://u:***@h/x.git`）。
- 集成：add → fetch → rename 后远端跟踪分支随之改名 → remove。
