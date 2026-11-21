# 01: 异步语法高亮、大文件/长行安全护栏与线性 Diff 算法

Status: done
Spec: [../spec.md](../spec.md)
Milestone: M7 — 性能架构治理与交互零卡顿优化 (P0)

- **关联文件**：
  - `Cargo.toml`
  - `crates/jbgit-core/src/details.rs`
  - `crates/jbgit-core/src/stage_tracker.rs`
  - `crates/jbgit-engine/src/details.rs`
  - `crates/jbgit-engine/src/stage.rs`
  - `crates/jbgit-tui/src/syntax.rs`
  - `crates/jbgit-tui/src/app.rs`
  - `crates/jbgit-tui/src/app_stage.rs`
  - `crates/jbgit-tui/src/views/commit_diff.rs`
  - `crates/jbgit-tui/src/views/log_details.rs`
  - `crates/jbgit-tui/src/views/file_history.rs`
  - `crates/jbgit-tui/src/i18n/locales/zh-CN.toml`
  - `crates/jbgit-tui/src/i18n/locales/en-US.toml`

---

## 1. 背景与目标

实测表明，`HighlightedDiff::build`（基于 `syntect` `default-fancy` 回溯正则引擎）在 UI 主线程同步运行，是切换不同代码修改时耗时百毫秒乃至数秒级（Dev 下 2658 行 Full Diff 达 **6.3 秒**，Release 达 **542 ms**）的首要元凶；同时大文件无体积上限、压缩单行无长度拦截、`StageLineTracker` 逐行堆拷贝与线性查找 `ranges`、以及提前对全量 20,000 行计算 `O(N * M)` `word_diff` 进一步放大了首屏阻塞与内存开销。

本 Issue 聚焦 P0 底座治理：把 `syntect` 高亮彻底移出 UI 主线程、为 Dev 模式补齐三方重计算 crate 优化、建立大文件与超长单行安全护栏，并将 Diff 构建与行内渲染中的二次方/冗余分配热点重构为线性或按需计算。

---

## 2. 核心任务清单

- [x] **Dev 模式三方依赖优化配置（`Cargo.toml`）**：
  - 在工作区根 `Cargo.toml` 为纯 CPU 重计算三方 crate 添加优化配置（不影响 `jbgit-*` 自身增量编译）：
    - `[profile.dev.package.syntect] opt-level = 3`
    - `[profile.dev.package.fancy-regex] opt-level = 3`
    - `[profile.dev.package.imara-diff] opt-level = 3`
    - `[profile.dev.package.flate2] opt-level = 3`
    - `[profile.dev.package.tiny-skia] opt-level = 3`
- [x] **改造 `syntax.rs` 语法高亮管线（`crates/jbgit-tui/src/syntax.rs`）**：
  - 在应用启动阶段通过后台任务预热 `assets()`（`SyntaxSet::load_defaults_newlines()` 与 Darcula `Theme`），消除首次切文件 40–270 ms 的冷启动抖动。
  - 消除 `DiffLineKind::Context` 双重高亮：按 hunk 预扫描是否包含 `DiffLineKind::Deleted`，**仅当 hunk 存在删除行时才创建并推进 `hl_old`**，否则仅跑 `hl_new` 单流（覆盖全上下文/纯新增场景，直接减半 `Context` 行计算量）。
  - 增加单行高亮字节上限常量 `MAX_SYNTAX_LINE_BYTES: usize = 2048`：超过上限的行直接返回空 token（保留 `base` Diff 背景与词级差异背景），彻底杜绝压缩 JS/JSON/SVG 单行引发 `fancy-regex` 灾难性回溯卡死。
  - 新增协作式可取消高亮接口 `HighlightedDiff::build_cancellable(hunks: ..., key_path: &str, cancel: &AtomicBool) -> Option<Self>`，在每个 hunk/每批行间检查取消标志，避免 `as_file_diff()` 为高亮专门深拷贝整个 `ThreeDocStageDiff`。
  - 重构 `render_diff_line_spans_with_tab`（`syntax.rs:218-268`）：利用 `syntax_tokens` 与 `word_ranges` 的有序区间维护双指针游标单趟推进，将 `cuts.windows(2)` 内的 `O(tokens^2)` 线性查找降为 `O(tokens + word_ranges)`。
- [x] **全工作台语法高亮异步化与 `FileDiffLoaded` 过期 `epoch` 过滤（`app.rs`, `app_stage.rs`, `views/*.rs`）**：
  - 在 UI 主线程与引擎事件处理中彻底移除对 `HighlightedDiff::build` 的同步调用：
    - `crates/jbgit-tui/src/app.rs` `EngineEvent::FileDiffLoaded`：首先判断 `if epoch < self.latest_file_diff_epoch`，若当前没有 `Stash` / `FileHistory` / `Log` 等有效请求关注该 key 且已过期则尽早跳过冗余处理；收到有效 diff 时先立即写入 `diff_cache` 渲染无语法色纯文本版本，同时以独立异步/防覆盖任务投递后台语法高亮计算（附带 `syntax_epoch` 与取消标记）。
    - `crates/jbgit-tui/src/app_stage.rs` `EngineEvent::StageFileDiffLoaded`：立即存入 `current_diff` 使首屏 Diff 即刻可见，并将语法高亮计算投递到后台可取消任务，不再同步执行 `HighlightedDiff::build(&diff.as_file_diff())`。
    - `crates/jbgit-tui/src/views/commit_diff.rs`、`views/log_details.rs`、`views/file_history.rs`：渲染函数**绝不**调用 `HighlightedDiff::build` 兜底；当 `diff_syntax` 缓存暂未就绪时直接以无 token 方式渲染纯文本 Diff，待后台高亮完成后触发单次 `needs_redraw` 无闪烁着色；对超长 `Full` diff 可分块优先高亮当前视口周边或全量异步回填。
- [x] **大文件读取前体积安全护栏（`jbgit-core`, `jbgit-engine`, `jbgit-tui`）**：
  - 定义共享常量 `MAX_DIFF_FILE_BYTES: usize = 4 * 1024 * 1024`（4 MiB）。
  - 在 `crates/jbgit-engine/src/details.rs` 读取 `git2::Blob` 与工作区文件前，先检查 `blob.size()` / `std::fs::metadata(path).len()`，超限即停止读取完整字节流并返回带大文件/截断标记的空 hunk `FileDiff`。
  - 在 `crates/jbgit-engine/src/stage.rs` 的 `read_tree_entry`、`read_index_entry`、`read_worktree_file` 与 `load_stage_file_diff_sync` 读取 `HEAD` / `Index` / `Worktree` 三版本前检查体积上限，超限时安全降级返回不执行 `StageLineTracker::new` 与 diff 计算。
  - 在 Diff 预览区渲染清晰提示文案，并在 `zh-CN.toml` / `en-US.toml` 补齐对应 i18n 键。
- [x] **数据结构与算法线性化（`stage_tracker.rs`, `jbgit-core/src/details.rs`, `jbgit-engine/src/details.rs`）**：
  - `SlicedDocument`（`crates/jbgit-core/src/stage_tracker.rs:50-85`）：改为共享 `bytes: Arc<[u8]>` + 行区间索引 `Vec<LineSlice { start: usize, end: usize, ending: LineEnding }>`（或零拷贝视图），消除三文档按行 `.to_vec()` 的几十万次小堆分配；核对局部暂存/回滚回写 `to_bytes()` 语义完全等价。
  - `StageLineTracker::range_for_staged_line` / `range_for_local_line`（`stage_tracker.rs:361-380`）：利用 `self.ranges` 按行号有序不变量改为 `partition_point` 二分查找（复杂度从 `O(R)` 降至 `O(log R)`，并考虑 `build_interactive_hunks` 内部维护推进游标达均摊 `O(1)`）。
  - 消除全量 `word_diff` 预计算阻塞（`crates/jbgit-core/src/details.rs:462-521`、`crates/jbgit-engine/src/details.rs:490`）：不再在后台构建 diff 时对全部最多 20,000 行无差别提前跑 `O(N * M)` LCS DP，改为按需对可视窗口或受控行预算内的成对修改行懒计算并缓存。
- [x] **补充针对性单测与性能回归基准**：
  - `StageLineTracker`：构造 10,000 行 / 1,000 个分散修改 hunk 测试集，校验新版二分/游标 `range_for_*` 与原线性实现输出逐项一致，且 `SlicedDocument` CRLF/LF/无尾换行还原字节逐比特相等。
  - `HighlightedDiff`：覆盖单行 `20 KB` 压缩字符串（必须跳过高亮不卡顿）、纯 `Context/Added` hunk 单流与混合 `Deleted` hunk 多流高亮结果一致性、以及取消标志中途置位后快速退出。
  - 大文件拦截测试：构造 `> MAX_DIFF_FILE_BYTES` 临时 blob / 工作区文件，验证 `load_file_diff_sync` 与 `load_stage_file_diff_sync` 跳过全文读取并正确标记。

---

## 3. 验收标准 (Acceptance Criteria)

- [x] `cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings` 全部通过。
- [x] 在 UI 渲染函数及事件处理主循环链路中搜索 `HighlightedDiff::build`，零同步阻塞调用残留。
- [x] 切换 2,500+ 行 Rust 文件的 `Full` Diff 时，首屏纯文本 diff 立即呈现不卡死 UI，语法高亮随后自动刷新，且切换下一文件时前序未完成高亮任务立即停止。
- [x] 预览超过 `4 MiB` 的文本大文件或单行超过 `2048 B` 的压缩 JS/JSON 文件时，界面保持流畅响应（`< 20 ms` 降级提示），不发生 OOM 或正则回溯假死。
- [x] `git` 仓库中仅包含新增优化与测试，不破坏现有局部 hunk/逐行暂存、Diff 渲染颜色与 `word_diff` 视觉效果。

---

## Comments

- 2026-11-21 (agent): 基于本机 `jbgit-pilot` 2×2 基准测试（Dev Full Diff 6.3s → 关高亮 112ms；Release Full Diff 542ms → 83ms）与源码审计创建本 P0 工单，作为 Spec 10 性能优化的底座任务。
- 2026-11-21 (agent): Issue 01 全量完成并验证：
  1. 异步高亮架构上线，生产渲染流同步 `HighlightedDiff::build` 完全清零；
  2. 4 MiB 大文件体积守卫与 2048 B 单行高亮安全拦截；
  3. `SlicedDocument` 连续切片与二分查找行追踪器；
  4. 预算制/按需 `word_diff`。全测试通过。
