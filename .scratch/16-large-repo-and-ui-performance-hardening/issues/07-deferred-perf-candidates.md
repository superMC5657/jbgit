# 07: 范围修订后暂缓的性能候选项（待评估）

Status: needs-triage
Spec: [../spec.md](../spec.md)
Milestone: M13 — 大仓库、渲染热路径与引擎并发性能二期治理
Priority: P2

- **来源**：2026-11-28 对 `01`–`06` 的价值评估（见 [`spec.md` §5](../spec.md#5-范围修订2026-11-28)）。以下各项有真实收益，但只在特定场景下明显，本轮暂缓；在出现对应场景的实际卡顿反馈或实测数据后再逐项立项。

---

## 1. 候选项

| # | 候选项 | 来源 | 何时值得做 |
| :---: | :--- | :---: | :--- |
| 1 | Diff 软换行行高前缀和缓存（`total_rows` / `visual_row_offset_of` / 渲染跳过视口前区间） | [`03`](./03-conflict-merge-and-diff-soft-wrap-search-perf.md) | 开启 soft wrap 且常看上万行 diff |
| 2 | 非模态 Log / Stage / Stash diff 搜索粘贴逐字符重算（`input/mod.rs`） | [`03`](./03-conflict-merge-and-diff-soft-wrap-search-perf.md) | 常在这些搜索框粘贴长串 |
| 3 | 超过 4 MiB 的文本冲突以 "Binary" 标题降级，需新增 TooLarge 变体与 i18n 文案 | [`03`](./03-conflict-merge-and-diff-soft-wrap-search-perf.md) | 体验问题，非性能项 |
| 4 | Commit Details 正文 hash 链接每帧线性 `resolve_revision`（改缓存或 OID 索引查找） | 原 04（已删除） | 大仓库且提交信息常含 hash |
| 5 | Filter 弹窗 `Paths` 每帧 `read_dir`、`User` 每帧统计作者（改一次性缓存） | 原 04（已删除） | 大仓库下筛选弹窗输入卡顿 |
| 6 | Console 视口虚拟化（`O(H)` 渲染）与 `ConsoleWorkbench::entries` 条数上限（不截断单条命令输出） | 原 04（已删除） | 长会话、大量命令输出 |
| 7 | `unstage_path_in_index` 的 `O(targets × index_entries)` 查找 | [`06`](./06-engine-tokio-unblock-windows-scaling-and-benchmarks.md) | 大仓库一次 unstage 数百文件 |
| 8 | `UpdateProject` / `collect_update_info` 按分支串行 spawn `git.exe`（改 libgit2 `graph_ahead_behind`） | [`06`](./06-engine-tokio-unblock-windows-scaling-and-benchmarks.md) | 跟踪分支多且常用 Update Project |

## 2. 产品决策（非性能项）

- **`recurse_untracked_dirs` 默认值**（`scan_config.rs`，当前 `StatusScanMode::Always`）：改为 `Auto` 会改变未跟踪目录在变更列表中的展示方式（目录折叠 vs 逐文件列出），需要产品层面决定，不作为性能修复处理。

## Comments

- **2026-11-28**：随 Spec 16 范围修订创建，汇总 `03` / `04` / `06` 中暂缓的候选项。
