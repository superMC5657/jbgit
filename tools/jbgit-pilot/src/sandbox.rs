use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxScenario {
    Basic,
    Full,
    Conflict,
}

impl SandboxScenario {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "basic" => Ok(Self::Basic),
            "full" => Ok(Self::Full),
            "conflict" => Ok(Self::Conflict),
            other => bail!("unknown scenario '{other}' (expected: full, conflict, basic)"),
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

    fs::write(
        dir.join("keymap.toml"),
        r#"
footer_max_lines = 2

[global]
prepend_keymap = [
    { on = [ "r" ], run = "noop" },
    { on = [ "b" ], run = "noop" },
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

pub fn init_sandbox_repo(dir: &Path, remote_dir: &Path, scenario: SandboxScenario) -> Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    if remote_dir.exists() {
        fs::remove_dir_all(remote_dir)?;
    }
    fs::create_dir_all(dir)?;

    run_git(dir, &["init", "-b", "main"])?;
    run_git(dir, &["config", "user.name", "QA Tester"])?;
    run_git(dir, &["config", "user.email", "qa@example.com"])?;

    match scenario {
        SandboxScenario::Basic => init_basic_scenario(dir)?,
        SandboxScenario::Conflict => init_conflict_scenario(dir)?,
        SandboxScenario::Full => init_full_scenario(dir, remote_dir)?,
    }
    Ok(())
}

fn init_basic_scenario(dir: &Path) -> Result<()> {
    fs::write(dir.join("README.md"), "# jbgit QA Sandbox\n\nline 1\nline 2\n")?;
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
    fs::write(
        dir.join("conflict_app.txt"),
        "line 1: base\nline 2: common\n",
    )?;
    fs::write(dir.join("README.md"), "# Conflict QA Repo\n")?;
    run_git(dir, &["add", "."])?;
    run_git(dir, &["commit", "-m", "base commit"])?;

    run_git(dir, &["checkout", "-b", "feature/theirs"])?;
    fs::write(
        dir.join("conflict_app.txt"),
        "line 1: theirs change\nline 2: common\n",
    )?;
    run_git(dir, &["commit", "-am", "theirs change"])?;

    run_git(dir, &["checkout", "main"])?;
    fs::write(
        dir.join("conflict_app.txt"),
        "line 1: ours change\nline 2: common\n",
    )?;
    run_git(dir, &["commit", "-am", "ours change"])?;

    let out = Command::new("git")
        .args(["merge", "feature/theirs"])
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "QA Tester")
        .env("GIT_AUTHOR_EMAIL", "qa@example.com")
        .env("GIT_COMMITTER_NAME", "QA Tester")
        .env("GIT_COMMITTER_EMAIL", "qa@example.com")
        .output()?;
    if out.status.success() {
        bail!("expected merge conflict in Conflict scenario, but merge succeeded");
    }
    Ok(())
}

fn init_full_scenario(dir: &Path, remote_dir: &Path) -> Result<()> {
    fs::create_dir_all(remote_dir)?;
    run_git(remote_dir, &["init", "--bare", "-b", "main"])?;

    fs::write(dir.join("README.md"), "# jbgit Full QA Sandbox\n\nline 1\n")?;
    fs::create_dir_all(dir.join("src/components"))?;
    let mut src_lines = Vec::new();
    for i in 1..=30 {
        src_lines.push(format!("fn step_{i:02}() {{ println!(\"step {i}\"); }}"));
    }
    fs::write(dir.join("src/main.rs"), format!("{}\n", src_lines.join("\n")))?;
    fs::write(
        dir.join("src/legacy_auth.rs"),
        "pub fn verify_user(user: &str) -> bool {\n    !user.is_empty()\n}\n",
    )?;
    fs::write(
        dir.join("src/components/header.rs"),
        "pub fn title() -> &'static str { \"jbgit\" }\n",
    )?;
    fs::write(dir.join(".gitignore"), "*.log\n")?;
    fs::write(dir.join("staged_partial.txt"), "alpha\nbeta\ngamma\ndelta\n")?;
    run_git(dir, &["add", "."])?;
    run_git(dir, &["commit", "-m", "initial project scaffold"])?;
    run_git(dir, &["tag", "v0.1.0"])?;

    fs::write(
        dir.join("src/legacy_auth.rs"),
        "pub fn verify_user(user: &str) -> bool {\n    !user.is_empty()\n}\n\npub fn token_ttl() -> u64 {\n    3600\n}\n",
    )?;
    run_git(dir, &["commit", "-am", "feat: add token_ttl to legacy_auth"])?;
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
    run_git(&peer_dir, &["config", "user.name", "Remote Peer"])?;
    run_git(&peer_dir, &["config", "user.email", "peer@example.com"])?;
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
        &["commit", "-am", "feat: refine auth, header, and README on main"],
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
    fs::write(dir.join("src/main.rs"), format!("{}\n", src_lines.join("\n")))?;

    fs::write(dir.join("scratch_note.txt"), "untracked qa file\n")?;
    fs::write(dir.join("debug_trace.log"), "ignored debug log\n")?;
    Ok(())
}

fn run_git(dir: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "QA Tester")
        .env("GIT_AUTHOR_EMAIL", "qa@example.com")
        .env("GIT_COMMITTER_NAME", "QA Tester")
        .env("GIT_COMMITTER_EMAIL", "qa@example.com")
        .output()
        .with_context(|| format!("run git {args:?}"))?;
    if !out.status.success() {
        bail!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}
