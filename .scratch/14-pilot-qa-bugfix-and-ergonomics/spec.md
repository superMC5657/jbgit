# Spec 14: 探索性 TUI QA（`jbgit-pilot`）发现缺陷定位与修复（Phase 9）

- **目录**：`.scratch/14-pilot-qa-bugfix-and-ergonomics/`
- **来源报告**：[`.pilot/qa-report.md`](../../.pilot/qa-report.md)（2026-11-25 全量 10 个沙盒场景 + `hover`/`resize` 压测，0 P0 / 2 P1 / 13 P2 / 4 P3，共 19 项发现）
- **QA 规程**：[`docs/agents/ai-exploratory-testing.md`](../../docs/agents/ai-exploratory-testing.md)
- **下属 Issues**（按优先级 P1 → P2 → P3 拆分为 6 个可独立并行执行的原子工单）：
  - [`01-windows-clone-path-and-unshallow-refresh.md`](./issues/01-windows-clone-path-and-unshallow-refresh.md)（**P1** · `P1-01` Windows `\\?\` 前缀导致 Clone 失败、`P1-02` `git fetch --unshallow` 后提交图谱卡死在旧数量）
  - [`02-staging-notice-and-conflict-unresolve-correctness.md`](./issues/02-staging-notice-and-conflict-unresolve-correctness.md)（**P2** · `P2-01` 暂存进度提示与 spinner 永不清除、`P2-02` `UD` 冲突重置 `r` 失败、`P2-05` FileHistory `范围:HEAD` 与 `start_oid` 语义矛盾）
  - [`03-log-changes-tree-and-branches-shortcuts.md`](./issues/03-log-changes-tree-and-branches-shortcuts.md)（**P2** · `P2-03` 变更树直按 `c`/`p`/`l` 失效且 `p` 误触发全局 Pull、`P2-04` 目录右键菜单暴露文件专属项、`P2-08` 分支浮窗 `Space` 不收藏、`P2-09` 子菜单 `F2` 不重命名、`P2-10` Diff `Ctrl+F` 不触发内搜）
  - [`04-conpty-pilot-keys-and-modal-escape.md`](./issues/04-conpty-pilot-keys-and-modal-escape.md)（**P2** · `P2-07` `Ctrl+~`/`Ctrl+\``、`P2-11` Push `Ctrl+Enter`、`P2-12` `Esc` 在 ConPTY 下失效 — 根因在 `jbgit-pilot` Windows 按键编码 + `jbgit-tui` 守卫补齐）
  - [`05-filter-bar-and-dialog-layout-overflow.md`](./issues/05-filter-bar-and-dialog-layout-overflow.md)（**P2** · `P2-06` 140 列下 Log 筛选栏挤掉 `路径▾`/`[排除合并]` 及作者弹窗 `Ctrl+U` 失效、`P2-13` 新建 Worktree `--force` 复选框提示截断）
  - [`06-modal-banner-console-and-ux-polish.md`](./issues/06-modal-banner-console-and-ux-polish.md)（**P3** · `P3-01` 身份弹窗初始即报红、`P3-02` Welcome 背景残留加载文案及 Clone 报错重复、`P3-03` Bisect 按钮空格与 Console 失败行排版截断、`P3-04` `Ctrl+a` 全选 / 自身分支菜单 / Hunk 回滚文案）

---

## 1. 背景与定位结论摘要

2026-11-25 使用 `jbgit-pilot` 在 Windows ConPTY + `vt100` 环境下对 `jbgit` 执行了全量 10 场景双重验证（终端渲染 + 磁盘 `git -C` 状态）。结论如下：

1. **稳定性与安全性达标（0 P0）**：全量运行零 panic、零死锁/卡死、零数据丢失、零明文凭据泄露。
2. **2 项 P1 为真实核心逻辑缺陷**：
   - `P1-01`：`session.rs:903-906` 对非 Git 启动目录调用 `canonicalize()` 未剥离 Windows `\\?\` 逐字前缀，透传进 `CloneDialogState`，导致 `git clone` 创建工作目录报错；
   - `P1-02`：双重根因 — `pool/worker_pool.rs:25-58` 线程本地缓存的 `git2::Repository` 在外部执行 `git fetch --unshallow` 后仍持有旧的 libgit2 shallow grafts，且 `pool/snapshot.rs:468-478, 1070-1094` 在浅克隆状态变化时仍复用旧 `SnapshotCommitCache`（保留了浅边界提交的空 `parents: []`）。
3. **3 项 P2 键位缺陷（`P2-07` / `P2-11` / `P2-12`）主因在 QA 驱动 `tools/jbgit-pilot/src/keys.rs`**：
   - `jbgit-tui` 本身已支持 `<C-`>` / `<C-~>`、`Ctrl+Enter` 推送与 `Esc` 关窗；但 `jbgit-pilot` 在 Windows ConPTY 下把 ``Ctrl+` ``/`Ctrl+~` 编码成 `0x00`、把 `Ctrl+Enter` 发成 ConPTY 不识别的 CSI `u` `\x1b[13;5u`、把 `Esc` 发成裸 `0x1b`（且多键不逐键 flush），造成 ConPTY 丢键或误合并；同时 `jbgit-tui` 在 `Reflog` / `TreeBrowser` 中对 `Esc` 附带了多余的 `!ctrl && !alt` 限制。
4. **其余 10 项 P2 与 4 项 P3 均为已精准定位的 TUI / Engine 缺陷**，全部具备明确修改点与低风险回归测试路径。

---

## 2. QA 编号 → 工单映射全景表

| QA ID | 严重度 | 归属工单 | 涉及 Crate | 核心根因文件 |
| :---: | :---: | :--- | :--- | :--- |
| `P1-01` | P1 | [`01`](./issues/01-windows-clone-path-and-unshallow-refresh.md) | `jbgit-engine`, `jbgit-tui` | `session.rs:903`, `repo_lifecycle.rs:142,322`, `probe.rs:61` |
| `P1-02` | P1 | [`01`](./issues/01-windows-clone-path-and-unshallow-refresh.md) | `jbgit-engine` | `pool/worker_pool.rs:25`, `pool/snapshot.rs:468,1070` |
| `P2-01` | P2 | [`02`](./issues/02-staging-notice-and-conflict-unresolve-correctness.md) | `jbgit-tui` | `stage_view/workbench.rs:1690,1958,1978` |
| `P2-02` | P2 | [`02`](./issues/02-staging-notice-and-conflict-unresolve-correctness.md) | `jbgit-engine` | `ops.rs:544` |
| `P2-05` | P2 | [`02`](./issues/02-staging-notice-and-conflict-unresolve-correctness.md) | `jbgit-engine`, `jbgit-tui` | `views/file_history.rs:121`, `details/file_history.rs:438` |
| `P2-03` | P2 | [`03`](./issues/03-log-changes-tree-and-branches-shortcuts.md) | `jbgit-tui` | `default.toml:20,60`, `keymap/catalog.rs`, `menus/log.rs` |
| `P2-04` | P2 | [`03`](./issues/03-log-changes-tree-and-branches-shortcuts.md) | `jbgit-tui` | `menus/log.rs:1086-1249` |
| `P2-08` | P2 | [`03`](./issues/03-log-changes-tree-and-branches-shortcuts.md) | `jbgit-tui` | `branches_popup.rs:381-432` |
| `P2-09` | P2 | [`03`](./issues/03-log-changes-tree-and-branches-shortcuts.md) | `jbgit-tui` | `input/menus.rs:108-167` |
| `P2-10` | P2 | [`03`](./issues/03-log-changes-tree-and-branches-shortcuts.md) | `jbgit-tui` | `default.toml:165`, `input/keyboard.rs:868,1084,1414` |
| `P2-07` | P2 | [`04`](./issues/04-conpty-pilot-keys-and-modal-escape.md) | `jbgit-pilot`, `jbgit-tui` | `tools/jbgit-pilot/src/keys.rs:149,161` |
| `P2-11` | P2 | [`04`](./issues/04-conpty-pilot-keys-and-modal-escape.md) | `jbgit-pilot`, `jbgit-tui` | `tools/jbgit-pilot/src/keys.rs:134-142`, `locales/*.toml:601` |
| `P2-12` | P2 | [`04`](./issues/04-conpty-pilot-keys-and-modal-escape.md) | `jbgit-pilot`, `jbgit-tui` | `tools/jbgit-pilot/src/keys.rs:47,143`, `server.rs:316`, `keyboard.rs:1381`, `tree_browser.rs:694` |
| `P2-06` | P2 | [`05`](./issues/05-filter-bar-and-dialog-layout-overflow.md) | `jbgit-tui` | `views/log.rs:384-682`, `input/keyboard.rs:223-386` |
| `P2-13` | P2 | [`05`](./issues/05-filter-bar-and-dialog-layout-overflow.md) | `jbgit-tui` | `views/dialogs.rs:1367-1435` |
| `P3-01` | P3 | [`06`](./issues/06-modal-banner-console-and-ux-polish.md) | `jbgit-tui` | `dialogs.rs:1147`, `views/dialogs.rs:509`, `app/git_workflows.rs:178` |
| `P3-02` | P3 | [`06`](./issues/06-modal-banner-console-and-ux-polish.md) | `jbgit-tui` | `views/header.rs:154`, `views/log.rs:694`, `app/events.rs:97` |
| `P3-03` | P3 | [`06`](./issues/06-modal-banner-console-and-ux-polish.md) | `jbgit-tui` | `views/banner.rs:129`, `views/console.rs:176,184` |
| `P3-04` | P3 | [`06`](./issues/06-modal-banner-console-and-ux-polish.md) | `jbgit-tui` | `dialogs.rs:188,2314`, `menus/branch.rs:122-237`, `stage_view/workbench.rs:2506` |

---

## 3. 执行顺序与依赖

6 个工单彼此代码面几乎完全正交，无强制串行依赖，建议按严重度分批推进：

```text
Batch 1 (P1 阻断核心路径): 01 Windows Clone 路径 & Unshallow 图谱刷新
Batch 2 (P2 行为与安全):   02 暂存进度/冲突重置/文件历史范围  |  03 变更树与分支快捷键  |  04 ConPTY 按键与 Esc
Batch 3 (P2/P3 布局与打磨): 05 筛选栏与弹窗溢出修复            |  06 弹窗/横幅/控制台/文案打磨
```

---

## 4. 验收总则

- 每个工单落地均须补齐对应的单元测试 / `TestBackend` 渲染测试 / 真实临时仓库集成测试（修复前必挂）；
- 全量通过：
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace --locked`
- 按 [`docs/agents/ai-exploratory-testing.md`](../../docs/agents/ai-exploratory-testing.md) Step 4 在 `jbgit-pilot` 对应沙盒场景中端到端复验并更新或归档 `.pilot/qa-report.md`。
