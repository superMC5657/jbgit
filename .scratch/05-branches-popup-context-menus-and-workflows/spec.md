# Spec 05: 分支浮窗、右键上下文菜单与高阶 Git 工作流规约

- **目录**：`.scratch/05-branches-popup-context-menus-and-workflows/`
- **下属 Issues**：
  - [`01-branches-popup-and-speed-search.md`](./issues/01-branches-popup-and-speed-search.md)
  - [`02-context-menu-framework-and-mouse-ux.md`](./issues/02-context-menu-framework-and-mouse-ux.md)
  - [`03-advanced-git-ops-and-conflict-state-banner.md`](./issues/03-advanced-git-ops-and-conflict-state-banner.md)
- **上游参考源码**：
  - `reference/plugins/git4idea/backend/src/ui/branch/GitBranchPopupActions.java`（分支浮窗条目、未合并删除保护、`↑↓` 提示）
  - `reference/plugins/git4idea/shared/resources/intellij.vcs.git.shared.xml`（`Git.Branches.Popup.Settings`、`Git.Branch` 动作组）
  - `reference/plugins/git4idea/backend/resources/intellij.vcs.git.backend.xml`（`Git.Log.ContextMenu`、`Git.Stage.Tree.Menu`、`Git.Ongoing.Rebase.Actions`、`Git.Branch.Backend`）
  - `reference/platform/vcs-log/impl/resources/intellij.platform.vcs.log.impl.xml`（`Vcs.Log.ContextMenu`）

---

## 1. `GitBranchesPopup` 全局分支浮窗规范 (`01-branches-popup-and-speed-search`)

对标 IDEA 标志性的 **`Git Branches` 浮窗**（快捷键 `Ctrl+Shift+\`` / `Alt+B` 全局呼出，或鼠标左键点击顶栏当前分支胶囊 `⎇ main ↑2 ↓1` 触发）：

### 1.1 浮窗视觉结构与级联双栏设计

```text
           ┌─ Git Branches (jGit) ────────────────────────── [↻ Fetch] [⚙] ─┐
           │ 🔍 feat                                                   [Esc] │
           ├─────────────────────────────────────────────────────────────────┤
           │ ⚠ Rebase in progress: Continue │ Abort │ Skip   (仅进行中状态显示)│
           │ + New Branch…                                                   │
           │ ⌖ Checkout Tag or Revision…                                     │
           ├─────────────────────────────────────────────────────────────────┤
           │ ▼ Recent                                                        │
           │   ★ main                                      origin/main ↑2 ↓1 │
           │ ▼ Local Branches                                                │
           │   ▼ feature                                                     │
           │   ▎ ★ [feat]ure/dag-layout                 origin/feat.. ↑1   ▸ ├───┬─ 'feature/dag-layout' ──────────────┐
           │       [feat]ure/stage-tracker                                 ▸ │   │ Checkout                            │
           │ ▼ Remote Branches                                               │   │ New Branch from 'feature/dag-la…'…  │
           │     ☁ origin/[feat]ure/dag-layout                             ▸ │   │ Checkout and Rebase onto 'main'     │
           └─────────────────────────────────────────────────────────────────┘   ├─────────────────────────────────────┤
                                                                                 │ Compare with 'main'                 │
                                                                                 │ Show Diff with Working Tree         │
                                                                                 ├─────────────────────────────────────┤
                                                                                 │ Rebase 'main' onto 'feature/dag…'   │
                                                                                 │ Merge 'feature/dag…' into 'main'    │
                                                                                 ├─────────────────────────────────────┤
                                                                                 │ Update / Pull                       │
                                                                                 │ Push…                               │
                                                                                 ├─────────────────────────────────────┤
                                                                                 │ Rename…                             │
                                                                                 │ Delete                              │
                                                                                 └─────────────────────────────────────┘
```

### 1.2 核心交互流细节
1. **零延迟模糊速查（`nucleo` Speed-Search）**：
   - 浮窗打开即处于可键入过滤状态（无需先点搜索框），键入 `fdl` 即可模糊命中 `feature/dag-layout`，匹配字符在分支名中以 `#548AF7` 加粗高亮！
   - `Up` / `Down` 跳过分组标题行仅在可执行条目间移动。
2. **近期检出分支（`Recent Branches`）**：
   - 对标 `git.recent.checkout.branches.reflog.entries.count = 50`，解析 `.git/logs/HEAD` 中形如 `checkout: moving from X to Y` 的最近 5 个唯一本地分支（排除当前已检出分支），实现一秒切回上一个工作分支！
3. **二级动作子菜单（对标 `GitBranchPopupActions` & `Git.Branch.Backend`）**：
   - 按 `Enter` 或 `Right` (`→`) 或鼠标点击分支行展开右侧级联动作菜单：
     - **本地分支子菜单**：`Checkout`、`New Branch from 'X'…`、`Checkout and Rebase onto Current`、`Compare with Current`（自动跳转 Log 视图设置双向差异过滤）、`Show Diff with Working Tree`、`Rebase Current onto 'X'`、`Merge 'X' into Current`（若非受保护分支且合入成功，提示是否清理已合并分支）、`Update`、`Push…`、`Rename…`、`Delete`。
     - **远程分支子菜单**：`Checkout`（智能跟踪逻辑：若本地尚无同名分支则自动执行 `git checkout -b <local> --track origin/<local>`；若本地已有同名分支且偏离，则弹窗询问重置或新建名称，完全对标 `GitRemoteBranchCheckoutUtil`！）、`New Branch from 'origin/X'…`、`Compare with Current`、`Merge into Current`、`Rebase Current onto 'origin/X'`、`Delete Remote Branch…`。

---

## 2. 通用上下文菜单引擎与鼠标/键盘交互规范 (`02-context-menu-framework-and-mouse-ux`)

### 2.1 触发方式双轨合一
- **鼠标右键触发**：在任意区域按下 `MouseButton::Right`，通过 `HitTestRegistry` 定位光标下具体对象 -> 自动将该对象设为选中态（若已在多选集合内则保留整个多选集合作为上下文参数！） -> 在光标坐标 `(x, y)` 打开对应的上下文菜单。
- **纯键盘等价触发**：按下 `Menu` 键、`Shift+F10`、`m` 或 `.`（非文本输入焦点时），在当前聚焦组件选中行的中心偏右坐标锚定弹出完全相同的上下文菜单！

### 2.2 视口边界自动翻转与无障碍设计
1. **边缘碰撞自适应（Viewport Flipping）**：
   - 若 `x + menu_width > terminal_width`，菜单向左翻转为 `x.saturating_sub(menu_width)`；
   - 若 `y + menu_height > terminal_height`，菜单向上翻转为 `(y + 1).saturating_sub(menu_height)`；
   - 级联二级子菜单同理：右侧空间不足时自动在父菜单左侧展开。
2. **视觉与交互体验**：
   - 采用 `#313438` 悬浮底色 + 细线圆角边框 `╭─╮` + 右下 1 格半透明暗色阴影装饰；
   - 每项左侧显示图标，中间显示动作名（带下划线助记符，如 `C̲herry-Pick` 按 `c` 直接执行），右侧对齐快捷键提示灰字（如 `Ctrl+C` 或 `▸` 表示含子菜单）；
   - 当前上下文不满足前置条件的菜单项（例如对非当前分支祖先提交执行 `Reword`，或存在未解决冲突时执行 `Rebase`）**置灰禁用并保留可见**，悬停/聚焦时在底部显示禁用原因说明（如 `Cannot reword: commit is already pushed to protected branch or not reachable from HEAD`）。

---

## 3. 三大核心场景上下文菜单全景清单

### 3.1 提交日志行右键菜单（对标 `Git.Log.ContextMenu` + `Vcs.Log.ContextMenu`）

| 分组 | 菜单项名称 | 快捷键 / 助记符 | 适用条件与行为说明 |
| :--- | :--- | :---: | :--- |
| **检出与对比** | `Checkout Revision 'a8f3d21'` | `c` | 若该提交上有本地分支，提供子菜单选择检出分支或 Detached HEAD |
| | `Compare with Local` | `d` | 对比所选提交与当前工作区差异 |
| | `Compare Versions` | `Ctrl+D` | 当多选恰好 2 个提交时激活，对比两提交快照净差异 |
| **历史拣选与回退** | `Cherry-Pick` | `p` | 将选中（单/多）提交按拓扑从旧到新依次拣选至当前分支 |
| | `Revert Commit` | `v` | 执行 `git revert --no-edit`（冲突时转入冲突解决流） |
| | `Reset Current Branch to Here…` | `r` | 弹出 **IDEA 四模式 Reset 对话框**（见 4.1 节） |
| | `Uncommit` | `u` | 仅在当前 `HEAD` 提交且非根提交时可用：等价于 `reset --soft HEAD~1` 并将原提交消息自动填回暂存区编辑框！ |
| **变基编辑** | `Reword Commit…` | `F2` / `w` | 弹出多行编辑框直接修改该历史提交的 Commit Message |
| | `Squash Into…` / `Squash Commits…`| `s` | 多选连续/同线提交压缩合并为单个提交并编辑合并消息 |
| | `Fixup…` | `f` | 基于当前暂存更改生成 `fixup!` 提交或直接交互式折叠进目标提交 |
| | `Drop Commit(s)` | `Del` | 在交互式变基中丢弃选中提交（带确认框） |
| | `Interactive Rebase from Here…` | `i` | 打开可视化 Rebase Todo 排序编辑弹窗（支持 `Up/Down` 调序、`Pick/Reword/Squash/Fixup/Drop` 切换） |
| | `Push All up to Here…` | — | 执行 `git push origin <sha>:refs/heads/<branch>` 仅推送到此提交为止 |
| **分支与标签** | `Branch '<name>' ▸` | `b` | 当该提交挂载了分支 Pill 时，内联展开该分支的全部子菜单动作 |
| | `New Branch…` | `Ctrl+Alt+N` | 以所选提交为起点创建新分支（可选立即 Checkout） |
| | `New Tag…` | `t` | 创建轻量或附注（Annotated）标签 |
| **辅助与跳转** | `Copy Revision Number` | `y` | 复制完整 40 位 SHA 到系统剪贴板（通过 OSC 52 + 系统剪贴板双保险） |
| | `Create Patch…` | — | 导出选中提交 `.patch` 文件或复制到剪贴板 |
| | `Go to Child` / `Go to Parent` | `←` / `→` | 沿 DAG 轨道上下跳转 |

### 3.2 变更暂存树右键菜单（对标 `Git.Stage.Tree.Menu`）

| 分组 | 菜单项名称 | 行为说明 |
| :--- | :--- | :--- |
| **冲突处理** | `Resolve Conflicts…` / `Accept Yours` / `Accept Theirs` | 仅对 `Merge Conflicts` 节点显示；支持一键采纳我方/对方或打开三栏合并视图 |
| **暂存流转** | `Stage` (`+`) / `Stage Without Content` (`git add -N`) | 对 `Unstaged / Untracked` 文件执行暂存或仅注册路径意图（方便后续局部 Hunk 暂存新文件！） |
| | `Unstage` (`-`) | 对 `Staged` 文件或目录执行重置回未暂存区 |
| | `Rollback / Discard Changes…` | 二次确认后丢弃选中文件的未暂存修改（Untracked 文件提示删除确认） |
| | `Stash Selected Files…` | 仅对选中的路径子集执行 `git stash push -- <paths>` |
| **忽略与补丁** | `Add to .gitignore` / `.git/info/exclude` | 自动追加相对路径或通配符规则至 `.gitignore` |
| | `Show Diff` / `Three-Side Diff` | 打开双栏或 `HEAD ↔ Staged ↔ Local` 三栏对比视图 |
| | `Create Patch to Clipboard` | 将选中变更生成标准 Unified Patch 写入剪贴板 |
| | `Copy Relative Path` / `Delete File…` | 复制仓库相对路径或安全删除文件 |

---

## 4. 高阶 Git 模态对话框与异常状态恢复工作流 (`03-advanced-git-ops-and-conflict-state-banner`)

### 4.1 `GitResetDialog` 四模式重置弹窗（精准对齐 IDEA 默认安全策略）
当触发 `Reset Current Branch to Here…` 时，弹出带单选组的模态对话框（默认选中 **`Mixed`** 或 **`Keep`**）：
1. **`( ) Soft`** (`--soft`)：文件与暂存区均不改动，原提交差异全部进入 `Staged`。
2. **`(•) Mixed`** (`--mixed`)：工作区文件不改动，暂存区重置，原提交差异进入 `Unstaged`。
3. **`( ) Hard`** (`--hard`，红色风险警示徽标)：彻底丢弃工作区与暂存区所有未提交改动！
4. **`( ) Keep`** (`--keep`)：重置分支并更新已提交文件，但**智能保留当前未触及的本地未提交更改**；若本地更改与重置目标冲突则安全中止绝不丢代码（IDEA 招牌安全模式）！

### 4.2 本地修改自动暂存保护（Smart Checkout / Smart Rebase Saver）
对标 IDEA `GitPreservingProcess` / `GitBranchCheckoutOperation`：
- 当用户执行 `Checkout`、`Pull`、`Rebase` 或 `Merge` 因本地存在未提交冲突修改而被 Git 拒绝（`Your local changes to the following files would be overwritten by checkout`）时：
- **绝不只甩出一行生硬报错**，而是弹出 **Smart Operation 恢复对话框**：
  - `[ Smart Checkout / Smart Merge ]`：自动执行 `git stash push -u` -> 执行目标 Checkout/Rebase -> 自动 `git stash pop` 恢复本地修改！
  - `[ Force Checkout (Discard Local) ]`：丢弃冲突修改强制检出。
  - `[ Cancel ]`：取消操作。

### 4.3 进行中多步操作全局状态横幅 (`OngoingRepositoryStateBanner`)
对标 `GitMergeRebaseWidget` 与 `Git.Ongoing.Rebase.Actions`：
- `jgit-engine` 实时检测 `.git/rebase-merge`、`.git/rebase-apply`、`.git/MERGE_HEAD`、`.git/CHERRY_PICK_HEAD`、`.git/REVERT_HEAD`：
- 一旦处于中间态，在主界面顶栏下方点亮醒目的琥珀色横幅：
  ```text
  ⚠ REBASING feat/dag onto main (Step 2/5) — 1 conflicted file   [ Resolve ] [ Continue ] [ Skip ] [ Abort ]
  ```
- 用户无论身处 `Commit` 还是 `Log` 标签页，均可一键解决冲突、继续（`Continue`）、跳过当前补丁（`Skip`）或安全中止回退到操作前状态（`Abort`）！

### 4.4 `[3 Stash]` 储藏箱工作台标签页
对标 `git4idea/stash/ui`（`intellij.vcs.git.backend.xml` 第 548-577 行）：
- 左侧列出全部 `stash@{0}: On main: message` 列表，右侧直接复用 `CommitChangesBrowser` + `DiffPreview` 预览该 Stash 内每个文件的改动明细；
- 支持 `Apply`、`Pop`、`Unstash as New Branch…`（`git stash branch`）、`Drop`、`Clear All Stashes…`。
