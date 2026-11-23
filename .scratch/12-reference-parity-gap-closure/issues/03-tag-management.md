# 03: Tag 管理（删除本地 / 远端 Tag、推送单个 Tag、Tag 菜单）

Status: resolved
Spec: [../spec.md](../spec.md)
Milestone: M9 — reference 缺口补齐
Priority: P0

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/branch/GitDeleteTagOperation.java`
  - `reference/plugins/git4idea/backend/src/actions/ref/GitDeleteRefAction.kt`
  - `reference/plugins/git4idea/backend/src/actions/tag/GitPushTagsAction.kt`、`GitPushTagsActionGroup.kt`
  - `reference/plugins/git4idea/backend/src/actions/GitSingleTagAction`
- **关联文件**：
  - `crates/jbgit-core/src/git_ops/{ops,outcome}.rs`、`crates/jbgit-engine/src/ops.rs`
  - `crates/jbgit-tui/src/menus/{branch,log}.rs`、`crates/jbgit-tui/src/branches_popup.rs`、`crates/jbgit-tui/src/log_view/branches_pane.rs`、`crates/jbgit-tui/src/dialogs.rs`、i18n locales

---

## 1. 背景与目标

目前只能 `CreateTag` 与 "Copy Tag Name"（`menu.branch.copy_tag_name`）；无法删除 Tag、推送 Tag，也没有像 `RestoreBranch` 那样的撤销。

## 2. 任务清单

- [x] `GitOp` 新增：
  - `DeleteTag { name: String, tip: Oid }` → `git tag -d -- <name>`（保留 `tip` 用于撤销）。
  - `RestoreTag { name: String, oid: Oid }` → `git tag <name> <oid>`（删除成功后的通知提供 "Restore" 动作，与 `RestoreBranch` 对称；附注 Tag 的消息无法恢复时在通知中说明）。
  - `DeleteRemoteTag { remote: String, name: String }` → `git push <remote> --delete refs/tags/<name>`。
  - `PushTag { remote: String, name: String, force: bool }` → `git push <remote> refs/tags/<name>:refs/tags/<name>`。
  - 在 `label()`、写操作分类、刷新范围（`RefreshScope` 刷新 refs）中接入，并补 `simple_steps` 参数顺序单元测试（`--` / `refs/tags/` 全限定，避免与同名分支冲突）。
- [x] `GitFailure` 新增 `TagAlreadyExists`（解析 `already exists`），创建 / 推送时给出"覆盖（`-f`）/ 取消"对话框。
- [x] 菜单：分支面板与分支浮窗中的 Tag 节点、Log 提交上的 Tag pill 子菜单提供：Checkout Tag（detached）、New Branch from Tag、Push Tag to `<remote>`…、Delete Tag、Delete Tag on Remote…（二次确认，提示影响所有协作者，复用 `delete_remote_warn` 文案风格）、Copy Tag Name、Compare with Current。
- [x] 删除本地 Tag 时若远端存在同名 Tag，对话框提供"同时从远端删除"复选框（远端存在性来自 `git ls-remote --tags <remote> <name>` 的只读查询，超时 5s 视为未知并隐藏复选框）。

## 3. 测试

- `jbgit-core`：各新 `GitOp` 编译参数断言。
- 引擎集成：裸仓作远端 → 创建并推送 Tag → 远端删除 → 本地删除并 Restore。
- TUI：Tag 节点菜单项可用性（无远端时远端相关项为 disabled 并带原因）。
