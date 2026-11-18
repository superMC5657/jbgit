# Spec 04: 变更暂存工作台、三文档行追踪与局部暂存规约

- **目录**：`.scratch/04-staging-workbench-and-partial-hunk/`
- **下属 Issues**：
  - [`01-staging-dual-split-and-changes-tree.md`](./issues/01-staging-dual-split-and-changes-tree.md)
  - [`02-three-doc-line-tracker-and-partial-staging.md`](./issues/02-three-doc-line-tracker-and-partial-staging.md)
  - [`03-commit-editor-amend-and-commit-workflow.md`](./issues/03-commit-editor-amend-and-commit-workflow.md)
- **上游参考源码**：
  - `reference/plugins/git4idea/backend/src/index/ui/GitStagePanel.kt`（暂存区主面板、树与提交框 `TwoKeySplitter`、内嵌 Diff 分屏）
  - `reference/plugins/git4idea/backend/src/index/ui/GitStageTree.kt`（`NodeKind`: `CONFLICTED`, `STAGED`, `UNSTAGED`, `UNTRACKED`, `IGNORED` 及操作映射）
  - `reference/plugins/git4idea/backend/src/index/GitStageLineStatusTracker.kt`（`vcsDocument` / `stagedDocument` / `document` 三文档模型、`BlockMerger`、`StagedRange` 局部暂存与回滚算法）
  - `reference/platform/vcs-impl/src/com/intellij/vcs/commit/AmendCommitHandlerImpl.kt`（非模态 Amend 修正模式与上次提交信息回填）

---

## 1. 变更暂存工作台整体双重分屏布局

对标 IDEA Git Staging Area（`GitStagePanel` + `ShelveSplitterDiffPreview`），`[1 Commit]` 标签页采用经典的 **左窄右宽 + 左栏上下双重分屏** 架构：

```text
┌───────────────────────────────────────┬─────────────────────────────────────────────────────────┐
│ Left-Top: GitStageTree (62% Height)   │ Right Split: Interactive Stage Diff Workbench (65% W)   │
│ Toolbar: [↻] [+ Tracked] [📦 Stash][⚙]│ Header: src/engine/index.rs (Unstaged ↔ Index ↔ HEAD)   │
│                                       │ Mode: [Unified ▾] [Context: 3▾] [Prev ↑] [Next ↓]       │
│ ▼ Merge Conflicts (1)   [Resolve…]    ├─────────────────────────────────────────────────────────┤
│   ! src/merge.rs      Both Modified   │ @@ -42,7 +42,9 @@ pub fn apply_hunk(...)     [+ Stage]  │
│ ▼ Staged (2)                          │                                       [↺ Rollback Hunk] │
│   ▼ src/graph                         │  42  42 │     let mut idx = repo.index()?;              │
│     M layout.rs             [- Unstage│  43     │ -   idx.write()?;                             │
│     A raster.rs             [- Unstage│      43 │ +   idx.add_frombuffer(&entry, &buf)?;        │
│ ▼ Unstaged (3)                        │      44 │ +   idx.write()?;                             │
│     M src/engine/index.rs   [+ Stage] │▌ 44  45 │ +   tracing::debug!("index updated");         │
│     ? docs/draft.md         [+ Stage] │  45  46 │     Ok(())                                    │
├───────────────────────────────────────┤                                                         │
│ Left-Bottom: Commit Panel (38% Height)│  [Space / s] Stage Hunk/Lines   [u] Unstage             │
│ [ ] Amend  [ ] Sign-off  [x] Hooks [🕘]│  [r] Rollback Unstaged          [v] Visual Line Select  │
│ ┌───────────────────────────────────┐ │  [Tab] Switch Side              [e] Open in $EDITOR     │
│ │feat(stage): add line-level staging│ │                                                         │
│ │                                 34│ │                                                         │
│ │Implements 3-doc BlockMerger.      │ │                                                         │
│ └───────────────────────────────────┘ │                                                         │
│ [ Commit (Ctrl+Enter) ] [Commit&Push▾]│                                                         │
└───────────────────────────────────────┴─────────────────────────────────────────────────────────┘
```

---

## 2. 左上分屏：`GitStageTree` 分类状态树 (`01-staging-dual-split-and-changes-tree`)

严格对标 `GitStageTree.kt` 第 426-457 行的 `NodeKind` 枚举与排序权重 `sortOrder`：

### 2.1 五大顶层分组节点（按固定优先级排列）
1. **`Merge Conflicts` (`NodeKind::Conflicted`, 权重 0)**：
   - 仅当存在未解决冲突（Index 含 stage 1/2/3 条目，如 `Both Modified`、`Deleted by Them`、`Added by Both`）时置顶显示，右侧附带高亮动作 `[Resolve…]`。
2. **`Staged` (`NodeKind::Staged`, 权重 1)**：
   - 对比 `HEAD Tree ↔ Index` 存在差异的条目（`IndexNew`、`IndexModified`、`IndexDeleted`、`IndexRenamed`）。
3. **`Unstaged` (`NodeKind::Unstaged`, 权重 2)**：
   - 对比 `Index ↔ Worktree` 存在差异的已跟踪文件；
   - **部分暂存双出现机制（Dual Presence）**：若同一文件 `foo.rs` 既有已暂存改动（Hunk 1），又有未暂存改动（Hunk 2），则 `foo.rs` **同时出现在 `Staged` 与 `Unstaged` 两个分组下**！选中 `Staged/foo.rs` 时右侧聚焦 `HEAD ↔ Index` 差异；选中 `Unstaged/foo.rs` 时右侧聚焦 `Index ↔ Worktree`（并融合三方行状态标记）！
   - **Untracked 文件归并策略**（对标 `GitStageTree.kt` 第 276-291 行 `createUntrackedNode`）：当未跟踪文件数量 ≤ 50 时，直接以红褐色 `?` 混排在 `Unstaged` 树下以便一键 `+` 暂存；当超过阈值（如新建了未忽略目录含数百文件）时，收拢为独立的折叠节点 `▶ Untracked (N files)` 防止刷屏。
4. **`Ignored` (`NodeKind::Ignored`, 权重 4，默认隐藏)**：
   - 通过工具栏 `[⚙ -> Show Ignored Files]` 开启后按需展示。

### 2.2 树交互快捷键与鼠标热区
- **悬停快捷图标（对标 `GitStageHoverIcon`）**：鼠标悬停在 `Unstaged` 文件或文件夹行右侧显示绿色 `[+]`，点击立即 Stage 该节点下所有文件；悬停在 `Staged` 节点显示 `[-]`，点击立即 Unstage！
- **键盘极速操作**：
  - `Space` 或 `s` / `+`（在 `Unstaged` 节点上）：Stage 选中文件或整个子目录；
  - `Space` 或 `u` / `-`（在 `Staged` 节点上）：Unstage 选中文件或整个子目录；
  - `Ctrl+Alt+Z` 或 `d`（带二次确认）：Rollback / Discard 未暂存更改（已跟踪文件恢复至 Index 内容，或可选删除新建 Untracked 文件）。

---

## 3. 核心算法：三文档行状态追踪与内存级局部暂存 (`02-three-doc-line-tracker-and-partial-staging`)

这是整个变更暂存工作台技术含量最高的核心模块，直接对标 `GitStageLineStatusTracker.kt`（第 96-320、908-1018 行）。

### 3.1 为什么不能只做简单的 `git apply --cached` 文本拼接？
传统的手工构造 unified diff patch 字符串喂给 `git apply --cached --unidiff-zero` 在遇到 CRLF/LF 混合换行符、文件末尾无换行符（`\ No newline at end of file`）、同一区域既有已暂存行又有未暂存二次修改时极易报 `patch does not apply` 错误！

IDEA `GitStageLineStatusTracker` 采用 **纯内存三文档双差分块合并模型（Three-Document Dual-Diff Block Model）**，数学上 100% 严密，零外部进程调用，天然免疫换行符与上下文偏移错误！

### 3.2 三文档与 `StagedRange` 数据模型 (`jgit-core::stage`)

对于任意选中的文本文件，从 `git2` 一次性加载三份行切片文档（保留每行原始 `\n` 或 `\r\n` 结尾字节元数据）：
1. **`vcs_doc` (`HEAD`)**：从 `HEAD` Tree 读取该路径（若发生重命名则读 `orig_path`）的 Blob；若为新增文件则为空文档 `[]`。
2. **`staged_doc` (`Index`)**：从 `git2::Index` 读取该路径当前的暂存区 Blob。
3. **`local_doc` (`Worktree`)**：从磁盘工作区读取当前文件内容。

使用 `imara-diff` 分别计算两组精确无损差分块（`DiffBlock { start1, end1, start2, end2 }`，左闭右开行号区间）：
- **`staged_blocks`**：`diff(vcs_doc, staged_doc)`
- **`unstaged_blocks`**：`diff(staged_doc, local_doc)`

随后运行对标 `BlockMerger`（`GitStageLineStatusTracker.kt` 第 922-1018 行）的线性归并器，以 `staged_doc` 的行坐标系为公共枢轴，输出统一融合区间列表 `Vec<StagedRange>`：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedRange {
    /// 在工作区文档 (local_doc) 中的起止行号 [line1, line2)
    pub local_range: Range<usize>,
    /// 在暂存区文档 (staged_doc) 中的起止行号 [staged_line1, staged_line2)
    pub staged_range: Range<usize>,
    /// 在 HEAD 文档 (vcs_doc) 中的起止行号 [vcs_line1, vcs_line2)
    pub vcs_range: Range<usize>,
    /// 该变更块是否包含已暂存差异 (HEAD != Index)
    pub has_staged: bool,
    /// 该变更块是否包含未暂存差异 (Index != Worktree)
    pub has_unstaged: bool,
}
```

### 3.3 Hunk 级与单行级（Visual Line Selection）精确操作算法

#### 操作一：局部暂存（Stage Hunk 或 Stage Selected Lines `[sel_start..sel_end)`）
对标 `GitStageLineStatusTracker.runBulkStage`（第 295-302 行）：
- **Hunk 级暂存**：对于目标 `unstaged_block`（其将 `staged_doc[s1..s2]` 替换为 `local_doc[l1..l2]`），直接在内存中构造新的 `new_staged_doc`：将 `staged_doc` 的 `s1..s2` 区间替换为 `local_doc[l1..l2]`，其余行保持 `staged_doc` 原样！
- **单行 / 多行局部选择暂存（Fine-Grained Line Staging）**：
  当用户在 Diff 视图进入行选择模式（`v` 键拖选或光标单行操作）仅选中一个 Hunk 内的部分 `+` 新增行或部分 `-` 删除行集合 $S$ 时：
  1. 遍历该 Hunk 内的逐行操作序列（`Keep(line)`、`Delete(staged_line_idx)`、`Insert(local_line_idx)`）：
     - 未被选中的 `Delete(staged_line_idx)`：**保留**在 `new_staged_doc` 中（因为用户不想暂存这行的删除！）；
     - 被选中的 `Delete(staged_line_idx)`：**从 `new_staged_doc` 中剔除**（应用删除到 Index！）；
     - 被选中的 `Insert(local_line_idx)`：**写入 `new_staged_doc`**（应用新增到 Index！）；
     - 未被选中的 `Insert(local_line_idx)`：**跳过不写入 `new_staged_doc`**（留在工作区作为未暂存新增！）。
  2. 将拼装好的 `new_staged_bytes` 直接通过 `git2::Index` 落盘：
     ```rust
     let mut index = repo.index()?;
     let mut entry = existing_index_entry_or_default(&index, path, file_mode);
     // add_frombuffer 自动将字节流写入 .git/objects 生成新 Blob Oid 并更新 Index 条目！
     index.add_frombuffer(&entry, &new_staged_bytes)?;
     index.write()?;
     ```
  - **关键优势**：工作区文件 `local_doc` **一个字节都不会被触碰**（编辑器不会触发外部重加载报警，未暂存代码毫发无损），且全流程在内存中完成，耗时 < 5ms！

#### 操作二：局部取消暂存（Unstage Hunk 或 Unstage Selected Lines）
对标 `GitStageLineStatusTracker.runBulkUnstage`（第 309-317 行）：
- 在 `staged_blocks`（`vcs_doc ↔ staged_doc`）上执行逆向局部替换：
  - 被选中的已暂存 `Insert(staged_line_idx)` 从 `new_staged_doc` 移除；
  - 被选中的已暂存 `Delete(vcs_line_idx)` 从 `vcs_doc` 恢复写回 `new_staged_doc`；
  - 调用 `index.add_frombuffer(&entry, &new_staged_bytes)` + `index.write()`。
- 工作区文件同样保持不变，被取消暂存的改动自动无缝转移到 `Unstaged` 视图中！

#### 操作三：局部回滚工作区未暂存块/行（Rollback Unstaged Hunk / Lines）
对标 `GitStageLineStatusTracker.runBulkRollback`（第 280-287 行）：
- 在 `unstaged_blocks`（`staged_doc ↔ local_doc`）上将选中的未暂存更改还原为 `staged_doc` 对应行，原子写回工作区磁盘文件（破坏性操作：单行/单 Hunk 回滚支持内存级一层 `Undo` 撤销栈防止手滑误删！）。

### 3.4 右侧交互式 Diff 工作台视觉细节
- **三态边栏指示条（对标 `GitStageLineStatusTracker.paintStageLines` 第 424-514 行）**：
  - 纯未暂存改动行：实心彩色竖条 `▌`（绿 `#629755` 新增 / 蓝 `#6897BB` 修改 / 红三角 `▸` 删除点）；
  - 纯已暂存改动行：细线空心框体竖条 `▏`；
  - 同一区域既有已暂存又有未暂存改动：双色复合指示条，点击或按 `i` 可弹出 **三方迷你对照卡片**（同时展示 `Worktree`、`Staged`、`HEAD` 三段文本及词级高亮，完全复刻 `GitStageLineStatusTracker.showHintAt` 第 575-611 行！）。
- **Hunk 头部内联动作栏**：每个 `@@ ... @@` 头部右侧直接渲染可鼠标点击的胶囊按钮：`[+ Stage Hunk (s)]`、`[- Unstage Hunk (u)]`、`[↺ Rollback (r)]`。

---

## 4. 左下分屏：Commit 提交面板与 Amend 工作流 (`03-commit-editor-amend-and-commit-workflow`)

对标 `GitStageCommitPanel`、`CommitOptionsPanel` 与 `NonModalAmendCommitHandler`：

1. **多行提交信息编辑器 (`CommitMessageEditor`)**：
   - 支持完整的 UTF-8 / CJK 输入、Bracketed Paste 多行粘贴、`Undo/Redo`；
   - **规范标尺提示**：第 1 行（Subject）右侧实时显示字符计数（超过 50 字符变柔和黄提示，超过 72 字符高亮提醒），第 2 行非空时温和提示空行分隔最佳实践；
   - **提交信息历史浮窗（`Ctrl+M` 或点击 `[🕘]` 图标）**：保存最近 20 条本地提交草稿及仓库近期提交消息，一键回填。
2. **核心提交选项栏**：
   - **`[ ] Amend` (`Alt+M`)**：
     - 勾选瞬间自动保存当前未发送草稿，读取 `HEAD` 提交的完整 message 回填至编辑框；
     - 同时在左上 `GitStageTree` 中展示虚拟参考节点 `▶ Head Commit to Amend (a8f3d21)`（对标 `GitStagePanel.kt` 第 407 行 `insertEditedCommitNode`），让用户直观看到即将合并进上一次提交的完整文件清单！取消勾选 `Amend` 时自动恢复原草稿。
   - **`[ ] Sign-off` (`Alt+S`)**：提交时自动追加符合规范的 `Signed-off-by: Name <email>`。
   - **`[x] Run Git Hooks`**：取消勾选时向 `git commit` 传递 `--no-verify`（对标 `GitSkipHooksCommitHandlerFactory`）。
   - **`Author` 覆写输入（可选展开）**：支持 `--author="Name <email>"` 代提交。
3. **主操作按钮组**：
   - **`[ Commit (Ctrl+Enter) ]`**：
     - 预检：若 `Staged` 为空且非 `Amend` 模式，提示是否启用 `Stage All Tracked & Commit`（对标 `git.stage.enable.commit.all`）或阻止空提交；若存在 `Merge Conflicts` 未解决，禁止提交并高亮冲突分组。
     - 执行提交并在底部状态栏弹出非阻塞成功 Toast（附 `[Undo Commit]` 快捷链接，对应 `Git.Uncommit`）。
   - **`[ Commit and Push… (Ctrl+Alt+K) ]`**（对标 `GitCommitAndPushExecutorAction`）：提交成功后无缝衔接打开 Push 确认/执行流。
