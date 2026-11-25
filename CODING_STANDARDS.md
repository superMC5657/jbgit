# Coding Standards

Hard-won engineering invariants for `jbgit`, distilled from production and audit defects across `jbgit-core`, `jbgit-engine`, and `jbgit-tui`. Automated `code-review` runs enforce these rules by ID (e.g., `CS-2.1`). Style and lint rules already enforced by `rustfmt` and `clippy` are omitted.

---

## 1. UTF-8, CJK & Numeric Safety

### CS-1.1 Guard UTF-8 char boundaries before byte slicing
- **Do:** Verify `is_ascii()`, match an explicit ASCII character first, or advance by `ch.len_utf8()` / `char_indices()` before slicing `&str`.
- **Don't:** Slice `&s[1..]` or `&s[0..2]` on unchecked user or Git strings (revision expressions like `HEAD~中`, timezone offsets, clone URLs, CJK text).
- **Why:** Byte indexing across a multi-byte UTF-8 character panics immediately.

### CS-1.2 Decode full Git C-quoted paths before separator normalization
- **Do:** Unescape full Git C-quoted byte sequences (`\ooo` octal UTF-8, `\t`, `\n`, `\r`, `\a`, `\b`, `\f`, `\v`, `\"`, `\\`) in patch/CLI parsers before converting `\` to `/`.
- **Don't:** Run `path.replace('\\', "/")` on partially unquoted Git paths.
- **Why:** `core.quotePath = true` emits octal escapes for CJK/non-ASCII paths; normalizing `\` first shatters filenames into slash-separated octal digits.

### CS-1.3 Use checked date and integer arithmetic on user input
- **Do:** Parse counts as bounded unsigned integers with `checked_mul`/`checked_sub`, use `NaiveDate::succ_opt()` / `pred_opt()`, and return an error/`None` on overflow or unparseable digits.
- **Don't:** Perform unchecked `n * 86_400`, `day + Duration::days(1)`, or silently coerce overflow via `.parse().ok().unwrap_or(1)`.
- **Why:** Crafted filter/revision inputs (e.g., `+262142-12-31`, `999999999999999d`, `-7d`) panic `chrono` or silently wrap into bogus queries.

---

## 2. Git CLI Security & Subprocess Discipline

### CS-2.1 Always insert `--` before positional remotes, refs, and paths
- **Do:** Pass `"--"` before positional remote, ref, branch, tag, and path arguments in every `git` invocation (`push`, `fetch`, `pull`, `merge`, `rebase`, `ls-remote`, `stash branch`, `log -L`, etc.).
- **Don't:** Pass user- or config-supplied remotes/refs without end-of-options termination.
- **Why:** Flag-like values (`--upload-pack=<cmd>`, `--receive-pack=<cmd>`, `--exec=<cmd>`, `-tag`) are parsed as Git options, enabling arbitrary command execution or ambiguous-path errors.

### CS-2.2 Respect configured `[git] executable` and explicit `--git-dir`
- **Do:** Spawn Git subprocesses using the resolved `[git] executable` setting and pass `--git-dir` (or explicit repo paths) for repository-scoped commands.
- **Don't:** Hardcode `"git"` or rely solely on process `cwd` for repository discovery.
- **Why:** Users override Git binaries on Windows/WSL/custom installs, and `cwd` discovery breaks on nested or detached paths.

### CS-2.3 Stream subprocess output as raw bytes (`read_until(b'\n')` + `from_utf8_lossy`)
- **Do:** Read `git` stdout/stderr as raw bytes with `read_until(b'\n', &mut buf)`, preserve `\r` bytes in diff/patch payloads, and decode with `String::from_utf8_lossy` only at text boundaries.
- **Don't:** Use `BufReader::lines()` on `git` stdout/stderr.
- **Why:** `Lines::next_line()` fails with `InvalidData` on non-UTF-8 files (GBK/Shift-JIS/Latin-1)—leaving the pipe open to deadlock on OS buffers or silently truncating patches—and strips `\r\n`, breaking `git apply` on CRLF repositories.

### CS-2.4 Keep `tokio::select!` parser state outside the loop
- **Do:** Hold partial byte buffers (`stderr_buf`) and CR/LF state (`pending_cr`) outside `tokio::select!`, or read stdout and stderr in separate tasks forwarding to a channel; emit `\r` progress segments immediately.
- **Don't:** Store partial bytes inside an async helper that calls `consume()` followed by another `.await` inside a `select!` branch.
- **Why:** When another branch wins `select!`, the losing future is dropped and already-consumed `fatal:`/`CONFLICT`/`index.lock` stderr bytes are permanently lost.

### CS-2.5 Enforce child teardown (`kill_on_drop(true)` + Windows Job Object)
- **Do:** Set `cmd.kill_on_drop(true)` on every `tokio::process::Command`, abort companion stdin writer tasks via a drop guard, and bind Windows children to a `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` Job Object.
- **Don't:** Spawn cancellable commands (`git clone`, remote checks, etc.) without drop cleanup or Windows process-tree containment.
- **Why:** Cancelling a task leaves orphan `git` and `git-remote-https`/`ssh` grandchildren running and holding Windows directory locks.

### CS-2.6 Never block Tokio or `Git2WorkerPool` with uninterruptible subprocesses or thread joins
- **Do:** Run cancellable CLI subprocesses asynchronously via `tokio::process::Command` outside `Git2WorkerPool`, and isolate unavoidable blocking calls (e.g., clipboard subprocesses, `JoinHandle::join()` teardown) in `tokio::task::spawn_blocking` or dedicated teardown threads.
- **Don't:** Call synchronous `std::process::Command::output()` or `JoinHandle::join()` on Tokio worker threads or inside `Git2WorkerPool` interactive threads.
- **Why:** Uninterruptible CLI calls ignore `EpochToken` cancellation and saturate all pool threads, starving diff/details/stage responsiveness.

### CS-2.7 Preserve step environment and context across `index.lock` retries
- **Do:** Carry full execution `env` (`GIT_EDITOR`, `GIT_SEQUENCE_EDITOR`, askpass variables) and multi-step workflow context when constructing `IndexLockConflict` retries.
- **Don't:** Retry failed guarded commands with empty `env` or after deleting temporary rebase/sequence state.
- **Why:** Stripped environment variables hang on interactive `$EDITOR` prompts, drop credentials, or abandon remaining multi-step/stash-restore actions.

---

## 3. Filesystem, Symlinks, Glob Pathspecs & Windows Paths

### CS-3.1 Never use symlink-following checks for deletes or conflicts
- **Do:** Inspect paths with `std::fs::symlink_metadata` (`file_type().is_dir()` vs `is_file() || is_symlink()`) and route all worktree/untracked deletions through `safe_remove_untracked_path`.
- **Don't:** Call `Path::is_dir()` or `Path::is_file()` before deleting files or loading conflict targets, or join unvalidated relative paths onto `workdir`.
- **Why:** Symlink-following checks misclassify directory/dangling symlinks and allow deletions outside `workdir` via parent symlinks, `..` traversal, or Windows reparse points.

### CS-3.2 Normalize Windows verbatim UNC paths before stripping `\\?\`
- **Do:** Match and convert `\\?\UNC\` (and `//?/UNC/`, case-insensitively) to `\\` / `//` *before* stripping `\\?\` / `//?/` (see `strip_verbatim_prefix` and `worktree::same_path`).
- **Don't:** Unconditionally `strip_prefix(r"\\?\")` on canonicalized Windows paths.
- **Why:** Stripping only `\\?\` turns SMB and WSL paths (`\\?\UNC\wsl.localhost\...`) into invalid relative `UNC\...` paths, breaking repo opening, watchers, and worktree identity.

### CS-3.3 Disable pathspec globbing and escape `.gitignore` metacharacters
- **Do:** Pass `IndexAddOption::DISABLE_PATHSPEC_MATCH`, `CheckoutBuilder::disable_pathspec_match(true)`, and `DiffOptions::disable_pathspec_match(true)` in libgit2 (or update index entries directly from `HEAD` instead of `repo.reset_default`), pass `--literal-pathspecs` on Git CLI pathspec commands, escape `\`, `[`, `]`, `*`, `?` when emitting literal `.gitignore` rules, and implement segment-aware gitignore matching (`*`/`?` never cross `/`; `**` matches 0+ segments).
- **Don't:** Pass raw user paths like `app/[id]/page.tsx` to glob-enabled libgit2/Git pathspec APIs or write them unescaped to `.gitignore`.
- **Why:** `fnmatch` interprets `[id]` as a character class matching nothing, causing silent no-ops on Next.js/SvelteKit paths.

### CS-3.4 Preserve external `.gitattributes` clean filters
- **Do:** Check `path_needs_git_clean_filter` on both whole-file and partial staging paths, route matching files through `git add` / `hash_object_with_filters`, and propagate filter errors (`?`).
- **Don't:** Write filtered files directly via libgit2 (`index.add_path`, `add_all`, `update_all`, `add_frombuffer`) or swallow filter failures and fall back to raw bytes.
- **Why:** libgit2 does not invoke external clean filters (`filter=lfs`, `git-crypt`), silently committing raw binaries instead of LFS pointers or plaintext secrets instead of ciphertext.

### CS-3.5 Account for `$GIT_COMMON_DIR` in linked worktrees and relative paths in watchers
- **Do:** Check `repo.commondir().join("shallow")` in addition to `repo.is_shallow()`, and strip `git_dir`/`common_dir` prefixes before classifying `.git` metadata paths (`objects/`, `logs/`, `refs/`) in `RepoWatcher`.
- **Don't:** Assume shallow markers live only in `git_dir` or match `/logs/`/`/objects/` against full absolute filesystem paths.
- **Why:** Linked worktrees store `shallow` in `$GIT_COMMON_DIR`, and repos nested under an ancestor named `logs` or `objects` silently drop 100% of watcher events.

### CS-3.6 Validate user-supplied filenames and Git ref components
- **Do:** Gate custom locale/theme/keymap filenames with `safe_preset_filename` before joining paths. Sanitize branch names per `/`-separated component (stripping `.lock` per component, stripping `@{` inside the structural fixpoint loop, and rejecting standalone `@`).
- **Don't:** Join raw config strings directly onto directories or sanitize `.lock`/`@{` only once on the full string.
- **Why:** Prevents `../` path traversal on config loads and invalid Git ref creation (`foo.lock/bar`, `@{`, `@`).

---

## 4. Engine Concurrency, Index Reload & Mutation Guards

### CS-4.1 Reload cached libgit2 index before fallback writes
- **Do:** Call `repo.index()?.read(true)?` before any libgit2 index write (`add_to_index`, `add_frombuffer`, etc.) that can follow an external `git` CLI invocation on the same repository.
- **Don't:** Flush a cached in-memory `git_index` after external `git add` calls modified `.git/index`.
- **Why:** Writing a stale cached index silently drops entries staged by earlier CLI commands.

### CS-4.2 Read-only operations never drain `MutationGuard::pending`
- **Do:** Resolve deferred watcher scopes with `permit.map_or(RefreshScope::None, MutationPermit::finish)`.
- **Don't:** Call `guard.take_pending()` when `permit` is `None`.
- **Why:** Draining `pending` on a read-only completion steals deferred events from an active writer and triggers `ExternalFsChanged` reads mid-mutation against an in-flight `.git/index` write.

### CS-4.3 Flush drained deferred refresh scopes on failed mutations
- **Do:** Emit `ExternalFsChanged(deferred)` whenever `deferred != RefreshScope::None` on *both* success and error paths after `permit.finish()`, and merge `RefreshScope::StageStatus` when a failing commit already ran staging or CRLF renormalization.
- **Don't:** Drop `deferred` in `!ok` or `Err(e)` branches.
- **Why:** Discarding drained watcher scopes leaves the UI permanently desynchronized from partial index/worktree mutations and concurrent external edits.

### CS-4.4 Validate commit inputs before mutating `.git/index`
- **Do:** Validate committer signature, `author_override`, and non-empty stripped commit message before calling `index.update_all` / `index.write()`.
- **Don't:** Stage all tracked files prior to validating commit preconditions.
- **Why:** A validation error after `index.write()` irreversibly clobbers the user's partial staging.

### CS-4.5 Refuse detached root-commit `HEAD` deletion
- **Do:** Verify `head.is_branch() && !repo.head_detached().unwrap_or(false)` before deleting a ref during root-commit undo; return an explicit error on detached `HEAD`.
- **Don't:** Call `repo.find_reference("HEAD")?.delete()?`.
- **Why:** Unlinking `.git/HEAD` corrupts the repository so neither `git` nor `libgit2` can open it.

### CS-4.6 Bound large-repo scans and avoid accidental quadratic loops
- **Do:** Gate untracked similarity scans on `LARGE_INDEX_ENTRIES` + `EpochToken::is_stale()`, index lookups with `FxHashMap`/`FxHashSet` or binary search on sorted slices (`bek_merges`), and reuse existing commit-index maps.
- **Don't:** Run `Vec::contains`/`iter().position` inside loops over thousands of stage rows, branches, or commits, or rebuild full commit maps per ref.
- **Why:** Quadratic scans freeze interactive rendering and bulk staging on large repositories.

---

## 5. DAG, Diff, Conflict & Ref Invariants

### CS-5.1 Respect `LineEnding::None` when splicing hunks or lines
- **Do:** Track when the previous emitted line in `StageLineTracker::splice_blocks` had `LineEnding::None` and emit the document's line ending (`\r\n` if CRLF-dominant, else `\n`) before appending further lines—including under whitespace-ignore modes.
- **Don't:** Concatenate subsequent lines directly after a `LineEnding::None` line or discard EOF-newline diffs when ignoring whitespace.
- **Why:** Partially staging, unstaging, or rolling back near EOF corrupts files by fusing two lines into one (`a\nbc\n`).

### CS-5.2 Enforce conflict parser and side-labeling invariants
- **Do:** Skip inline marker scanning whenever `side_presence.is_delete_conflict()` is true and synthesize the whole-file Delete/Modify block directly. In marker-fallback parsing, capture the opening `<` run length `n >= 7` (followed by space or EOL) and require `=======` to equal `"=".repeat(n)` exactly. Swap `(our, their)` labels consistently when `is_reverse_conflict_state` is true, and exclude conflicted submodules from `unstaged`.
- **Don't:** Scan Delete/Modify worktree files for inline markers, match separators with `starts_with("=======")`, or label rebase conflicts without checking `is_reverse_conflict_state`.
- **Why:** Literal Markdown banners (`===============`) or marker text in surviving files corrupt conflict resolution, and unswapped rebase labels invert Ours/Theirs between Stage and Resolver views.

### CS-5.3 Uphold graph pagination, folding, and filter invariants
- **Do:** Under `first_parent_only`, inspect `pg.down_nodes(u).first().and_then(|t| t.as_loaded())` before filtering unloaded targets. In `LinearBek`, skip folding any merge that removes or overwrites a `GraphEdgeType::Collapsed` edge, and preserve dotted ancestry compression across non-contiguous range filters. In `BranchFilter` exclusions, match `snapshot.head_oid`/`head_branch` (not only `snapshot.refs`). Only short-circuit log loading to empty when `tip_oids.is_empty()` (even on unborn `HEAD`). Carry explicit custom `Squash` messages forward onto the final folding entry when followed by `Fixup`/`Squash`.
- **Don't:** Call `loaded_down_nodes(u).next()` for first-parent traversal, overwrite `Collapsed` edges, or wipe commits on unborn `HEAD` when other branches/tags exist.
- **Why:** Unloaded first parents silently jump to second parents, erased `Collapsed` edges orphan hidden commits, and unborn `HEAD` hides all existing branch history.

### CS-5.4 Parse remote refs with `split_tracking` and `=`-bearing keys with `rsplit_once`
- **Do:** Resolve `<remote>/<branch>` using `RepoSnapshot::split_tracking` / `remote_of_tracking` (with `split_once('/')` only as a fallback), and parse `key=value` entries whose key can contain `=` via `rsplit_once('=')`.
- **Don't:** Split remote tracking refs on the first `/` or split `expanded:<path>=<bool>` on the first `=`.
- **Why:** Remotes containing `/` (e.g., `team/upstream/feat`) bypass protected-branch rewrite guards and create wrong local branch names; local upstreams produce bogus remotes.

### CS-5.5 Preserve rename `old_path`, mode pairs, and path-swap collisions
- **Do:** Restore `old_path` and remove `new_path` when discarding unstaged renames, preserve displaced entries when folding multi-commit net changes on delete+rename or path swaps, and carry resolved `(old_mode, new_mode)` alongside `known_oids`.
- **Don't:** Ignore `old_path` on discard, overwrite existing `HashMap` net entries on rename collisions, or infer multi-commit file modes from `oids.first()`/`oids.last()`.
- **Why:** Prevents silent no-op discards, lost diff entries, and bogus mode/symlink flags on non-contiguous multi-commit selections.

---

## 6. TUI Event Loop, Modal Stack, Keymaps & i18n Parity

### CS-6.1 Never run synchronous subprocesses or disk I/O on the UI event loop
- **Do:** Probe terminal capabilities (`tmux`, graphics) asynchronously, wrap repo discovery/`bind_location` in `tokio::task::spawn_blocking`, and route draft/history/recent-repo persistence to background workers.
- **Don't:** Spawn blocking commands or flush files synchronously inside `on_resize`, `switch_repository`, or dispatch handlers.
- **Why:** Blocking the UI loop stalls rendering and input coalescing (e.g., 50ms+ per `tmux` resize event).

### CS-6.2 Stamp every async UI task with `session_generation` and a task ID
- **Do:** Deliver async UI completions via `SessionInput::UiAsyncFrom(generation, ...)` with a monotonic task ID checked by `AppState`, abort in-flight tasks (`active_clone_task`, etc.) on repo switch/shutdown, invalidate *all* syntax slots in `prepare_unbind`, and re-sync pending dialogs (e.g., credentials) in `rebind`.
- **Don't:** Emit unstamped `SessionInput::UiAsync` events for repo-scoped or cancellable tasks (`InitRepo`, `CloneRepo`, `WorktreeSetup`).
- **Why:** Late completions from a previous repo or cancelled clone can hijack the active session and yank the user out of the newly opened repository.

### CS-6.3 Guard rollback undo stacks, auto-stashes, worktree caches, and error retries
- **Do:** Clear `rollback_undo_stack` on commit, git ops, `HEAD` changes, discards, or external invalidations, and verify current disk content before restoring `prev_local_bytes`. Match auto-created stashes strictly by sentinel message (return early if missing; never `.unwrap_or(0)`). Evict `DetailsMode::CompareWithLocal` entries from details/diffs/syntax caches on worktree changes or refresh. On `EngineEvent::Error`, suppress automatic retries for the failed key until selection/snapshot changes or explicit refresh, and only clear `commit_in_progress` on commit completion/failure.
- **Don't:** Blindly overwrite disk from stale undo buffers, fall back to `stash@{0}`, or clear `requested` and immediately call `reconcile_all_requests` on every `EngineEvent::Error`.
- **Why:** Prevents silent data loss on newer files, popping unrelated stashes, stale `CompareWithLocal` diffs, infinite hot retry loops, and duplicate concurrent commits.

### CS-6.4 Enforce applicability, detached-HEAD, preselection, and input guards
- **Do:** Check `is_action_applicable` before executing command-palette (`?`) actions. Treat `RefKind::Head` as `is_current` on detached HEAD and reject rename/delete/checkout/set-upstream targeting literal `"HEAD"`. Preselect right-clicked rows (e.g., `StashChangeRow`) before building context menus without mutating an active visual range when right-clicking inside it. Clamp popup cursor indices, gate bracketed paste on active search/edit state, filter mixed selections to untracked paths for `.gitignore` actions, and surface precondition errors instead of dropping them via `.ok()`.
- **Don't:** Dispatch destructive actions from `?` without scope checks, leave detached `HEAD` deletable, or swallow user-triggered catalog errors silently.
- **Why:** Stops out-of-scope destructive mutations on hidden tabs, wrong-file overwrite on right-click, and broken keyboard/paste interactions.

### CS-6.5 Preserve `needs_redraw`, focus invariants, modal stack order, and debounce deadlines
- **Do:** Save `prev_redraw` before handling `Action::MouseMove` and restore it when `!changed`, fast-pathing no-op moves before `ensure_graph_cache`/`reconcile_all_requests`. Use `self.set_focus(tab, pane)` for all cross-tab navigation. Clear `menu_hover` in `push_modal`, never truncate non-menu modals above a context menu, and remove-then-push re-opened modals so they rise to the top. Clear `max_scroll = None` in `ScrollViewport::reset()`, and verify `at <= ctx.now` before flushing debounced requests.
- **Don't:** Assign `self.active_tab` directly without updating `focused_pane`, reset `needs_redraw = false` over an earlier coalesced redraw, or ignore debounce timestamps when unrelated timers fire.
- **Why:** Prevents dropped frames, keystrokes routing to hidden tabs, disappearing dialogs, clamped jumps on cached diffs, and premature request storms.

### CS-6.6 Maintain 100% i18n parity, conflict-free keymaps, and doc/config alignment
- **Do:** Keep `en-US.toml` and `zh-CN.toml` at 100% key parity—including every `help.action.*` key—format parameterized templates via `t_fmt`, and derive or sync displayed shortcut hints from active keymaps. Verify no higher-priority scope shadows a lower-priority binding across `default`, `jetbrains`, and `vim` presets; keep context-menu mnemonics unique and avoid `'j'`/`'k'` (reserved for menu navigation). Annotate optional string config fields defaulting to `""` with `deserialize_opt_non_empty`, and keep `jbgit.default.toml` and `docs/manual.md` aligned with shipped bindings and feature gates.
- **Don't:** Leave Chinese fallbacks in `en-US` help descriptions, render raw `{placeholder}` strings, hardcode drifted shortcut letters in locale text, or ship shadowed preset bindings.
- **Why:** Guarantees consistent UX across locales, presets, and documentation, and catches regressions automatically in review and tests.

### CS-6.7 Always clear floating overlays via `views::clear_modal_overlay`
- **Do:** Call `crate::views::clear_modal_overlay(frame, rect)` before rendering any floating modal, popup, context menu, tooltip, or which-key overlay. It sanitizes double-width background glyphs (CJK/emoji) straddling `rect.x - 1` and `rect.right()` before invoking `Clear`.
- **Don't:** Call `frame.render_widget(Clear, rect)` directly in modal or popup renderers.
- **Why:** When a 2-column CJK character sits at `(rect.x - 1, y)`, `ratatui`'s `Buffer::diff` emits it and skips `(rect.x, y)`, erasing the overlay's left border (`╭`/`│`).
