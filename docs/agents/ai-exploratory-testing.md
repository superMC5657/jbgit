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
- Multi-branch DAG with merge commit (`main` ahead of `origin/main` by 3 commits, `feature/login`, `feature/ui-cards`, `bugfix/scroll-edge`, remote-only `origin/feature/remote-only`, tag `v0.1.0`)
- **Renamed & multi-commit file history**: `src/legacy_auth.rs` → `src/auth.rs` (4 commits on `main` across `git mv`, plus nested `src/components/header.rs` for directory history / path filtering)
- **Remote sync branches**:
  - `feature/pull-me` (remote `origin/feature/pull-me` has 2 un-fetched peer commits → test `f` Fetch showing `↓2`, then checkout & `p` Pull)
  - `feature/diverged` (diverged from `origin/feature/diverged` → test `P` Push Rejected / Force Push / Rebase handling)
- 2 real stashes (`wip: refactor stash test`, `wip: older stash`)
- 1 partially staged file (`staged_partial.txt`), 1 multi-hunk modified file (`src/main.rs`), 1 untracked file (`scratch_note.txt`), 1 ignored file (`debug_trace.log` via `.gitignore`)
- Isolated empty config dir (does not touch user global config)

```powershell
.\target\debug\jbgit-pilot.exe serve --sandbox --cols 140 --rows 40
```

### Scenario B: Merge Conflict (`--scenario conflict`)
Creates `.pilot/sandbox-repo` in an active **`git merge` conflict** state (`conflict_app.txt` has real `<<<<<<<` / `=======` / `>>>>>>>` conflict markers) for testing the ongoing conflict banner and 3-way conflict resolver:

```powershell
.\target\debug\jbgit-pilot.exe serve --scenario conflict --cols 140 --rows 40
```

### Scenario C: Custom Theme & Keymap Config (`--custom-config`)
Can be combined with `--sandbox` (or `--repo <dir>`) to auto-generate `.pilot/custom-config/` (`jbgit.toml`, `theme.toml`, `keymap.toml`) and launch with `JBGIT_CONFIG_DIR`:
- `jbgit.toml`: `language = "en-US"`, `default_tab = "log"`, plus `[layout]` custom split ratios (`log_ratio`, `commit_ratio`, `file_history_ratio`, `file_history_vertical_ratio`)
- `theme.toml`: `flavor = "catppuccin-mocha"`, `glyphs.mode = "unicode"`, `bg_canvas = "#112233"`, `bg_panel = "#223344"`, `border_focused = "#FF6600"`
- `keymap.toml`: `footer_max_lines = 2`, unbinds `r`/`b`/`n`/`<C-s>` via `noop`, rebinds `refresh -> <F8>`, `toggle_branches_popup -> <F6>`, `new_branch -> <F7>`, `commit -> <F9>`, and adds custom chord `["g", "x"] -> refresh`

```powershell
.\target\debug\jbgit-pilot.exe serve --sandbox --custom-config --cols 140 --rows 40
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
.\target\debug\jbgit-pilot.exe rclick 45 5
.\target\debug\jbgit-pilot.exe scroll down 45 5 --steps 3

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

## 5. Recording Findings

Report discovered unimplemented features or bugs (with exact `jbgit-pilot` reproduction steps and screen captures) directly in your response or save a report under `.pilot/qa-report.md` (only create files in `.scratch/` if the user explicitly asks to file formal issue tickets per [issue-tracker.md](./issue-tracker.md)).
