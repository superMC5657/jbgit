# 01: Windows Clone 路径 `\\?\` 前缀剥离与 `Unshallow` 提交图谱缓存失效修复

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M11 — Pilot QA 缺陷修复与体验加固
Priority: P1
QA-Refs: `P1-01`, `P1-02`

- **关联文件**：
  - `crates/jbgit-engine/src/probe.rs`（`strip_verbatim_prefix_str` `:61-82`，当前为 `pub(crate)`）
  - `crates/jbgit-tui/src/session.rs`（`init_engine_and_session` `:902-915`）
  - `crates/jbgit-tui/src/repo_lifecycle.rs`（`CloneDialogState::sync_inferred_target_dir` `:142-160`、`build_request` `:286-294`、`default_clone_base_dir` `:322-334`）
  - `crates/jbgit-engine/src/pool/worker_pool.rs`（`with_thread_repo` `:25-58`）
  - `crates/jbgit-engine/src/pool/snapshot.rs`（`load_repo_snapshot_incremental_sync` `:423-481`、`try_incremental_commits_refresh` `:944-961`、`collect_commits_with_reuse` `:1055-1102`）

---

## 1. 根因定位

### 1.1 [`P1-01`] Clone 自动推导的目标目录带 Windows `\\?\` 前缀导致 `git clone` 失败
- 在非 Git 目录启动进入 Welcome 模态时，`crates/jbgit-tui/src/session.rs:903-906` 直接执行：
  ```rust
  let resolved_dir = target_path
      .canonicalize()
      .or_else(|_| std::env::current_dir().map(|cwd| cwd.join(&target_path)))
      .unwrap_or(target_path);
  ```
- Windows 上 `std::fs::canonicalize` 返回带逐字前缀 `\\?\C:\...` 的路径。`jbgit-engine/src/probe.rs:57-82` 已经实现了 `strip_verbatim_prefix` / `strip_verbatim_prefix_str` 处理 `\\?\` 和 `\\?\UNC\`，但 `session.rs:903` 没有调用它，直接把 `\\?\...` 写入 `AppState.repo_path_display` 和 `WelcomeState`。
- 用户在 Welcome 弹窗按 `c` 打开 `CloneDialogState` 时，`default_clone_base_dir`（`repo_lifecycle.rs:322-334`）与 `sync_inferred_target_dir`（`repo_lifecycle.rs:142-160`）把 `base_dir` 拼成 `\\?\C:\...\sandbox-clone-src` 并作为参数传给 `git clone`，而 Windows 版 Git CLI 拒绝创建以 `\\?\` 开头的目录并报 `fatal: could not create work tree dir '\\?\...'`。

### 1.2 [`P1-02`] `git fetch --unshallow` 后提交图谱永久卡死在旧数量（`3 个提交`）
存在**双重根因**叠加：
1. **线程本地 `git2::Repository` 缓存持有陈旧 shallow grafts（`pool/worker_pool.rs:25-58`）**：
   - `with_thread_repo` 将 `git2::Repository` 缓存在 `thread_local! THREAD_REPOS` 中，复用时只调用 `index.read(true)` 刷新 `.git/index`，从未因 `.git/shallow` 文件变化而重建句柄。
   - 在 libgit2 中，`repo.is_shallow()` 直接 `stat` 磁盘上的 `.git/shallow` 文件（因此 UI 顶部的 `· 浅克隆` 徽章正确消失），但 `git_repository` 结构体内部在首次 `revwalk`/`find_commit` 时缓存的 `shallow_grafts` 不会因外部 `git fetch --unshallow` 删除 `.git/shallow` 而失效！因此后续 `revwalk` 仍在原浅边界提交 `c73a0a6` 处被截断，只产出原来的 3 个提交。
2. **浅克隆状态变化时 `collect_commits_with_reuse` 错误复用了旧缓存（`pool/snapshot.rs:468-478, 1070-1094`）**：
   - 当 `probe.is_shallow != prev.is_shallow` 时，`try_incremental_commits_refresh`（`:957`）正确返回 `Ok(None)` 回退到全量分支；
   - 但 `else` 分支（`:468-471`）在 `SnapshotLoadMode::Incremental` 下仍将 `let reuse = previous;` 传给 `collect_commits_with_reuse`；
   - `collect_commits_with_reuse`（`:1070`）仅检查 `c.load_commit_bodies == load_commit_bodies`，未检查 `is_shallow` / `shallow_fingerprint` 是否变化。即便 `revwalk` 走到了全部 6 个提交，原浅边界提交 `c73a0a6` 也会命中 `cache.oid_to_index` 直接 `cached.clone()`，保留原先在浅克隆时生成的 `parents: vec![]`，导致详情仍显示 `父提交 (根提交)` 且图谱断链。

---

## 2. 任务清单

- [ ] **统一剥离 Windows `\\?\` 前缀（`P1-01`）**：
  - 将 `strip_verbatim_prefix` / `strip_verbatim_prefix_str`（`crates/jbgit-engine/src/probe.rs:61-82`）提升为 `pub`（或下沉到 `jbgit-core` 供 `engine` 与 `tui` 共享）；
  - 在 `crates/jbgit-tui/src/session.rs:903-906` 对 `resolved_dir` 应用 `strip_verbatim_prefix`；
  - 在 `crates/jbgit-tui/src/repo_lifecycle.rs` 的 `default_clone_base_dir`（`:322`）及 `CloneDialogState::build_request`（`:286-294`）对 `target_dir` 同样做兜底剥离，确保无论路径来自启动目录、当前仓库父目录还是用户粘贴，传给 `git clone` 的都不含 `\\?\`。
- [ ] **浅克隆状态变化时驱逐线程本地 `Repository` 并禁用提交缓存复用（`P1-02`）**：
  - 在 `crates/jbgit-engine/src/pool/worker_pool.rs` 增加 `evict_thread_repo(git_dir: &Path)` 或 `with_fresh_thread_repo(git_dir: &Path, f)`（也可在 `THREAD_REPOS` 中随 `Repository` 一并记录上次打开时的 `shallow_fingerprint: Option<Vec<u8>>`，当磁盘 `<common_dir|git_dir>/shallow` 存在性或内容发生变化时自动移除旧 `Repository` 并重新 `Repository::open(git_dir)`，从而惠及全部 worker 线程上的 `revwalk`、`find_commit`、`details` 查询！）；
  - 在 `crates/jbgit-engine/src/pool/snapshot.rs:468-471` 与 `:1070`：当 `previous` 存在且 `probe.is_shallow != prev.is_shallow || shallow_fingerprint != prev.shallow_fingerprint` 时，强制 `reuse = None`（并在 `collect_commits_with_reuse` 签名/过滤中双保险校验），确保旧浅边界提交重新读取完整 `parents`；
  - 检查 `crates/jbgit-engine/src/details/`（提交详情加载）若同样通过 `with_thread_repo` 读取 `commit.parent_ids()`，依赖上述 `THREAD_REPOS` 按 shallow 指纹自动重开即可一并修复详情页 `父提交 (根提交)`。

---

## 3. 测试与验收

- **单元测试（`P1-01`）**：
  - 验证 `CloneDialogState` 在 `base_dir` 为 `\\?\C:\Users\dev\workspace` 时推导出的 `target_dir` 和 `build_request().target_dir` 均为 `C:\Users\dev\workspace\<repo>`（以及 `\\?\UNC\server\share\dir` 正确转为 `\\server\share\dir`）。
- **真实临时仓库集成测试（`P1-02`，修复前必挂）**：
  - 在同一 worker 线程池（复现 `with_thread_repo` 缓存）中：先对 `--depth 1` 浅克隆仓库调用 `load_repo_snapshot_incremental_sync`（确认 `is_shallow == true`、提交数为浅克隆数、边界提交 `parents.is_empty()`）→ 执行 `GitOp::Unshallow`（`git fetch --unshallow`）→ 再次以 `SnapshotLoadMode::Incremental` 传入 `previous` cache 调用 `load_repo_snapshot_incremental_sync` → 断言 `is_shallow == false`、提交数量恢复完整、原浅边界提交的 `parents` 非空、且 `load_commit_details` 返回正确父提交。
- **Pilot 端到端复验**：
  - `welcome` 场景按 `c` 输入 `sandbox-clone-src.git` → `Tab` → `Enter` 直接克隆成功；
  - `capabilities` 场景点击浅克隆边界 `完整拉取…` → `Enter` → 图谱自动更新为 `6 个提交` 且 `c73a0a6` 显示真实父提交。
