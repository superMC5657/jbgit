use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::askpass_http::AskpassHttpServer;

const QA_USER_NAME: &str = "QA Tester";
const QA_USER_EMAIL: &str = "qa@example.com";
const PEER_USER_NAME: &str = "Remote Peer";
const PEER_USER_EMAIL: &str = "peer@example.com";
const LFS_DISABLE_ARGS: &[&str] = &[
    "-c",
    "filter.lfs.required=false",
    "-c",
    "filter.lfs.process=",
    "-c",
    "filter.lfs.clean=",
    "-c",
    "filter.lfs.smudge=",
];

pub struct PreparedSandbox {
    pub repo_dir: PathBuf,
    pub askpass_url: Option<String>,
    pub patches: Option<(PathBuf, PathBuf)>,
    pub clone_source: Option<PathBuf>,
    pub commit_template: Option<PathBuf>,
    pub recent_repos: Vec<PathBuf>,
    temp_cleanup: Option<PathBuf>,
    _askpass_server: Option<AskpassHttpServer>,
}

impl Drop for PreparedSandbox {
    fn drop(&mut self) {
        if let Some(path) = self.temp_cleanup.take() {
            let _ = remove_path_if_exists(&path);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxScenario {
    Basic,
    Full,
    Conflict,
    /// Two repositories + a linked worktree + a recent-repositories list
    /// (Spec 12 #09/#10 repository switching).
    MultiRepo,
    /// Shallow clone + dirty/ahead submodule + Git LFS v1 pointer diff
    /// (Spec 12 #14 repository capabilities).
    Capabilities,
    /// Ephemeral HTTP Basic Auth remote requiring `GIT_ASKPASS` (Spec 12 #08).
    Askpass,
    /// Active `git rebase` conflict with remaining rebase sequence steps
    /// (Spec 13 #02/#05/#09).
    RebaseConflict,
    /// Active `git bisect` session across 8 linear commits (Spec 13 #14).
    Bisect,
    /// Empty non-Git workspace + local bare clone source + recent repo entry
    /// (Spec 13 #13 Welcome / Init / Clone).
    Welcome,
    /// Commit template, missing local identity, and staged CRLF guard fixture
    /// (Spec 13 #07/#13).
    Workflow,
}

impl SandboxScenario {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "basic" => Ok(Self::Basic),
            "full" => Ok(Self::Full),
            "conflict" => Ok(Self::Conflict),
            "multi-repo" | "multirepo" => Ok(Self::MultiRepo),
            "capabilities" | "caps" => Ok(Self::Capabilities),
            "askpass" | "auth" => Ok(Self::Askpass),
            "rebase-conflict" | "rebase" => Ok(Self::RebaseConflict),
            "bisect" => Ok(Self::Bisect),
            "welcome" | "non-git" | "init" => Ok(Self::Welcome),
            "workflow" | "commit-workflow" | "crlf" => Ok(Self::Workflow),
            other => bail!(
                "unknown scenario '{other}' (expected: full, conflict, basic, multi-repo, capabilities, askpass, rebase-conflict, bisect, welcome, workflow)"
            ),
        }
    }
}

pub(crate) fn write_custom_test_config(dir: &Path) -> Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    fs::create_dir_all(dir)?;

    fs::write(
        dir.join("jbgit.toml"),
        r#"
[general]
language = "en-US"
default_tab = "log"

[keymap]
preset = "default"
footer_max_lines = 2

[layout]
log_ratio = [ 1, 3, 2 ]
commit_ratio = [ 1, 2 ]
file_history_ratio = [ 1, 2 ]
file_history_vertical_ratio = [ 1, 2 ]
"#,
    )?;

    fs::write(
        dir.join("theme.toml"),
        r##"
flavor = "catppuccin-mocha"

[glyphs]
mode = "unicode"

[palette]
bg_canvas = "#112233"
bg_panel = "#223344"
border_focused = "#FF6600"
"##,
    )?;

    fs::create_dir_all(dir.join("keymaps"))?;
    fs::write(
        dir.join("keymaps/default.toml"),
        r#"
[global]
prepend_keymap = [
    { on = [ "R" ], run = "noop" },
    { on = [ "B" ], run = "noop" },
    { on = [ "<F8>" ], run = "refresh", show_hint = true },
    { on = [ "<F6>" ], run = "toggle_branches_popup", show_hint = true },
    { on = [ "g", "x" ], run = "refresh" },
]

[log]
prepend_keymap = [
    { on = [ "n" ], run = "noop" },
    { on = [ "<F7>" ], run = "new_branch", show_hint = true },
]

[branch]
prepend_keymap = [
    { on = [ "n" ], run = "noop" },
    { on = [ "<F7>" ], run = "new_branch", show_hint = true },
]

[commit]
prepend_keymap = [
    { on = [ "<C-s>" ], run = "noop" },
    { on = [ "<F9>" ], run = "commit", show_hint = true },
]
"#,
    )?;
    Ok(())
}

/// Sibling directories of the multi-repo scenario: `(worktree, second repo)`.
fn multi_repo_siblings(dir: &Path) -> (PathBuf, PathBuf) {
    let name = dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("sandbox-repo");
    let parent = dir.parent().unwrap_or(Path::new("."));
    (
        parent.join(format!("{name}-feature")),
        parent.join(format!("{name}-b")),
    )
}

fn capabilities_sub_repo(dir: &Path) -> PathBuf {
    dir.parent()
        .unwrap_or(Path::new("."))
        .join("sandbox-sub.git")
}

fn askpass_http_root(dir: &Path) -> PathBuf {
    dir.parent()
        .unwrap_or(Path::new("."))
        .join("sandbox-http-root")
}

fn full_patch_paths(dir: &Path) -> (PathBuf, PathBuf) {
    let parent = dir.parent().unwrap_or(Path::new("."));
    (parent.join("sample.patch"), parent.join("conflict.patch"))
}

fn make_writable(path: &Path) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    let mut perms = meta.permissions();
    if perms.readonly() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(perms.mode() | 0o200);
        }
        #[cfg(not(unix))]
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = fs::set_permissions(path, perms);
    }
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        if fs::remove_dir_all(path).is_err() {
            clear_readonly_recursive(path);
            fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))?;
        }
    } else if fs::remove_file(path).is_err() {
        make_writable(path);
        fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
    }
    Ok(())
}

fn clear_readonly_recursive(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        make_writable(&p);
        if p.is_dir() {
            clear_readonly_recursive(&p);
        }
    }
}

fn cleanup_sandbox_artifacts(dir: &Path, remote_dir: &Path) -> Result<()> {
    let parent = dir.parent().unwrap_or(Path::new("."));
    let (wt_dir, b_dir) = multi_repo_siblings(dir);
    let (sample_patch, conflict_patch) = full_patch_paths(dir);
    for path in [
        wt_dir,
        b_dir,
        capabilities_sub_repo(dir),
        askpass_http_root(dir),
        parent.join("sandbox-peer"),
        parent.join("sandbox-seed"),
        parent.join("sandbox-sub-seed"),
        parent.join("sandbox-empty"),
        parent.join("sandbox-clone-src.git"),
        parent.join("sandbox-recent"),
        sample_patch,
        conflict_patch,
        dir.to_path_buf(),
        remote_dir.to_path_buf(),
    ] {
        remove_path_if_exists(&path)?;
    }
    Ok(())
}

pub fn prepare_sandbox(session_dir: &Path, scenario: SandboxScenario) -> Result<PreparedSandbox> {
    let session_dir = std::path::absolute(session_dir)?;
    let dir = session_dir.join("sandbox-repo");
    let remote_dir = session_dir.join("sandbox-remote.git");
    cleanup_sandbox_artifacts(&dir, &remote_dir)?;

    let mut prepared = PreparedSandbox {
        repo_dir: dir.clone(),
        askpass_url: None,
        patches: None,
        clone_source: None,
        commit_template: None,
        recent_repos: Vec::new(),
        temp_cleanup: None,
        _askpass_server: None,
    };

    match scenario {
        SandboxScenario::Basic => {
            init_git_repo(&dir, "main")?;
            init_basic_scenario(&dir)?;
        }
        SandboxScenario::Conflict => {
            init_git_repo(&dir, "main")?;
            init_conflict_scenario(&dir)?;
        }
        SandboxScenario::Full => {
            init_git_repo(&dir, "main")?;
            prepared.patches = Some(init_full_scenario(&dir, &remote_dir)?);
        }
        SandboxScenario::MultiRepo => {
            init_git_repo(&dir, "main")?;
            let (wt_dir, b_dir) = multi_repo_siblings(&dir);
            init_multi_repo_scenario(&dir, &wt_dir, &b_dir)?;
            prepared.recent_repos.push(b_dir);
        }
        SandboxScenario::Capabilities => {
            init_capabilities_scenario(&dir, &remote_dir)?;
        }
        SandboxScenario::Askpass => {
            let http_root = askpass_http_root(&dir);
            fs::create_dir_all(&http_root)?;
            let server = AskpassHttpServer::start(http_root.clone())?;
            let url = server.repo_url();
            init_askpass_scenario(&dir, &http_root, &url)?;
            prepared.askpass_url = Some(url);
            prepared._askpass_server = Some(server);
        }
        SandboxScenario::RebaseConflict => {
            init_git_repo(&dir, "main")?;
            init_rebase_conflict_scenario(&dir)?;
        }
        SandboxScenario::Bisect => {
            init_git_repo(&dir, "main")?;
            init_bisect_scenario(&dir)?;
        }
        SandboxScenario::Welcome => {
            let (empty_dir, clone_src, recent_dir, temp_cleanup) =
                init_welcome_scenario(&session_dir)?;
            prepared.repo_dir = empty_dir;
            prepared.clone_source = Some(clone_src);
            prepared.recent_repos.push(recent_dir);
            prepared.temp_cleanup = temp_cleanup;
        }
        SandboxScenario::Workflow => {
            init_git_repo(&dir, "main")?;
            prepared.commit_template = Some(init_workflow_scenario(&dir)?);
        }
    }

    Ok(prepared)
}

/// `sandbox-repo` (basic scenario) with `feature/ui-test` checked out in the
/// linked worktree `sandbox-repo-feature`, plus an unrelated repository
/// `sandbox-repo-b` on `develop` with its own uncommitted change.
fn init_multi_repo_scenario(dir: &Path, wt_dir: &Path, b_dir: &Path) -> Result<()> {
    init_basic_scenario(dir)?;
    let wt = wt_dir.to_string_lossy().into_owned();
    run_git(dir, &["worktree", "add", &wt, "feature/ui-test"])?;
    fs::write(
        wt_dir.join("WORKTREE_NOTE.md"),
        "edited only in the worktree\n",
    )?;

    init_git_repo(b_dir, "develop")?;
    fs::write(b_dir.join("b.txt"), "repository B\n")?;
    run_git(b_dir, &["add", "b.txt"])?;
    run_git(b_dir, &["commit", "-m", "B: initial commit"])?;
    fs::write(b_dir.join("b.txt"), "repository B (modified)\n")?;
    Ok(())
}

/// Seeds `recent-repos.toml` in the isolated config dir with `repos`
/// (newest first) so the repository switcher (`Alt+O`) lists them.
pub fn write_recent_repos(cfg_dir: &Path, repos: &[PathBuf]) -> Result<()> {
    fs::create_dir_all(cfg_dir)?;
    let mut body = String::new();
    for (i, repo) in repos.iter().enumerate() {
        let name = repo.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
        // TOML literal string: no escaping of Windows backslashes needed.
        let path = repo.to_string_lossy();
        body.push_str(&format!(
            "[[repos]]\npath = '{path}'\nname = '{name}'\nlast_opened = {}\n\n",
            1_900_000_000 - i as i64
        ));
    }
    fs::write(cfg_dir.join("recent-repos.toml"), body)?;
    Ok(())
}

fn init_basic_scenario(dir: &Path) -> Result<()> {
    fs::write(
        dir.join("README.md"),
        "# jbgit QA Sandbox\n\nline 1\nline 2\n",
    )?;
    run_git(dir, &["add", "README.md"])?;
    run_git(dir, &["commit", "-m", "initial commit"])?;

    fs::create_dir_all(dir.join("src"))?;
    fs::write(
        dir.join("src/main.rs"),
        "fn main() {\n    println!(\"hello jbgit\");\n}\n",
    )?;
    run_git(dir, &["add", "src/main.rs"])?;
    run_git(dir, &["commit", "-m", "add main.rs"])?;

    run_git(dir, &["branch", "feature/ui-test"])?;
    run_git(dir, &["branch", "bugfix/scroll"])?;
    run_git(dir, &["tag", "v0.1.0"])?;

    fs::write(
        dir.join("src/main.rs"),
        "fn main() {\n    println!(\"hello jbgit modified\");\n}\n",
    )?;
    fs::write(dir.join("scratch_note.txt"), "untracked qa file\n")?;
    Ok(())
}

fn init_conflict_scenario(dir: &Path) -> Result<()> {
    let base_app = "\
line 01: header\n\
line 02: config\n\
line 03: keep A\n\
line 04: keep B\n\
line 05: conflict slot\n\
line 06: keep C\n\
line 07: keep D\n\
line 08: keep E\n\
line 09: auto-merge slot\n\
line 10: footer\n";
    fs::write(dir.join("conflict_app.txt"), base_app)?;
    fs::write(dir.join("conflict_second.txt"), "second base\n")?;
    fs::write(dir.join("deleted_by_theirs.txt"), "base content\n")?;
    fs::write(dir.join("README.md"), "# Conflict QA Repo\n")?;
    run_git(dir, &["add", "."])?;
    run_git(dir, &["commit", "-m", "base commit"])?;

    run_git(dir, &["checkout", "-b", "feature/theirs"])?;
    let theirs_app = "\
line 01: header\n\
line 02: config\n\
line 03: keep A\n\
line 04: keep B\n\
line 05: theirs conflict edit\n\
line 06: keep C\n\
line 07: keep D\n\
line 08: keep E\n\
line 09: auto-merge theirs only\n\
line 10: footer\n";
    fs::write(dir.join("conflict_app.txt"), theirs_app)?;
    fs::write(dir.join("conflict_second.txt"), "second theirs\n")?;
    run_git(dir, &["rm", "--", "deleted_by_theirs.txt"])?;
    run_git(dir, &["commit", "-am", "theirs changes"])?;

    run_git(dir, &["checkout", "main"])?;
    let ours_app = "\
line 01: header\n\
line 02: config\n\
line 03: keep A\n\
line 04: keep B\n\
line 05: ours conflict edit\n\
line 06: keep C\n\
line 07: keep D\n\
line 08: keep E\n\
line 09: auto-merge slot\n\
line 10: footer\n";
    fs::write(dir.join("conflict_app.txt"), ours_app)?;
    fs::write(dir.join("conflict_second.txt"), "second ours\n")?;
    fs::write(dir.join("deleted_by_theirs.txt"), "ours modified\n")?;
    run_git(dir, &["commit", "-am", "ours changes"])?;

    let out = Command::new("git")
        .args(["merge", "--", "feature/theirs"])
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", QA_USER_NAME)
        .env("GIT_AUTHOR_EMAIL", QA_USER_EMAIL)
        .env("GIT_COMMITTER_NAME", QA_USER_NAME)
        .env("GIT_COMMITTER_EMAIL", QA_USER_EMAIL)
        .output()?;
    if out.status.success() {
        bail!("expected merge conflict in Conflict scenario, but merge succeeded");
    }
    Ok(())
}

fn init_full_scenario(dir: &Path, remote_dir: &Path) -> Result<(PathBuf, PathBuf)> {
    fs::create_dir_all(remote_dir)?;
    run_git(remote_dir, &["init", "--bare", "-b", "main"])?;

    fs::write(dir.join("README.md"), "# jbgit Full QA Sandbox\n\nline 1\n")?;
    fs::create_dir_all(dir.join("src/components"))?;
    let mut src_lines = Vec::new();
    for i in 1..=30 {
        src_lines.push(format!("fn step_{i:02}() {{ println!(\"step {i}\"); }}"));
    }
    fs::write(
        dir.join("src/main.rs"),
        format!("{}\n", src_lines.join("\n")),
    )?;
    fs::write(
        dir.join("src/legacy_auth.rs"),
        "pub fn verify_user(user: &str) -> bool {\n    !user.is_empty()\n}\n",
    )?;
    fs::write(
        dir.join("src/components/header.rs"),
        "pub fn title() -> &'static str { \"jbgit\" }\n",
    )?;
    fs::create_dir_all(dir.join("assets"))?;
    fs::write(
        dir.join("assets/logo.bin"),
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01",
    )?;
    let wide_base = format!(
        "pub fn indented_check() {{\n    let alpha = 1;\n}}\n\npub const WIDE_SPEC13_LINE: &str = \"{}_TAIL_BASE\";\n",
        "X".repeat(130)
    );
    fs::write(dir.join("src/wide_lines.rs"), &wide_base)?;
    fs::write(dir.join(".gitignore"), "*.log\n")?;
    fs::write(
        dir.join("staged_partial.txt"),
        "alpha\nbeta\ngamma\ndelta\n",
    )?;
    run_git(dir, &["add", "."])?;
    run_git(dir, &["commit", "-m", "initial project scaffold"])?;
    run_git(dir, &["tag", "v0.1.0"])?;

    fs::write(
        dir.join("src/legacy_auth.rs"),
        "pub fn verify_user(user: &str) -> bool {\n    !user.is_empty()\n}\n\npub fn token_ttl() -> u64 {\n    3600\n}\n",
    )?;
    run_git(
        dir,
        &["commit", "-am", "feat: add token_ttl to legacy_auth"],
    )?;
    run_git(dir, &["mv", "src/legacy_auth.rs", "src/auth.rs"])?;
    run_git(
        dir,
        &["commit", "-m", "refactor: rename legacy_auth.rs to auth.rs"],
    )?;

    run_git(
        dir,
        &["remote", "add", "origin", &remote_dir.to_string_lossy()],
    )?;
    run_git(dir, &["push", "-u", "origin", "main"])?;
    run_git(dir, &["push", "origin", "v0.1.0"])?;

    run_git(dir, &["checkout", "-b", "feature/remote-only"])?;
    fs::write(dir.join("remote_note.md"), "remote-only commit\n")?;
    run_git(dir, &["add", "remote_note.md"])?;
    run_git(dir, &["commit", "-m", "feat: remote-only branch work"])?;
    run_git(dir, &["push", "-u", "origin", "feature/remote-only"])?;

    run_git(dir, &["checkout", "main"])?;
    run_git(dir, &["branch", "-D", "feature/remote-only"])?;
    run_git(dir, &["checkout", "-b", "feature/ui-cards"])?;
    fs::write(
        dir.join("src/cards.rs"),
        "pub fn card() -> &'static str { \"card\" }\n",
    )?;
    run_git(dir, &["add", "src/cards.rs"])?;
    run_git(dir, &["commit", "-m", "feat: add ui cards module"])?;

    run_git(dir, &["checkout", "main"])?;
    run_git(
        dir,
        &[
            "merge",
            "--no-ff",
            "feature/ui-cards",
            "-m",
            "merge feature/ui-cards",
        ],
    )?;

    run_git(dir, &["checkout", "-b", "feature/linear-chain"])?;
    for step in 1..=5 {
        fs::write(
            dir.join("linear_chain.txt"),
            format!("linear step {step}\n"),
        )?;
        run_git(dir, &["add", "linear_chain.txt"])?;
        run_git(
            dir,
            &["commit", "-m", &format!("chore(linear): step {step}")],
        )?;
    }

    run_git(dir, &["checkout", "main"])?;
    run_git(dir, &["checkout", "-b", "feature/login"])?;
    fs::write(
        dir.join("src/auth.rs"),
        "pub fn verify_user(user: &str) -> bool {\n    !user.is_empty()\n}\n\npub fn token_ttl() -> u64 {\n    3600\n}\n\npub fn login() -> bool {\n    true\n}\n",
    )?;
    run_git(dir, &["add", "src/auth.rs"])?;
    run_git(dir, &["commit", "-m", "feat: implement oauth login"])?;

    run_git(dir, &["checkout", "main"])?;
    run_git(dir, &["branch", "bugfix/scroll-edge"])?;

    run_git(dir, &["checkout", "-b", "feature/pull-me"])?;
    fs::write(dir.join("pull_me.txt"), "base\n")?;
    run_git(dir, &["add", "pull_me.txt"])?;
    run_git(dir, &["commit", "-m", "chore: pull-me base"])?;
    run_git(dir, &["push", "-u", "origin", "feature/pull-me"])?;

    run_git(dir, &["checkout", "-b", "feature/diverged", "main"])?;
    fs::write(dir.join("diverged.txt"), "base\n")?;
    run_git(dir, &["add", "diverged.txt"])?;
    run_git(dir, &["commit", "-m", "chore: diverged base"])?;
    run_git(dir, &["push", "-u", "origin", "feature/diverged"])?;
    fs::write(dir.join("diverged.txt"), "local side change\n")?;
    run_git(dir, &["commit", "-am", "feat: local diverged commit"])?;

    let peer_dir = dir
        .parent()
        .map(|p| p.join("sandbox-peer"))
        .unwrap_or_else(|| PathBuf::from("sandbox-peer"));
    if peer_dir.exists() {
        fs::remove_dir_all(&peer_dir)?;
    }
    run_git(
        dir.parent().unwrap_or(dir),
        &[
            "clone",
            &remote_dir.to_string_lossy(),
            &peer_dir.to_string_lossy(),
        ],
    )?;
    configure_git_user(&peer_dir, PEER_USER_NAME, PEER_USER_EMAIL)?;
    run_git(&peer_dir, &["checkout", "feature/pull-me"])?;
    fs::write(peer_dir.join("pull_me.txt"), "base\nremote 1\n")?;
    run_git(&peer_dir, &["commit", "-am", "remote: pull commit 1"])?;
    fs::write(peer_dir.join("pull_me.txt"), "base\nremote 1\nremote 2\n")?;
    run_git(&peer_dir, &["commit", "-am", "remote: pull commit 2"])?;
    run_git(&peer_dir, &["push", "origin", "feature/pull-me"])?;

    run_git(&peer_dir, &["checkout", "feature/diverged"])?;
    fs::write(peer_dir.join("diverged.txt"), "remote side change\n")?;
    run_git(&peer_dir, &["commit", "-am", "remote: diverged commit"])?;
    run_git(&peer_dir, &["push", "origin", "feature/diverged"])?;
    let _ = fs::remove_dir_all(&peer_dir);

    run_git(dir, &["checkout", "main"])?;

    let (sample_patch, conflict_patch) = full_patch_paths(dir);
    // Capture conflict.patch against the committed base blob before main diverges
    // on `src/components/header.rs` so `git apply --check` fails and `--3way` enters
    // the 3-way conflict-resolution flow.
    fs::write(
        dir.join("src/components/header.rs"),
        "pub fn title() -> &'static str { \"jbgit patch\" }\n",
    )?;
    let conflict_diff = run_git_stdout(
        dir,
        &[
            "-c",
            "core.autocrlf=false",
            "diff",
            "--",
            "src/components/header.rs",
        ],
    )?;
    fs::write(&conflict_patch, &conflict_diff)?;
    run_git(dir, &["checkout", "--", "src/components/header.rs"])?;

    fs::write(
        dir.join("README.md"),
        "# jbgit Full QA Sandbox\n\nline 1\nupdated on main\n",
    )?;
    fs::write(
        dir.join("src/auth.rs"),
        "pub fn verify_user(user: &str) -> bool {\n    !user.trim().is_empty()\n}\n\npub fn token_ttl() -> u64 {\n    3600\n}\n",
    )?;
    fs::write(
        dir.join("src/components/header.rs"),
        "pub fn title() -> &'static str { \"jbgit TUI\" }\n",
    )?;
    run_git(
        dir,
        &[
            "commit",
            "-am",
            "feat: refine auth, header, and README on main",
        ],
    )?;
    run_git(
        dir,
        &["tag", "-a", "v0.2.0-rc1", "-m", "Release candidate 1"],
    )?;

    // Unmerged comparison & cherry-pick highlight fixture (Spec 13 #06/#11).
    run_git(dir, &["checkout", "-b", "feature/cherry-source"])?;
    run_git(dir, &["cherry-pick", "--", "feature/login"])?;
    fs::write(
        dir.join("src/cherry_helper.rs"),
        "pub fn cherry_helper() -> bool {\n    true\n}\n",
    )?;
    run_git(dir, &["add", "src/cherry_helper.rs"])?;
    run_git(dir, &["commit", "-m", "feat: cherry-source helper"])?;
    run_git(dir, &["checkout", "main"])?;

    fs::write(
        &sample_patch,
        "diff --git a/src/cards.rs b/src/cards.rs\n--- a/src/cards.rs\n+++ b/src/cards.rs\n@@ -1 +1,2 @@\n pub fn card() -> &'static str { \"card\" }\n+pub fn card_badge() -> &'static str { \"badge\" }\n",
    )?;

    fs::write(
        dir.join("README.md"),
        "# jbgit Full QA Sandbox\n\nstash one edit\n",
    )?;
    run_git(dir, &["stash", "push", "-m", "wip: older stash"])?;
    fs::write(
        dir.join("README.md"),
        "# jbgit Full QA Sandbox\n\nstash two edit\n",
    )?;
    run_git(dir, &["stash", "push", "-m", "wip: refactor stash test"])?;

    fs::write(
        dir.join("staged_partial.txt"),
        "alpha-staged\nbeta\ngamma\ndelta\n",
    )?;
    run_git(dir, &["add", "staged_partial.txt"])?;
    fs::write(
        dir.join("staged_partial.txt"),
        "alpha-staged\nbeta\ngamma\ndelta-unstaged\n",
    )?;

    src_lines[1] = "fn step_02() { println!(\"step 2 modified hunk A\"); }".to_string();
    src_lines[26] = "fn step_27() { println!(\"step 27 modified hunk B\"); }".to_string();
    fs::write(
        dir.join("src/main.rs"),
        format!("{}\n", src_lines.join("\n")),
    )?;

    // Binary diff card + whitespace/horizontal-scroll/search diff fixtures (Spec 13 #04).
    fs::write(
        dir.join("assets/logo.bin"),
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x02\x00\xff\x7f",
    )?;
    let wide_modified = format!(
        "pub fn indented_check() {{\n        let alpha = 1;\n}}\n\npub const WIDE_SPEC13_LINE: &str = \"{}_TAIL_EDITED\";\n",
        "X".repeat(130)
    );
    fs::write(dir.join("src/wide_lines.rs"), wide_modified)?;

    fs::write(dir.join("scratch_note.txt"), "untracked qa file\n")?;
    fs::write(dir.join("debug_trace.log"), "ignored debug log\n")?;
    Ok((sample_patch, conflict_patch))
}

/// Shallow clone (`--depth 1`) + dirty/ahead submodule + Git LFS v1 pointer diff
/// + synthetic `gpgsig` signed commit for testing Spec 12 #14 repository capabilities.
fn init_capabilities_scenario(dir: &Path, remote_dir: &Path) -> Result<()> {
    let parent = dir.parent().unwrap_or(Path::new("."));
    let seed_dir = parent.join("sandbox-seed");
    let sub_seed_dir = parent.join("sandbox-sub-seed");
    let sub_repo = capabilities_sub_repo(dir);

    // 1. Bare upstream repository with 4 commits so shallow clone leaves 3 unloaded ancestors.
    fs::create_dir_all(remote_dir)?;
    run_git(remote_dir, &["init", "--bare", "-b", "main"])?;
    init_git_repo(&seed_dir, "main")?;

    fs::write(seed_dir.join("README.md"), "# Capabilities QA Sandbox\n")?;
    run_git(&seed_dir, &["add", "README.md"])?;
    run_git(&seed_dir, &["commit", "-m", "init: capabilities sandbox"])?;

    fs::write(
        seed_dir.join(".gitattributes"),
        "*.bin filter=lfs diff=lfs merge=lfs -text\n",
    )?;
    fs::create_dir_all(seed_dir.join("assets"))?;
    let lfs_v1 = format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize 1048576\n",
        "1".repeat(64)
    );
    fs::write(seed_dir.join("assets/model.bin"), lfs_v1)?;
    run_git_no_lfs(&seed_dir, &["add", ".gitattributes", "assets/model.bin"])?;
    run_git(
        &seed_dir,
        &["commit", "-m", "feat: track model.bin with LFS"],
    )?;

    fs::create_dir_all(seed_dir.join("src"))?;
    fs::write(
        seed_dir.join("src/lib.rs"),
        "pub fn ok() -> bool {\n    true\n}\n",
    )?;
    run_git(&seed_dir, &["add", "src/lib.rs"])?;
    run_git(&seed_dir, &["commit", "-m", "feat: add src/lib.rs"])?;

    fs::write(
        seed_dir.join("README.md"),
        "# Capabilities QA Sandbox\nupdated upstream\n",
    )?;
    run_git(&seed_dir, &["commit", "-am", "docs: update README"])?;
    run_git(
        &seed_dir,
        &["remote", "add", "origin", &remote_dir.to_string_lossy()],
    )?;
    run_git(&seed_dir, &["push", "--no-verify", "-u", "origin", "main"])?;
    remove_path_if_exists(&seed_dir)?;

    // 2. Bare submodule repository with 2 commits.
    fs::create_dir_all(&sub_repo)?;
    run_git(&sub_repo, &["init", "--bare", "-b", "main"])?;
    init_git_repo(&sub_seed_dir, "main")?;
    fs::write(sub_seed_dir.join("sub.txt"), "sub base\n")?;
    run_git(&sub_seed_dir, &["add", "sub.txt"])?;
    run_git(&sub_seed_dir, &["commit", "-m", "sub: base"])?;
    fs::write(sub_seed_dir.join("sub.txt"), "sub v2\n")?;
    run_git(&sub_seed_dir, &["commit", "-am", "sub: v2"])?;
    run_git(
        &sub_seed_dir,
        &["remote", "add", "origin", &sub_repo.to_string_lossy()],
    )?;
    run_git(&sub_seed_dir, &["push", "-u", "origin", "main"])?;
    remove_path_if_exists(&sub_seed_dir)?;

    // 3. Shallow clone into `dir` (`--no-local --depth 1` preserves `.git/shallow`).
    run_git_no_lfs(
        parent,
        &[
            "-c",
            "protocol.file.allow=always",
            "clone",
            "--no-local",
            "--depth",
            "1",
            &remote_dir.to_string_lossy(),
            &dir.to_string_lossy(),
        ],
    )?;
    configure_git_user(dir, QA_USER_NAME, QA_USER_EMAIL)?;

    // 4. Add submodule and make it both ahead (`new commits`) and dirty.
    run_git(
        dir,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &sub_repo.to_string_lossy(),
            "libs/submod",
        ],
    )?;
    run_git(dir, &["commit", "-m", "chore: add libs/submod submodule"])?;

    let sub_work = dir.join("libs/submod");
    configure_git_user(&sub_work, QA_USER_NAME, QA_USER_EMAIL)?;
    fs::write(sub_work.join("sub.txt"), "sub v2\nlocal submodule commit\n")?;
    run_git(&sub_work, &["commit", "-am", "sub: local advance"])?;
    fs::write(
        sub_work.join("sub.txt"),
        "sub v2\nlocal submodule commit\nuncommitted dirty line\n",
    )?;

    // 5. Add a local commit carrying a synthetic PGP `gpgsig` header so `%G?`
    // classifies HEAD as signed (CannotCheck/Bad) while HEAD~1 stays Unsigned.
    fs::write(
        dir.join("src/lib.rs"),
        "pub fn ok() -> bool {\n    true\n}\n\npub fn signed_fixture() -> bool {\n    true\n}\n",
    )?;
    run_git(dir, &["add", "src/lib.rs"])?;
    run_git(dir, &["commit", "-m", "chore: signed commit fixture"])?;
    attach_synthetic_gpgsig_to_head(dir)?;

    // 6. Modify LFS pointer file in worktree (no git-lfs filter binary needed).
    let lfs_v2 = format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize 2097152\n",
        "2".repeat(64)
    );
    fs::write(dir.join("assets/model.bin"), lfs_v2)?;
    Ok(())
}

fn attach_synthetic_gpgsig_to_head(dir: &Path) -> Result<()> {
    // Structurally valid RFC 4880 V4 OpenPGP signature packet (issuer key ID
    // DEADBEEFCAFEBABE) with valid CRC-24 armor checksum, so `git log --format=%G?`
    // reports `E` (CannotCheck / unknown public key) and `%GK = DEADBEEFCAFEBABE`
    // without requiring GPG keys on the machine.
    const SYNTHETIC_GPGSIG: &str = concat!(
        "gpgsig -----BEGIN PGP SIGNATURE-----\n",
        " \n",
        " iQCcBAABCAAGBQJlAAAAAAoJEN6tvu/K/rq+AAAEAIAAAAAAAAAAAAAAAAAAAAAA\n",
        " AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n",
        " AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n",
        " AAAAAAAAAAAAAAAAAAAA\n",
        " =+xN9\n",
        " -----END PGP SIGNATURE-----",
    );

    let raw_bytes = run_git_stdout(dir, &["cat-file", "commit", "HEAD"])?;
    let raw = String::from_utf8(raw_bytes).context("commit object is not UTF-8")?;
    let (headers, rest) = raw
        .split_once("\n\n")
        .context("unexpected commit object format without header/body separator")?;
    let signed_obj = format!("{headers}\n{SYNTHETIC_GPGSIG}\n\n{rest}");
    let oid_bytes = run_git_with_stdin(
        dir,
        &["hash-object", "-t", "commit", "-w", "--stdin"],
        signed_obj.as_bytes(),
    )?;
    let new_oid = String::from_utf8(oid_bytes)
        .context("hash-object output is not UTF-8")?
        .trim()
        .to_string();
    run_git(dir, &["update-ref", "HEAD", &new_oid])?;
    Ok(())
}

/// Initializes a local repo backed by `http_root/repo.git` served over HTTP Basic Auth
/// so Fetch / Push exercise the live `GIT_ASKPASS` credential dialog (Spec 12 #08).
fn init_askpass_scenario(dir: &Path, http_root: &Path, http_url: &str) -> Result<()> {
    let bare = http_root.join("repo.git");
    fs::create_dir_all(&bare)?;
    run_git(&bare, &["init", "--bare", "-b", "main"])?;
    run_git(&bare, &["config", "http.receivepack", "true"])?;

    init_git_repo(dir, "main")?;
    fs::write(dir.join("README.md"), "# Askpass QA Sandbox\n")?;
    run_git(dir, &["add", "README.md"])?;
    run_git(dir, &["commit", "-m", "initial commit"])?;
    run_git(
        dir,
        &["push", &bare.to_string_lossy(), "main:refs/heads/main"],
    )?;

    run_git(dir, &["remote", "add", "origin", http_url])?;
    run_git(dir, &["config", "--local", "branch.main.remote", "origin"])?;
    run_git(
        dir,
        &["config", "--local", "branch.main.merge", "refs/heads/main"],
    )?;
    run_git(dir, &["config", "--local", "credential.helper", ""])?;
    run_git(dir, &["config", "--local", "http.emptyAuth", "false"])?;
    run_git(dir, &["update-ref", "refs/remotes/origin/main", "HEAD"])?;

    // Push 1 un-fetched commit directly to the bare repo so Fetch triggers askpass.
    let parent = dir.parent().unwrap_or(Path::new("."));
    let peer_dir = parent.join("sandbox-peer");
    remove_path_if_exists(&peer_dir)?;
    run_git(
        parent,
        &[
            "clone",
            &bare.to_string_lossy(),
            &peer_dir.to_string_lossy(),
        ],
    )?;
    configure_git_user(&peer_dir, PEER_USER_NAME, PEER_USER_EMAIL)?;
    fs::write(
        peer_dir.join("README.md"),
        "# Askpass QA Sandbox\nremote auth commit\n",
    )?;
    run_git(&peer_dir, &["commit", "-am", "remote: auth-gated commit"])?;
    run_git(&peer_dir, &["push", "origin", "main"])?;
    remove_path_if_exists(&peer_dir)?;

    // Also create a local branch ready to push over HTTP Basic Auth.
    run_git(dir, &["checkout", "-b", "feature/auth-push"])?;
    fs::write(dir.join("auth_push.txt"), "push over http basic auth\n")?;
    run_git(dir, &["add", "auth_push.txt"])?;
    run_git(dir, &["commit", "-m", "feat: push over basic auth"])?;
    run_git(dir, &["checkout", "main"])?;
    Ok(())
}

/// Active `git rebase main` stopped on a conflict in `rebase_target.txt` with
/// 1 remaining commit in the rebase sequence (Spec 13 #02/#05/#09).
fn init_rebase_conflict_scenario(dir: &Path) -> Result<()> {
    fs::write(dir.join("rebase_target.txt"), "base line\n")?;
    fs::write(dir.join("README.md"), "# Rebase Conflict QA Repo\n")?;
    run_git(dir, &["add", "."])?;
    run_git(dir, &["commit", "-m", "base commit"])?;

    run_git(dir, &["checkout", "-b", "feature/rebase-me"])?;
    fs::write(dir.join("rebase_target.txt"), "feature branch change 1\n")?;
    run_git(dir, &["commit", "-am", "feat: branch change 1"])?;
    fs::write(dir.join("rebase_followup.txt"), "second rebased commit\n")?;
    run_git(dir, &["add", "rebase_followup.txt"])?;
    run_git(dir, &["commit", "-m", "feat: branch followup"])?;

    run_git(dir, &["checkout", "main"])?;
    fs::write(dir.join("rebase_target.txt"), "upstream main change\n")?;
    run_git(dir, &["commit", "-am", "main: upstream edit"])?;

    run_git(dir, &["checkout", "feature/rebase-me"])?;
    let out = Command::new("git")
        .args(["rebase", "--", "main"])
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", QA_USER_NAME)
        .env("GIT_AUTHOR_EMAIL", QA_USER_EMAIL)
        .env("GIT_COMMITTER_NAME", QA_USER_NAME)
        .env("GIT_COMMITTER_EMAIL", QA_USER_EMAIL)
        .output()?;
    if out.status.success() {
        bail!("expected rebase conflict in RebaseConflict scenario, but rebase succeeded");
    }
    Ok(())
}

/// Active `git bisect` session across 8 linear commits on `main` (Spec 13 #14).
fn init_bisect_scenario(dir: &Path) -> Result<()> {
    for step in 1..=8 {
        let label = if step == 1 {
            "good"
        } else if step == 8 {
            "bad"
        } else {
            "step"
        };
        fs::write(dir.join("bisect_log.txt"), format!("bisect step {step}\n"))?;
        run_git(dir, &["add", "bisect_log.txt"])?;
        run_git(
            dir,
            &["commit", "-m", &format!("bisect: step {step} ({label})")],
        )?;
    }
    let root_bytes = run_git_stdout(dir, &["rev-list", "--max-parents=0", "HEAD"])?;
    let root_oid = String::from_utf8(root_bytes)
        .context("root oid is not UTF-8")?
        .trim()
        .to_string();
    run_git(dir, &["bisect", "start", "HEAD", &root_oid])?;
    Ok(())
}

/// Empty non-Git workspace + local bare clone source + recent repo entry
/// (Spec 13 #13 Welcome / Init / Clone).
fn init_welcome_scenario(
    session_dir: &Path,
) -> Result<(PathBuf, PathBuf, PathBuf, Option<PathBuf>)> {
    let has_git_ancestor = session_dir
        .ancestors()
        .any(|ancestor| ancestor.join(".git").exists());
    let (empty_dir, temp_cleanup) = if has_git_ancestor {
        // Place the empty workspace outside any parent `.git` (e.g. when
        // `session_dir` is `.pilot/` inside the jGit repository) so
        // `Repository::discover` opens the Welcome modal instead of jGit.
        let root = std::env::temp_dir().join(format!("jbgit-pilot-welcome-{}", std::process::id()));
        remove_path_if_exists(&root)?;
        let workspace = root.join("sandbox-empty");
        fs::create_dir_all(&workspace)?;
        (workspace, Some(root))
    } else {
        let workspace = session_dir.join("sandbox-empty");
        fs::create_dir_all(&workspace)?;
        (workspace, None)
    };

    let clone_src = session_dir.join("sandbox-clone-src.git");
    fs::create_dir_all(&clone_src)?;
    run_git(&clone_src, &["init", "--bare", "-b", "main"])?;

    let seed_dir = session_dir.join("sandbox-seed");
    init_git_repo(&seed_dir, "main")?;
    fs::write(seed_dir.join("README.md"), "# Clone Source Repo\n")?;
    run_git(&seed_dir, &["add", "README.md"])?;
    run_git(&seed_dir, &["commit", "-m", "init: clone source"])?;
    fs::write(seed_dir.join("hello.txt"), "cloned from local bare repo\n")?;
    run_git(&seed_dir, &["add", "hello.txt"])?;
    run_git(&seed_dir, &["commit", "-m", "feat: add hello.txt"])?;
    run_git(
        &seed_dir,
        &["remote", "add", "origin", &clone_src.to_string_lossy()],
    )?;
    run_git(&seed_dir, &["push", "-u", "origin", "main"])?;
    remove_path_if_exists(&seed_dir)?;

    let recent_dir = session_dir.join("sandbox-recent");
    init_git_repo(&recent_dir, "main")?;
    fs::write(recent_dir.join("README.md"), "# Recent Repo\n")?;
    run_git(&recent_dir, &["add", "README.md"])?;
    run_git(&recent_dir, &["commit", "-m", "init: recent repo"])?;

    Ok((empty_dir, clone_src, recent_dir, temp_cleanup))
}

/// Commit template, missing local identity, and staged CRLF file
/// (Spec 13 #07/#13).
fn init_workflow_scenario(dir: &Path) -> Result<PathBuf> {
    fs::write(dir.join("README.md"), "# Workflow QA Repo\n")?;
    run_git(dir, &["add", "README.md"])?;
    run_git(dir, &["commit", "-m", "init: workflow sandbox"])?;

    fs::create_dir_all(dir.join("src"))?;
    fs::write(dir.join("src/app.rs"), "pub fn run() {}\n")?;
    run_git(dir, &["add", "src/app.rs"])?;
    run_git(dir, &["commit", "-m", "feat: add app module"])?;

    let template_path = dir.join(".gitmessage");
    fs::write(&template_path, "feat(): \n\n# Spec 13 commit template\n")?;
    run_git(
        dir,
        &["config", "--local", "commit.template", ".gitmessage"],
    )?;

    run_git(dir, &["config", "--local", "core.autocrlf", "false"])?;
    fs::write(
        dir.join("crlf_guarded.txt"),
        b"first line\r\nsecond line\r\n",
    )?;
    run_git(dir, &["add", "--", "crlf_guarded.txt"])?;

    run_git(dir, &["config", "--local", "user.name", ""])?;
    run_git(dir, &["config", "--local", "user.email", ""])?;
    Ok(template_path)
}

fn configure_git_user(dir: &Path, name: &str, email: &str) -> Result<()> {
    run_git(dir, &["config", "user.name", name])?;
    run_git(dir, &["config", "user.email", email])?;
    Ok(())
}

fn init_git_repo(dir: &Path, branch: &str) -> Result<()> {
    fs::create_dir_all(dir)?;
    run_git(dir, &["init", "-b", branch])?;
    configure_git_user(dir, QA_USER_NAME, QA_USER_EMAIL)
}

fn run_git_no_lfs(dir: &Path, args: &[&str]) -> Result<()> {
    let mut full: Vec<&str> = Vec::with_capacity(LFS_DISABLE_ARGS.len() + args.len());
    full.extend_from_slice(LFS_DISABLE_ARGS);
    full.extend_from_slice(args);
    run_git(dir, &full)
}

fn run_git_stdout(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", QA_USER_NAME)
        .env("GIT_AUTHOR_EMAIL", QA_USER_EMAIL)
        .env("GIT_COMMITTER_NAME", QA_USER_NAME)
        .env("GIT_COMMITTER_EMAIL", QA_USER_EMAIL)
        .output()
        .with_context(|| format!("run git {args:?}"))?;
    if !out.status.success() {
        bail!(
            "git {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );
    }
    Ok(out.stdout)
}

fn run_git_with_stdin(dir: &Path, args: &[&str], stdin_bytes: &[u8]) -> Result<Vec<u8>> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn git {args:?}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(stdin_bytes)?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "git {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );
    }
    Ok(out.stdout)
}

fn run_git(dir: &Path, args: &[&str]) -> Result<()> {
    let _ = run_git_stdout(dir, args)?;
    Ok(())
}

#[cfg(test)]
pub(crate) struct TestDir(pub(crate) PathBuf);

#[cfg(test)]
impl TestDir {
    pub(crate) fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "jbgit-pilot-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = remove_path_if_exists(&p);
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

#[cfg(test)]
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = remove_path_if_exists(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn full_scenario_creates_linear_chain_tags_and_patches() {
        let tmp = TestDir::new("full");
        let prepared = prepare_sandbox(&tmp.0, SandboxScenario::Full).unwrap();
        let dir = &prepared.repo_dir;
        let remote = tmp.0.join("sandbox-remote.git");

        let local_tags = git_out(dir, &["tag", "-l"]);
        assert!(local_tags.contains("v0.1.0"), "{local_tags}");
        assert!(local_tags.contains("v0.2.0-rc1"), "{local_tags}");
        let remote_tags = git_out(&remote, &["tag", "-l"]);
        assert!(remote_tags.contains("v0.1.0"), "{remote_tags}");

        let linear_log = git_out(dir, &["log", "--oneline", "main..feature/linear-chain"]);
        assert_eq!(linear_log.lines().count(), 5, "{linear_log}");

        let (sample_patch, conflict_patch) =
            prepared.patches.as_ref().expect("full scenario patches");
        let ok = Command::new("git")
            .args(["apply", "--check", &sample_patch.to_string_lossy()])
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            ok.status.success(),
            "{}",
            String::from_utf8_lossy(&ok.stderr)
        );

        let fail = Command::new("git")
            .args(["apply", "--check", &conflict_patch.to_string_lossy()])
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            !fail.status.success(),
            "expected conflict.patch to fail check"
        );

        let three_way_check = Command::new("git")
            .args([
                "apply",
                "--check",
                "--numstat",
                "--3way",
                &conflict_patch.to_string_lossy(),
            ])
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            three_way_check.status.success(),
            "expected conflict.patch --3way check to succeed: {}",
            String::from_utf8_lossy(&three_way_check.stderr)
        );
        let check_out = String::from_utf8_lossy(&three_way_check.stdout);
        assert!(
            check_out.contains("src/components/header.rs"),
            "{check_out}"
        );

        let three_way_apply = Command::new("git")
            .args(["apply", "--3way", &conflict_patch.to_string_lossy()])
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            !three_way_apply.status.success(),
            "expected conflict.patch --3way apply to stop with conflict"
        );
        let conflicted = fs::read_to_string(dir.join("src/components/header.rs")).unwrap();
        assert!(conflicted.contains("<<<<<<<"), "{conflicted}");
        let status = git_out(dir, &["status", "--short"]);
        assert!(status.contains("UU src/components/header.rs"), "{status}");
        assert!(status.contains("assets/logo.bin"), "{status}");
        assert!(status.contains("src/wide_lines.rs"), "{status}");

        let cherry_log = git_out(dir, &["log", "--oneline", "main..feature/cherry-source"]);
        assert_eq!(cherry_log.lines().count(), 2, "{cherry_log}");
    }

    #[test]
    fn conflict_scenario_creates_multi_file_queue_and_delete_modify() {
        let tmp = TestDir::new("conflict");
        let prepared = prepare_sandbox(&tmp.0, SandboxScenario::Conflict).unwrap();
        let dir = &prepared.repo_dir;

        assert!(dir.join(".git/MERGE_HEAD").is_file(), "expected MERGE_HEAD");
        let status = git_out(dir, &["status", "--short"]);
        assert!(status.contains("UU conflict_app.txt"), "{status}");
        assert!(status.contains("UU conflict_second.txt"), "{status}");
        assert!(status.contains("UD deleted_by_theirs.txt"), "{status}");
    }

    #[test]
    fn rebase_conflict_scenario_starts_active_rebase_with_conflict() {
        let tmp = TestDir::new("rebase-conflict");
        let prepared = prepare_sandbox(&tmp.0, SandboxScenario::RebaseConflict).unwrap();
        let dir = &prepared.repo_dir;

        assert!(
            dir.join(".git/rebase-merge").is_dir(),
            "expected .git/rebase-merge"
        );
        let status = git_out(dir, &["status", "--short"]);
        assert!(status.contains("UU rebase_target.txt"), "{status}");
    }

    #[test]
    fn bisect_scenario_starts_active_bisect_session() {
        let tmp = TestDir::new("bisect");
        let prepared = prepare_sandbox(&tmp.0, SandboxScenario::Bisect).unwrap();
        let dir = &prepared.repo_dir;

        assert!(
            dir.join(".git/BISECT_START").is_file(),
            "expected .git/BISECT_START"
        );
        let status = git_out(dir, &["status", "--short"]);
        assert!(status.trim().is_empty(), "expected clean tree: {status}");
    }

    #[test]
    fn welcome_scenario_creates_non_git_dir_clone_source_and_recent_repo() {
        let tmp = TestDir::new("welcome");
        let prepared = prepare_sandbox(&tmp.0, SandboxScenario::Welcome).unwrap();

        assert!(prepared.repo_dir.is_dir(), "expected empty dir");
        assert!(
            !prepared.repo_dir.join(".git").exists(),
            "welcome dir must not contain .git"
        );
        let clone_src = prepared.clone_source.as_deref().expect("clone_source");
        assert!(
            clone_src.join("HEAD").is_file(),
            "expected bare clone source"
        );
        let log = git_out(clone_src, &["log", "--oneline", "main"]);
        assert_eq!(log.lines().count(), 2, "{log}");

        assert_eq!(prepared.recent_repos.len(), 1);
        assert!(prepared.recent_repos[0].join(".git").exists());
    }

    #[test]
    fn workflow_scenario_configures_template_missing_identity_and_staged_crlf() {
        let tmp = TestDir::new("workflow");
        let prepared = prepare_sandbox(&tmp.0, SandboxScenario::Workflow).unwrap();
        let dir = &prepared.repo_dir;

        assert_eq!(
            prepared.commit_template.as_deref(),
            Some(dir.join(".gitmessage").as_path())
        );
        assert_eq!(
            git_out(dir, &["config", "--local", "--get", "commit.template"]).trim(),
            ".gitmessage"
        );
        assert_eq!(
            git_out(dir, &["config", "--local", "--get", "user.name"]).trim(),
            ""
        );
        assert_eq!(
            git_out(dir, &["config", "--local", "--get", "user.email"]).trim(),
            ""
        );
        assert_eq!(
            git_out(dir, &["config", "--local", "--get", "core.autocrlf"]).trim(),
            "false"
        );
        let staged_blob = run_git_stdout(dir, &["cat-file", "blob", ":crlf_guarded.txt"]).unwrap();
        assert!(
            staged_blob.windows(2).any(|w| w == b"\r\n"),
            "expected CRLF bytes in staged blob: {staged_blob:?}"
        );
    }

    #[test]
    fn capabilities_scenario_creates_shallow_dirty_submodule_lfs_and_signature() {
        let tmp = TestDir::new("caps");
        let prepared = prepare_sandbox(&tmp.0, SandboxScenario::Capabilities).unwrap();
        let dir = &prepared.repo_dir;

        assert!(dir.join(".git/shallow").is_file(), "expected shallow clone");
        assert!(dir.join("libs/submod/.git").exists(), "expected submodule");

        let status = git_out(dir, &["status", "--short"]);
        assert!(status.contains("libs/submod"), "{status}");
        assert!(status.contains("assets/model.bin"), "{status}");

        let lfs_bytes = fs::read(dir.join("assets/model.bin")).unwrap();
        let ptr = jbgit_core::parse_lfs_pointer(&lfs_bytes).expect("valid LFS pointer");
        assert_eq!(ptr.size, 2_097_152);

        let head_sig_line = git_out(dir, &["log", "-1", "--format=%G?%x1f%GS%x1f%GK"]);
        let head_sig = jbgit_core::CommitSignature::parse_single_format_line(&head_sig_line);
        assert!(
            head_sig.status.is_signed(),
            "expected HEAD to be signed, got {:?} (raw: {head_sig_line:?})",
            head_sig.status
        );
        assert_eq!(
            head_sig.status,
            jbgit_core::SignatureStatus::CannotCheck,
            "raw: {head_sig_line:?}"
        );
        assert_eq!(
            head_sig.key.as_deref(),
            Some("DEADBEEFCAFEBABE"),
            "raw: {head_sig_line:?}"
        );

        let parent_sig_line = git_out(dir, &["log", "-1", "HEAD~1", "--format=%G?%x1f%GS%x1f%GK"]);
        let parent_sig = jbgit_core::CommitSignature::parse_single_format_line(&parent_sig_line);
        assert_eq!(
            parent_sig.status,
            jbgit_core::SignatureStatus::Unsigned,
            "expected HEAD~1 to be unsigned, got {:?} (raw: {parent_sig_line:?})",
            parent_sig.status
        );
    }

    #[test]
    fn askpass_scenario_configures_empty_credential_helper_and_http_origin() {
        let tmp = TestDir::new("askpass");
        let prepared = prepare_sandbox(&tmp.0, SandboxScenario::Askpass).unwrap();
        let dir = &prepared.repo_dir;
        let url = prepared.askpass_url.as_deref().expect("askpass url");
        assert!(url.starts_with("http://127.0.0.1:"), "{url}");

        let helper = git_out(dir, &["config", "--local", "--get", "credential.helper"]);
        assert_eq!(helper.trim(), "");
        let origin = git_out(dir, &["remote", "get-url", "origin"]);
        assert_eq!(origin.trim(), url);
        let http_root = askpass_http_root(dir);
        let receive = git_out(
            &http_root.join("repo.git"),
            &["config", "--get", "http.receivepack"],
        );
        assert_eq!(receive.trim(), "true");
    }
}
