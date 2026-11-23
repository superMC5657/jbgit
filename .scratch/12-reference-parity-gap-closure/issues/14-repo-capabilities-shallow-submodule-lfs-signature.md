# 14: 仓库能力感知（Shallow / Submodule / LFS）与签名状态细分

Status: ready-for-agent
Spec: [../spec.md](../spec.md)
Milestone: M9 — reference 缺口补齐
Priority: P3

- **参考源码**：
  - `reference/plugins/git4idea/backend/src/actions/GitUnshallowRepositoryAction`
  - `reference/plugins/git4idea/backend/src/log/{GitCommitSignatureStatusProvider,GitCommitSignatureLoader}.kt`
  - `reference/plugins/git4idea/backend/src/repo/`（子模块相关）
- **关联文件**：
  - `crates/jbgit-engine/src/probe.rs`、`crates/jbgit-engine/src/pool/{snapshot,stage_status}.rs`
  - `crates/jbgit-engine/src/details/commit_changes.rs`（`verify_commit_signature_sync`，当前只把 `%G?` 的 `G` 映射为 `Verified`，其余全部为 `SignedUnverified`）
  - `crates/jbgit-core/src/details.rs`（`SignatureStatus`）、`crates/jbgit-tui/src/views/log_details/`

---

## 1. 背景

几项独立的小缺口，单独成工单收益不高，合并为一个工单待 triage 时再决定是否拆分或部分 `wontfix`。

## 2. 候选任务

- [ ] **Shallow**：`RepoSnapshot.is_shallow`（检测 `<git_dir>/shallow`）；Log 末端显示"浅克隆边界"标记；`GitOp::Unshallow` → `git fetch --unshallow`（长时操作，控制台显示进度）。
- [ ] **Submodule**：Stage 树中子模块条目显示为只读特殊节点（新提交 / 脏工作区两种状态），禁止行级暂存，提供"在切换器中打开子模块"（依赖 Issue 09）与 `git submodule update --init --recursive` 动作。
- [ ] **LFS**：检测 `.gitattributes` 中的 `filter=lfs`；Diff 遇到 LFS 指针文件时显示"LFS 对象：oid / size"占位而非指针文本；不实现 LFS 下载。
- [ ] **签名状态细分**：`SignatureStatus` 扩展为 `Good`、`GoodUnknownValidity (U)`、`Expired (X/Y)`、`Revoked (R)`、`CannotCheck (E)`、`Bad (B)`、`Unsigned (N)`，并附 `signer: Option<String>`（`%GS`）与 `key: Option<String>`（`%GK`）；详情面板与 Log 列显示对应图标与提示，对齐 `GitCommitSignatureStatusProvider`。

## 3. Triage 结论（2026-11-23 维护者拍板）

- **四项全部要做**，保留在本工单内，按下列顺序实现，每项单独提交：签名状态细分 → 子模块只读标注 → LFS 指针占位 → 浅克隆 Unshallow。
- 子模块的"在切换器中打开"依赖 Issue 09；09 未落地时先只做只读标注与 `submodule update` 动作。
- 子模块不作为多根仓库处理（见 Issue 11 的 Answer）。

## 4. 测试

- 签名：`%G?` 各返回值到 `SignatureStatus` 的映射单元测试；`%GS` / `%GK` 解析。
- 子模块：临时仓库添加子模块 → 子模块内新提交与脏工作区两种状态在 Stage 树中正确标注，行级暂存被禁用并给出原因。
- LFS：构造 LFS 指针文本（无需安装 git-lfs）→ Diff 显示占位。
- Shallow：`git clone --depth 1` 的临时仓库 → `is_shallow == true` → Unshallow 后变为 false。
