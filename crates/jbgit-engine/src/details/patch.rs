//! `git apply` argument builder and the read-only `--check --numstat`
//! preflight shared by the Apply Patch dialog and [`crate::ops`].

use std::path::{Path, PathBuf};

use jbgit_core::{
    ApplyMode, EpochToken, PatchCheck, PatchCheckKey, PatchInput, first_apply_error,
    parse_apply_check,
};

use crate::cli::non_interactive_std_command;
use crate::error::EngineError;
use crate::probe::RepoLocation;

/// `git apply` arguments. `file` = `None` reads the patch from stdin (`-`).
pub(crate) fn apply_args(mode: ApplyMode, check: bool, file: Option<&Path>) -> Vec<String> {
    let mut args = vec!["apply".to_string()];
    if check {
        args.push("--check".to_string());
        args.push("--numstat".to_string());
    }
    if let Some(flag) = mode.flag() {
        args.push(flag.to_string());
    }
    args.push("--whitespace=nowarn".to_string());
    match file {
        Some(path) => {
            args.push("--".to_string());
            args.push(path.display().to_string());
        }
        None => args.push("-".to_string()),
    }
    args
}

/// Resolves a [`PatchInput`] to `git apply` file argument or stdin bytes.
pub(crate) fn resolve_patch_input(
    loc: &RepoLocation,
    source: &PatchInput,
) -> Result<(Option<PathBuf>, Option<Vec<u8>>), String> {
    match source {
        PatchInput::File(path) => {
            let abs = if path.is_absolute() {
                path.clone()
            } else {
                loc.workdir.join(path)
            };
            if !abs.is_file() {
                return Err(format!("patch file not found: {}", abs.display()));
            }
            Ok((Some(abs), None))
        }
        PatchInput::Clipboard => {
            crate::clipboard::read_clipboard().map(|bytes| (None, Some(bytes)))
        }
    }
}

/// Runs `git apply --check --numstat [mode]`: never touches the working tree
/// or index. A failing check is reported in [`PatchCheck::error`], not as an
/// engine error.
pub(crate) fn check_patch_sync(
    loc: &RepoLocation,
    key: &PatchCheckKey,
    token: &EpochToken,
) -> Result<PatchCheck, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    let failed = |error: String| PatchCheck {
        key: key.clone(),
        files: Vec::new(),
        error: Some(error),
    };
    let (file, stdin) = match resolve_patch_input(loc, &key.source) {
        Ok(x) => x,
        Err(e) => return Ok(failed(e)),
    };
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    let git = crate::cli::cached_git_executable().unwrap_or_else(|| Path::new("git"));
    let mut cmd = non_interactive_std_command(git, Some(&loc.workdir));
    cmd.arg("--git-dir")
        .arg(&loc.git_dir)
        .arg("--work-tree")
        .arg(&loc.workdir)
        .args(["-c", "core.quotepath=false"])
        .args(apply_args(key.mode, true, file.as_deref()))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            EngineError::GitNotFound
        } else {
            EngineError::Io(e)
        }
    })?;
    if let Some(bytes) = stdin
        && let Some(mut pipe) = child.stdin.take()
    {
        use std::io::Write;
        // A rejected patch may close stdin early; the exit status reports it.
        let _ = pipe.write_all(&bytes);
    }
    let output = child.wait_with_output()?;
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    let split = |b: &[u8]| -> Vec<String> {
        String::from_utf8_lossy(b)
            .lines()
            .map(str::to_string)
            .collect()
    };
    let stdout = split(&output.stdout);
    let stderr = split(&output.stderr);
    if !output.status.success() {
        let mut all = stderr;
        all.extend(stdout);
        return Ok(failed(first_apply_error(&all)));
    }
    let files = parse_apply_check(&stdout, &stderr);
    if files.is_empty() {
        return Ok(failed("No valid patches in input".to_string()));
    }
    Ok(PatchCheck {
        key: key.clone(),
        files,
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_arguments_per_mode() {
        assert_eq!(
            apply_args(ApplyMode::WorkingTree, false, None),
            vec!["apply", "--whitespace=nowarn", "-"]
        );
        assert_eq!(
            apply_args(ApplyMode::Index, true, Some(Path::new("/p/x.patch"))),
            vec![
                "apply",
                "--check",
                "--numstat",
                "--index",
                "--whitespace=nowarn",
                "--",
                "/p/x.patch"
            ]
        );
        assert_eq!(
            apply_args(ApplyMode::ThreeWay, false, None),
            vec!["apply", "--3way", "--whitespace=nowarn", "-"]
        );
    }
}
