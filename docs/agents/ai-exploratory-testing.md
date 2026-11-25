# Automated & Exploratory TUI QA (`jbgit-pilot`)

`jbgit-pilot` runs `jbgit` inside a **Windows ConPTY / Unix PTY** backed by a `vt100` emulator, giving AI agents exact screen text, cursor/RGB inspection, OSC 52 clipboard capture, and real key/mouse/resize injection.

## 1. Automated QA Workflow

1. **Build prerequisites**:
   ```powershell
   cargo build -p jbgit-tui   # add `--target-dir tools/jbgit-pilot/target` if jbgit.exe is locked
   cargo build -p jbgit-pilot
   ```
   **Done when**: `.\target\debug\jbgit.exe` and `.\target\debug\jbgit-pilot.exe` reflect the latest code.

2. **Sweep scenarios (dual verification)**:
   Exercise every scenario in Section 3, plus `hover` and `resize` (`80x24`, `50x16`, `35x8`, `140x40`) stress checks.
   - **Session isolation**: Pass a unique `--session-dir` (for example `.pilot-full`, `.pilot-conflict`) per concurrent scenario or subagent.
   - **Dual verification**: After every state-mutating UI action, verify both terminal output (`screen`, `colors`, `inspect-row`, `clipboard`) and on-disk Git state (`git -C <repo> status` / `log` / `rev-parse`).
   - **Done when**: Every scenario and check in Section 3 is recorded as pass or finding.

3. **Classify & report findings**:
   Write all findings to `.pilot/qa-report.md` (create formal `.scratch/` tickets only when explicitly requested per [issue-tracker.md](./issue-tracker.md)):
   - **P0 (Blocker)**: Panic, crash, hang/freeze, silent data loss, credential leak, or corrupted Git state.
   - **P1 (Major)**: Broken core workflow, misleading or failing Git op, unreachable modal/control, or wrong repository mutation.
   - **P2 (Minor)**: Layout clipping, CJK truncation/border shift, wrong shortcut hint or stale status notice, hover/theme mismatch.
   - **P3 (Polish)**: Rough copy/translation or minor alignment glitches.
   Include for each entry: severity + title, scenario + launch flags, exact `jbgit-pilot` command sequence, screen/`colors`/`git -C` evidence, and expected vs actual behavior.
   **Done when**: `.pilot/qa-report.md` accounts for every issue from Step 2.

4. **Fix & regression-lock loop** *(only when asked to fix)*:
   Fix root cause in `crates/` (per `CODING_STANDARDS.md`) → add fast `TestBackend`/engine regression test → re-verify in `jbgit-pilot` → pass `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace --locked`.
   **Done when**: All checks pass after the final edit.

5. **Session hygiene**:
   Run `.\target\debug\jbgit-pilot.exe --session-dir <dir> stop` on every launched daemon, remove temporary `.pilot-*` session directories (keeping `.pilot/qa-report.md` if generated), and verify `git status --short` is clean of test artifacts.
   **Done when**: Zero test daemons remain and `git status --short` shows only intended edits.

## 2. Driver Usage (`jbgit-pilot`)

Launch the server **with `IsDaemon: true`** (all runtime artifacts stay under `--session-dir`, default `.pilot/`; raw PTY + stderr backtraces go to `<session-dir>/pty_raw.log`). Run `.\target\debug\jbgit-pilot.exe --help` for full CLI flags.

```powershell
# Launch a sandbox scenario (adds isolated empty config unless --custom-config / --config-dir / --use-user-config)
.\target\debug\jbgit-pilot.exe [--session-dir .pilot-run] serve --sandbox --scenario <name> --cols 140 --rows 40 [--custom-config] [--env KEY=VALUE]

# Inspect & interact (all coordinates are 0-based; action commands print the settled screen)
.\target\debug\jbgit-pilot.exe screen
.\target\debug\jbgit-pilot.exe key Ctrl+s                    # also: b, Down Down Enter, Shift+F10, F8, Alt+r
.\target\debug\jbgit-pilot.exe type "fix: test commit"
.\target\debug\jbgit-pilot.exe find "Commit"                 # returns 0-based (col, row)
.\target\debug\jbgit-pilot.exe hover 12 1
.\target\debug\jbgit-pilot.exe click 12 1 [--ctrl] [--shift] [--alt]
.\target\debug\jbgit-pilot.exe rclick 45 5 [--ctrl] [--shift] [--alt]
.\target\debug\jbgit-pilot.exe scroll down 45 5 --steps 3
.\target\debug\jbgit-pilot.exe colors                        # + `inspect-row 39`, `clipboard` (OSC 52)
.\target\debug\jbgit-pilot.exe resize 80 24                  # + `wait-idle`
.\target\debug\jbgit-pilot.exe stop
```

## 3. Sandbox Scenarios & Verification Matrix

### `full` (default `--sandbox` / `--scenario full`)
Multi-branch DAG (`main` ahead of `origin/main` by 3, `feature/login`, `feature/ui-cards` merge, `bugfix/scroll-edge`, remote-only `origin/feature/remote-only`, pushed tag `v0.1.0`, unpushed annotated tag `v0.2.0-rc1`), `feature/linear-chain` (5 commits), `feature/pull-me` (2 behind remote), `feature/diverged` (diverged from remote), `feature/cherry-source` (cherry-picked `feature/login` + `src/cherry_helper.rs`), renamed history (`src/legacy_auth.rs -> src/auth.rs`), nested `src/components/header.rs`, `.pilot/sample.patch` + `.pilot/conflict.patch` (paths exported in `ready.json`), 2 stashes (`wip: refactor stash test`, `wip: older stash`), `staged_partial.txt` (partially staged), `src/main.rs` (multi-hunk), `src/wide_lines.rs` (indent + ~170-col edit), `assets/logo.bin` (binary edit), `scratch_note.txt` (untracked), `debug_trace.log` (ignored).
- **Log (`2`)**: Navigate (`j`/`k`), Diff Preview (`Ctrl+D`), Zoom Diff (`F4`, press `P` on `merge feature/ui-cards` to switch parent), click parent links, copy hash (`y` + `clipboard`), filters (text/regex/branch/user/date/path + `show_only_affected_changes`), context menu (`.`), non-contiguous `click --ctrl` comparison (`Compare Versions` + swap direction), Reflog modal, LinearBek toggle, My Commits / Cherry-Picked highlighters (`feature/cherry-source`).
- **Log Changes Tree & File History (`H`)**: Open `.` / `Shift+F10` / `rclick` on files & `src/components` (View/Zoom Diff `F4`, Compare with Local `l`, File/Directory History `h`/`H`, Filter Log by Path `f`, Get Version `g`, Copy Relative Path `c`, Copy File Patch `p` + `clipboard`). In File History on `src/auth.rs`, verify `← src/legacy_auth.rs`, toggle `--follow` (`f`), range diff (`Shift+Down`), Side-by-Side (`S`), Context (`w`), Zoom (`z`/`F4`), narrow vertical split (`resize 80 24`), `Enter` jump to Log, `Esc`.
- **Commit / Stage (`1`) & Diff**: Stage/unstage (`Space`), folder expand/collapse, toggle Ignored (`debug_trace.log`), Diff pane (`Tab`), `[`/`]` hunk nav, hunk/line `Space`, visual mode (`v`), 3-way inspector (`i` on `staged_partial.txt`), rollback (`d`) + `Ctrl+Z`, commit editor (`c`), Amend (`A`), Sign-off (`Alt+s`), Commit (`Ctrl+S`) + `git log -1`/`status`. On `src/wide_lines.rs`, cycle whitespace modes, horizontal scroll vs soft-wrap, and inline diff search (`/`/`Ctrl+F`); on `assets/logo.bin`, verify non-LFS binary summary card.
- **Branches (`b` / `Ctrl+~`), Pull/Rebase, Push (`P`), Update (`Alt+U`) & Remotes (`Alt+R`)**: Speed search, favorite (`Space`), submenu (`Enter`), checkout, create, rename (`F2`), delete (`d`/`Del`), two-branch compare; Pull & Rebase option dialogs; Merge options (`--no-ff`/`--ff-only`/`--squash`); Fetch `feature/pull-me` (`f` → `↓2`) & Pull (`p`); Push dialog on `main`/`feature/diverged` (outgoing commits, target edit, tags, `--force-with-lease`, Push Rejected retry); delete local/remote tags & push single tag; `Alt+U` Update Project; `Alt+R` remote CRUD.
- **Stash (`3`), Console (`4`), Annotate (`Alt+G`), Collapse, Patch (`Alt+P`) & Tree Browser (`Alt+T`)**: Stash inspect/apply (`a`)/pop (`o`)/branch (`B`)/drop (`d`)/clear (`C`); Console `p` probe, Output Detail (`Enter`), `gg`/`G`/`Ctrl+D`/`Ctrl+U` clamping & severity colors; Annotate diff (`d`)/whitespace (`w`)/moves (`m`)/prev rev (`p`)/`Enter`/line-range `H` (`git log -L`); collapse/expand `feature/linear-chain` (`Ctrl+Shift+-`/`=` or Vim `zc`/`zo`/`zM`/`zR`); apply `sample.patch` cleanly and verify `conflict.patch` surfaces check failure + `--3way`; browse revision tree via `Alt+T`.
- **Hover & Resize Stress**: `hover` toolbar buttons/header pills; `resize` `80x24`, `50x16`, `35x8` (too-small guard), and back to `140x40` for panics, clipped modals, or CJK misalignment.

### `full --custom-config`
Auto-generates `<session-dir>/custom-config/` (`jbgit.toml`, `theme.toml`, `keymap.toml`): `en-US`, `log` default tab, custom `[layout]` ratios (`log_ratio`, `commit_ratio`, `file_history_ratio`, `file_history_vertical_ratio`), `catppuccin-mocha`, `unicode` glyphs, `#112233`/`#223344`/`#FF6600`, `footer_max_lines = 2`, unbinds `r`/`b`/`n`/`<C-s>` via `noop`, rebinds `refresh->F8`, `toggle_branches_popup->F6`, `new_branch->F7`, `commit->F9`, chord `g x -> refresh`.
- **Verify**: `colors` / `inspect-row` show custom hex + `Unicode` glyphs + `[layout]` splits; old bindings are no-ops; `F8`/`F6`/`F7`/`F9`/`g x` work; shortcut hints update dynamically in footer, `?` Help, `.` Context Menu, Branches Popup, and Which-Key popup.

### `conflict` (`--scenario conflict`)
Active `git merge` conflict queue: `conflict_app.txt` (`UU`, non-overlapping `theirs` block + conflict block), `conflict_second.txt` (`UU`), `deleted_by_theirs.txt` (`UD`, removed on `feature/theirs`, modified on `main`).
- **Verify**: Ongoing `MERGING` banner, Conflict Queue navigation, 3-way auto-merge + inline Result editing on `conflict_app.txt`, keep-ours vs accept-theirs removal on `deleted_by_theirs.txt`, stage resolved files, and `git status`.

### `rebase-conflict` (`--scenario rebase-conflict`)
Active `git rebase main` on `feature/rebase-me` stopped on `UU rebase_target.txt` with `rebase_followup.txt` remaining.
- **Verify**: `REBASING` banner, reversed `Ours` (`main`) / `Theirs` (rebased commit) labels on `rebase_target.txt`, resolve, and Continue/Abort.

### `bisect` (`--scenario bisect`)
8 linear commits in active `git bisect start HEAD <root>`.
- **Verify**: `BISECTING` banner step/revision estimate and `Good`/`Bad`/`Skip`/`Reset` controls update `HEAD` or clear `.git/BISECT_START`.

### `welcome` (`--scenario welcome`)
Empty non-Git directory (placed under `%TEMP%` when `--session-dir` sits inside a Git repo), local bare `clone_source` (in `ready.json`), and `sandbox-recent` in `recent-repos.toml`.
- **Verify**: Startup opens Welcome modal with *Initialize Git Repository*, *Clone Repository* (test `clone_source`), and *Open Recent* (`sandbox-recent`).

### `workflow` (`--scenario workflow`)
`commit.template = .gitmessage`, cleared local `user.name`/`user.email`, `core.autocrlf = false`, staged CRLF `crlf_guarded.txt`.
- **Verify**: Commit editor pre-fills from `.gitmessage`; `Ctrl+S` opens missing Git identity prompt; after setting identity, committing `crlf_guarded.txt` triggers the CRLF pre-commit guard.

### `multi-repo` (`--scenario multi-repo`)
`sandbox-repo`, linked worktree `sandbox-repo-feature` (`feature/ui-test` + `WORKTREE_NOTE.md`), unrelated `sandbox-repo-b` (`develop` + `recent-repos.toml`).
- **Verify**: Switch via `Alt+O` and manage worktrees via `Alt+W`; header, Log, Commit, Stash, and Console follow the active repo, footer announces switches, and commit drafts stay isolated per repo/worktree.

### `capabilities` (`--scenario capabilities`)
`--depth 1` shallow clone (3 upstream commits behind `origin/main`), dirty/ahead submodule `libs/submod`, LFS v1 pointer `assets/model.bin` (`2 MiB`), synthetic PGP `gpgsig` on `HEAD` vs unsigned `HEAD~1`.
- **Verify**: Shallow boundary marker + Unshallow (`?`/context), read-only `libs/submod` badge, LFS pointer diff placeholder (line staging disabled), and signed vs unsigned badges in Log/Details.

### `askpass` (`--scenario askpass`)
Loopback HTTP Basic Auth (`alice` / `s3cr3t`, `credential.helper = ""`, `askpass.url` in `ready.json`): `origin/main` has 1 un-fetched commit, `feature/auth-push` has 1 unpushed commit.
- **Verify**: `f` Fetch and `P` Push trigger masked `jbgit` credential prompts; `Esc` (or `--env JBGIT_ASKPASS_RESPONDER=cancel`) classifies `AuthenticationFailed`; `[4 Console]` contains zero plaintext credentials.
