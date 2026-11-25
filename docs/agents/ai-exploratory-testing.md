# AI Exploratory TUI Testing (`jbgit-pilot`)

`jbgit-pilot` runs the real `jbgit` binary inside a **Windows ConPTY / Unix PTY** backed by a `vt100` terminal emulator, giving AI agents live eyes (exact screen text, cursor position, RGB colors, text search) and hands (real keystrokes, UTF-8 text input, SGR mouse clicks/scroll, terminal resize) to explore the app interactively and uncover unimplemented features or bugs.

## 1. Build Prerequisites

```powershell
# Build jbgit (if target/debug/jbgit.exe is locked by an already-open window,
# pass `--target-dir tools/jbgit-pilot/target`)
cargo build -p jbgit-tui

# Build the pilot driver (workspace member)
cargo build -p jbgit-pilot
```

Binary path: `.\target\debug\jbgit-pilot.exe`

## 2. Pre-built Sandbox Scenarios (Zero Git Setup Needed)

Launch the PTY server **with `IsDaemon: true`** so it persists across tool calls (all runtime/sandbox files live under `.pilot/`, keeping `.scratch/` untouched):

### Scenario A: Full Sandbox (`--sandbox` / `--scenario full`, default)
Creates `.pilot/sandbox-repo` + bare remote `.pilot/sandbox-remote.git` with:
- Multi-branch DAG with merge commit (`main` ahead of `origin/main` by 3 commits, `feature/login`, `feature/ui-cards`, `bugfix/scroll-edge`, remote-only `origin/feature/remote-only`, pushed tag `v0.1.0` + unpushed annotated tag `v0.2.0-rc1`)
- **Linear collapsible chain**: `feature/linear-chain` adds 5 consecutive single-parent commits (`chore(linear): step 1..5`) for testing linear branch collapse/expand (Spec 12 #12)
- **Renamed & multi-commit file history**: `src/legacy_auth.rs` → `src/auth.rs` (4 commits on `main` across `git mv`, plus nested `src/components/header.rs` for directory history / path filtering)
- **Remote sync branches**:
  - `feature/pull-me` (remote `origin/feature/pull-me` has 2 un-fetched peer commits → test `f` Fetch showing `↓2`, then checkout & `p` Pull)
  - `feature/diverged` (diverged from `origin/feature/diverged` → test `P` Push dialog / Push Rejected / Force-with-lease / Rebase retry)
- **Patch fixtures**: `.pilot/sample.patch` (cleanly applies to `src/cards.rs`) and `.pilot/conflict.patch` (targets `src/components/header.rs` with a real ancestor index blob so plain `git apply --check` fails and `ThreeWay` (`git apply --3way`) enters the conflict-resolution flow) for Spec 12 #13 (paths are also exported in `.pilot/ready.json`)
- **Spec 13 Diff & Compare fixtures**: `assets/logo.bin` (uncommitted binary modification for Binary file diff card), `src/wide_lines.rs` (uncommitted indentation-only change + ~170-col line edit for whitespace modes, horizontal scroll / soft-wrap, and inline diff search), and `feature/cherry-source` (cherry-picked `feature/login` + `src/cherry_helper.rs` for two-branch compare & cherry-pick highlight)
- 2 real stashes (`wip: refactor stash test`, `wip: older stash`)
- 1 partially staged file (`staged_partial.txt`), 1 multi-hunk modified file (`src/main.rs`), 1 untracked file (`scratch_note.txt`), 1 ignored file (`debug_trace.log` via `.gitignore`)
- Isolated empty config dir (does not touch user global config)

```powershell
.\target\debug\jbgit-pilot.exe serve --sandbox --cols 140 --rows 40
```

### Scenario B: Merge Conflict (`--scenario conflict`)
Creates `.pilot/sandbox-repo` in an active **`git merge` conflict** state with a multi-file conflict queue (Spec 13 #02 / #09):
- `conflict_app.txt` (`UU`, contains both a non-overlapping block modified only by `theirs` for 3-way auto-merge and a true conflict block for inline Result editing)
- `conflict_second.txt` (`UU`, second file in the Conflict Queue)
- `deleted_by_theirs.txt` (`UD`, delete/modify conflict where `feature/theirs` removed the file and `main` modified it)

```powershell
.\target\debug\jbgit-pilot.exe serve --scenario conflict --cols 140 --rows 40
```

### Scenario D: Multi-Repository & Worktrees (`--scenario multi-repo`)
Creates three sibling directories under `.pilot/` for repository-switching QA (Spec 12 #09/#10):
- `sandbox-repo` (basic scenario: `main`, `feature/ui-test`, `bugfix/scroll`, tag `v0.1.0`, one modified + one untracked file)
- `sandbox-repo-feature`: linked worktree of `sandbox-repo` with `feature/ui-test` checked out (plus an untracked `WORKTREE_NOTE.md`)
- `sandbox-repo-b`: unrelated repository on `develop` with one modified file
- The isolated config dir gets a `recent-repos.toml` listing `sandbox-repo-b`, so `Alt+O` (repository switcher) shows it next to the worktree; `Alt+W` opens the Working Trees modal

Verify after every switch that the header, Log, Commit (stage), Stash and Console all point to the new repository, that the switch is announced in the footer, and that each worktree keeps its own commit draft.

```powershell
.\target\debug\jbgit-pilot.exe serve --sandbox --scenario multi-repo --cols 140 --rows 40
```

### Scenario C: Custom Theme & Keymap Config (`--custom-config`)
Can be combined with `--sandbox` (or `--repo <dir>`) to auto-generate `.pilot/custom-config/` (`jbgit.toml`, `theme.toml`, `keymap.toml`) and launch with `JBGIT_CONFIG_DIR`:
- `jbgit.toml`: `language = "en-US"`, `default_tab = "log"`, plus `[layout]` custom split ratios (`log_ratio`, `commit_ratio`, `file_history_ratio`, `file_history_vertical_ratio`)
- `theme.toml`: `flavor = "catppuccin-mocha"`, `glyphs.mode = "unicode"`, `bg_canvas = "#112233"`, `bg_panel = "#223344"`, `border_focused = "#FF6600"`
- `keymap.toml`: `footer_max_lines = 2`, unbinds `r`/`b`/`n`/`<C-s>` via `noop`, rebinds `refresh -> <F8>`, `toggle_branches_popup -> <F6>`, `new_branch -> <F7>`, `commit -> <F9>`, and adds custom chord `["g", "x"] -> refresh`

```powershell
.\target\debug\jbgit-pilot.exe serve --sandbox --custom-config --cols 140 --rows 40
```

### Scenario E: Repository Capabilities (`--scenario capabilities`)
Creates a `--depth 1` shallow clone in `.pilot/sandbox-repo` backed by `.pilot/sandbox-remote.git` and `.pilot/sandbox-sub.git` for Spec 12 #14:
- **Shallow clone**: `.git/shallow` exists with 3 unloaded upstream commits behind `origin/main`, displaying the Log shallow boundary marker and enabling **Unshallow** (`?` Command Palette / Log context action)
- **Dirty / ahead submodule**: `libs/submod` has both a local commit ahead of its recorded gitlink and uncommitted changes in `sub.txt` (`[new commits, dirty]`), rendered read-only in the Commit / Stage workbench
- **Git LFS pointer diff**: `.gitattributes` configures `*.bin filter=lfs`, and `assets/model.bin` has a modified LFS v1 pointer (`2 MiB`) showing the LFS pointer diff placeholder and disabling line-level staging
- **Signed commit status**: `HEAD` (`chore: signed commit fixture`) carries a synthetic PGP `gpgsig` header so Log / Details display a signed (cannot-check/bad) status alongside unsigned `HEAD~1` without requiring local GPG keys

```powershell
.\target\debug\jbgit-pilot.exe serve --sandbox --scenario capabilities --cols 140 --rows 40
```

### Scenario F: Askpass Credential Bridge (`--scenario askpass`)
Starts an ephemeral loopback HTTP Basic Auth server (`git-http-backend`, accepting `alice` / `s3cr3t`; `.pilot/ready.json` exports only `askpass.url` and `auth_required` with no plaintext credentials written under `.pilot/`) with `credential.helper = ""` so `GIT_ASKPASS` is forced on every remote op (Spec 12 #08):
- `origin/main` has 1 un-fetched remote commit (`f` Fetch triggers the masked `jbgit` credential prompt)
- Local `feature/auth-push` has 1 unpushed commit (`P` Push triggers the credential prompt and pushes over HTTP Basic Auth)
- Pressing `Esc` in the credential dialog cancels askpass and classifies `AuthenticationFailed` without leaking credentials into `[4 Console]`

```powershell
.\target\debug\jbgit-pilot.exe serve --sandbox --scenario askpass --cols 140 --rows 40
```

### Scenario G: Rebase Conflict (`--scenario rebase-conflict`)
Creates `.pilot/sandbox-repo` in an active **`git rebase main`** state on `feature/rebase-me` stopped with `UU rebase_target.txt` and 1 remaining commit (`rebase_followup.txt`) in the sequence (Spec 13 #02 / #05 / #09), so you can verify rebase ours/theirs reversal (`Ours` = upstream `main`, `Theirs` = rebased commit) and Continue/Abort:

```powershell
.\target\debug\jbgit-pilot.exe serve --sandbox --scenario rebase-conflict --cols 140 --rows 40
```

### Scenario H: Active Bisect (`--scenario bisect`)
Creates `.pilot/sandbox-repo` with 8 linear commits on `main` and an active **`git bisect start HEAD <root>`** session (Spec 13 #14) to verify the `BISECTING` banner (`Good` / `Bad` / `Skip` / `Reset`) and Log bisect controls:

```powershell
.\target\debug\jbgit-pilot.exe serve --sandbox --scenario bisect --cols 140 --rows 40
```

### Scenario I: Welcome / Init / Clone (`--scenario welcome`)
Creates an empty non-Git workspace (under `%TEMP%` when `.pilot/` sits inside a Git repository so parent `.git` discovery does not shadow it), `.pilot/sandbox-clone-src.git` (local bare repository with 2 commits, path exported in `.pilot/ready.json` as `clone_source`), and `.pilot/sandbox-recent` (registered in `recent-repos.toml`) so startup opens the **Welcome modal** (*Initialize Git Repository* / *Clone Repository* / *Open Recent*, Spec 13 #13):

```powershell
.\target\debug\jbgit-pilot.exe serve --sandbox --scenario welcome --cols 140 --rows 40
```

### Scenario J: Commit Workflow & CRLF Guard (`--scenario workflow`)
Creates `.pilot/sandbox-repo` with `commit.template = .gitmessage`, cleared local `user.name`/`user.email`, `core.autocrlf = false`, and a staged CRLF file (`crlf_guarded.txt`) to test template pre-fill, missing-identity prompt, and the pre-commit CRLF guard (Spec 13 #07 / #13):

```powershell
.\target\debug\jbgit-pilot.exe serve --sandbox --scenario workflow --cols 140 --rows 40
```

## 3. Interactive AI Testing Commands

Every action command injects real PTY input, waits for screen output to settle, and prints the updated screen with 0-based row numbers (`00|...` .. `39|...`):

```powershell
# Read current screen
.\target\debug\jbgit-pilot.exe screen

# Send keys (single chars, named keys, or modifier combos)
.\target\debug\jbgit-pilot.exe key b
.\target\debug\jbgit-pilot.exe key Down Down Enter
.\target\debug\jbgit-pilot.exe key Ctrl+s
.\target\debug\jbgit-pilot.exe key Shift+F10
.\target\debug\jbgit-pilot.exe key F8
.\target\debug\jbgit-pilot.exe key Alt+r

# Type UTF-8 text into focused search box / commit editor / dialog input
.\target\debug\jbgit-pilot.exe type "fix: test commit"

# Locate UI text to get exact 0-based (col, row) coordinates, then hover / left-click / right-click / scroll
.\target\debug\jbgit-pilot.exe find "Commit"
.\target\debug\jbgit-pilot.exe hover 12 1
.\target\debug\jbgit-pilot.exe click 12 1
.\target\debug\jbgit-pilot.exe click 20 8 --ctrl
.\target\debug\jbgit-pilot.exe click 20 12 --shift
.\target\debug\jbgit-pilot.exe click 20 14 --ctrl --shift
.\target\debug\jbgit-pilot.exe rclick 45 5
.\target\debug\jbgit-pilot.exe scroll down 45 5 --steps 3

# Launch with custom environment overrides (e.g., deterministic askpass responder)
.\target\debug\jbgit-pilot.exe serve --sandbox --scenario askpass --env JBGIT_ASKPASS_RESPONDER=cancel

# Inspect real RGB colors & OSC 52 clipboard output
.\target\debug\jbgit-pilot.exe colors
.\target\debug\jbgit-pilot.exe inspect-row 39
.\target\debug\jbgit-pilot.exe clipboard

# Resize terminal dynamically & wait for async git ops
.\target\debug\jbgit-pilot.exe resize 80 24
.\target\debug\jbgit-pilot.exe wait-idle

# Cleanly quit jbgit and close the PTY session
.\target\debug\jbgit-pilot.exe stop
```

Raw PTY output (including any stderr panic backtrace) is saved at `.pilot/pty_raw.log`.

## 4. Exploratory QA Checklist for AI Agents

When asked to find bugs or unimplemented features interactively, explore systematically and verify **both the UI screen AND the underlying `git` repository on disk** (`.pilot/sandbox-repo`):

1. **Log Tab (`2`)**:
   - Navigate commits (`j`/`k`), toggle Diff Preview (`Ctrl+D`), zoom Diff (`F4`, press `P` on `merge feature/ui-cards` to switch merge parent), inspect commit details and file diffs, click parent commit links, copy commit hash (`y` + verify with `clipboard`), filter by text/regex/branch/user/date/paths, open context menu (`.`) and test actions (New Branch, Checkout, Reset, Reword, Drop, Cherry-pick, Merge, Rebase).
2. **Log Changes Tree Context Menu & File History (`H` / Spec #09)**:
   - Focus Changes Tree (`Enter` from commit table or click), open context menu (`.` / `Shift+F10` / `rclick`), test file & directory actions: View/Zoom Diff (`F4`), Compare with Local (`l`, auto-selects target file), View File History (`h` / `H`), View Directory History on `src/components`, Filter Log by Path (`f`), Restore File (`Get Version` / `g` with confirmation), Copy Relative Path (`c` + verify with `clipboard`), Copy File Patch (`p` + verify with `clipboard`).
   - Open File History (`H`) on `src/auth.rs` (in Log or Commit tab): verify `← src/legacy_auth.rs` rename badge, toggle `--follow` (`f`), Compare with Local (`l`), two-commit range diff (`Shift+Down`), Side-by-Side (`S`), Context lines (`w`), Zoom Diff (`z`/`F4`), resize `< 100` cols (`resize 80 24`) to test vertical split fallback, `Enter` to locate commit in Log, and `Esc` to close non-destructively.
3. **Commit / Stage Tab (`1`)**:
   - Stage/unstage files (`Space`), expand/collapse directory tree, toggle Ignored files (`debug_trace.log`), open File History (`H`) on tracked files, enter Diff pane (`Tab`), test multi-hunk navigation (`[`/`]`), hunk/line staging (`Space`), visual mode (`v`), 3-way inspector (`i` on `staged_partial.txt`), rollback (`d`), undo rollback (`Ctrl+Z`), commit message editor (`c`), Amend (`A`), Sign-off (`Alt+s`), Commit (`Ctrl+S`), and verify `git -C .pilot/sandbox-repo log -1` / `status`.
4. **Branches Popup (`b` / `Ctrl+~`) & Branch Tree**:
   - Speed-search filtering, favorite toggle (`Space`), branch submenu (`Enter`), checkout, create branch, rename (`F2`), delete (`d`/`Del`), compare with current, remote fetch/pull/push.
5. **Stash Tab (`3`) & Console Tab (`4`)**:
   - Inspect both stashes (`Enter`), apply (`a`), pop (`o`), branch from stash (`B`), drop (`d`), clear (`C`); in Console, press `p` (`git_status_probe`), enter Output Detail (`Enter`), and test `gg`/`G`/`Ctrl+D`/`Ctrl+U` bounded scroll clamping & severity styling.
6. **Merge Conflict Scenario (`--scenario conflict`)**:
   - Check the ongoing `MERGING` banner, open the 3-Way Conflict Resolver on `conflict_app.txt`, resolve blocks, stage, and verify `git status`.
7. **Theme, Layout & Keymap Config (`--sandbox --custom-config`)**:
   - Verify with `colors` / `inspect-row` that `#112233` / `#223344` / `#FF6600`, `Unicode` glyphs, and `[layout]` split ratios (including `file_history_ratio` / `file_history_vertical_ratio`) actually apply.
   - Verify old keys (`r`, `b`, `n`, `Ctrl+s`) stop working, rebound keys (`F8`, `F6`, `F7`, `F9`, chord `g x`) execute real git operations, and **shortcut hints update dynamically** in: footer bar, `?` Help dialog, `.` Context Menu, Branches Popup (`NewBranch` hint), and Which-Key chord popup.
8. **Hover & Layout Resize Stress**:
   - Use `hover <col> <row>` on toolbar buttons/header pills to verify hover highlighting; `resize` to `80x24`, `50x16`, `35x8` (too-small guard), and back to `140x40`; check for panics, clipped modals, or misaligned CJK text.
9. **Spec 12 Parity Features (across `full`, `multi-repo`, `capabilities`, `askpass`)**:
   - **Annotate (`Alt+G`) & Selection History (`H`)**: Open Annotate on `src/auth.rs` from Log/Commit/File History, toggle diff (`d`), whitespace (`w`), moves (`m`), step to previous revision (`p`), jump to commit (`Enter`), and select a line range + `H` for `git log -L` selection history.
   - **Tag Management & Push Dialog (`P`)**: Delete local (`v0.2.0-rc1`) & remote (`v0.1.0`) tags, push a single tag, open the Push dialog on `main` / `feature/diverged` to inspect outgoing commits, edit target branch, toggle tag mode / `--force-with-lease`, and verify Push Rejected retry options.
   - **Merge Options, Update Project (`Alt+U`) & Remotes (`Alt+R`)**: Open the Merge options dialog (`--no-ff` / `--ff-only` / `--squash` mutual exclusion), run `Alt+U` Update Project (`Merge`/`Rebase` + stash/restore + Update Info summary), and add/rename/edit/remove a remote via `Alt+R`.
   - **Repo Switcher (`Alt+O`) & Worktrees (`Alt+W`)** (`--scenario multi-repo`): Switch between `sandbox-repo`, `sandbox-repo-feature`, and `sandbox-repo-b`, create/lock/unlock/remove a worktree, and verify drafts stay isolated per repo/worktree.
   - **Linear Collapse, Apply Patch (`Alt+P`) & Browse at Revision (`Alt+T`)** (`--scenario full`): Collapse/expand `feature/linear-chain` (`Ctrl+Shift+-` / `Ctrl+Shift+=` or `z c`/`z o`/`z M`/`z R` in Vim preset), apply `.pilot/sample.patch` cleanly and verify `.pilot/conflict.patch` surfaces a check failure, and browse the tree at a historical commit via `Alt+T`.
   - **Capabilities (`--scenario capabilities`) & Askpass (`--scenario askpass`)**: Verify the shallow boundary marker + Unshallow, `libs/submod` read-only submodule badge, `assets/model.bin` LFS pointer diff placeholder, and end-to-end HTTP Basic Auth Fetch/Push (`alice` / `s3cr3t`) + `Esc` cancel with zero plaintext credential output in `[4 Console]`.
10. **Spec 13 Reference Parity Features (across `full`, `conflict`, `rebase-conflict`, `bisect`, `welcome`, `workflow`)**:
   - **Non-Contiguous Log Selection & Compare (`--scenario full`)**: Use `click <col> <row> --ctrl` on two non-contiguous commits in Log to verify aggregated multi-commit changes and two-commit comparison (`Compare Versions` / swap direction), plus `show_only_affected_changes` under active Path filter.
   - **Diff Whitespace, Horizontal Scroll, Search & Binary Card (`--scenario full`)**: Open `src/wide_lines.rs` in Diff/Commit view, cycle whitespace modes (`Default` / `IgnoreChange` / `IgnoreAll`), test horizontal scroll vs soft-wrap on the ~170-col line, search inside the diff (`/` / `Ctrl+F`), and inspect `assets/logo.bin` to verify the non-LFS binary file summary card.
   - **Pull / Rebase Option Dialogs & Reflog / Highlighters (`--scenario full`)**: Open Pull options, Rebase options, `feature/cherry-source` two-branch comparison, Reflog modal, LinearBek toggle, and My Commits / Cherry-Picked highlighters.
   - **Multi-File Conflict Queue, Delete/Modify & Rebase Reversal (`--scenario conflict` / `rebase-conflict`)**: On `conflict`, inspect the Conflict Queue across `conflict_app.txt` (test auto-merge + inline Result editing), `conflict_second.txt`, and `deleted_by_theirs.txt` (`UD` keep-ours vs accept-theirs removal). On `rebase-conflict`, verify `REBASING` banner, reversed Ours/Theirs labels on `rebase_target.txt`, and Continue/Abort flow.
   - **Active Bisect Banner (`--scenario bisect`)**: Verify the `BISECTING` banner displays remaining steps/revisions and `Good` / `Bad` / `Skip` / `Reset` buttons advance or reset `git bisect` cleanly.
   - **Welcome / Init / Clone (`--scenario welcome`)**: Verify startup opens the Welcome modal in the empty workspace (`repo` in `.pilot/ready.json`) with *Initialize Git Repository*, *Clone Repository* (test cloning `clone_source` from `.pilot/ready.json`), and *Open Recent* (`sandbox-recent`).
   - **Commit Template, Missing Identity & CRLF Guard (`--scenario workflow`)**: Verify Commit editor pre-fills from `.gitmessage`, attempting Commit triggers the missing `user.name`/`user.email` identity prompt, and after setting identity, committing staged `crlf_guarded.txt` triggers the CRLF pre-commit guard.

## 5. Recording Findings

Report discovered unimplemented features or bugs (with exact `jbgit-pilot` reproduction steps and screen captures) directly in your response or save a report under `.pilot/qa-report.md` (only create files in `.scratch/` if the user explicitly asks to file formal issue tickets per [issue-tracker.md](./issue-tracker.md)).
