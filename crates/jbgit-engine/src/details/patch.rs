//! `git apply` argument builder and the read-only `--check --numstat`
//! preflight shared by the Apply Patch dialog and [`crate::ops`].

use std::path::{Path, PathBuf};

use jbgit_core::{
    ApplyOptions, EpochToken, PatchCheck, PatchCheckKey, PatchInput, first_apply_error,
    parse_apply_check,
};

use crate::cli::non_interactive_std_command;
use crate::error::EngineError;
use crate::probe::RepoLocation;

/// Maximum patch size parsed for in-modal per-file diff preview (8 MiB).
const MAX_PATCH_PREVIEW_BYTES: usize = 8 * 1024 * 1024;

/// Escapes Git wildmatch glob metacharacters (`\`, `*`, `?`, `[`, `]`) so
/// `git apply --include=<path>` matches literal paths such as `app/[id]/page.tsx`.
fn escape_wildmatch_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        if matches!(ch, '\\' | '*' | '?' | '[' | ']') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// `git apply` arguments. `file` = `None` reads the patch from stdin (`-`).
pub(crate) fn apply_args(options: &ApplyOptions, check: bool, file: Option<&Path>) -> Vec<String> {
    let mut args = vec!["apply".to_string()];
    if check {
        args.push("--check".to_string());
        args.push("--numstat".to_string());
    }
    if let Some(flag) = options.mode.flag() {
        args.push(flag.to_string());
    }
    if options.reverse {
        args.push("-R".to_string());
    }
    if let Some(strip) = options.strip {
        args.push(format!("-p{strip}"));
    }
    if let Some(dir) = options.normalized_directory() {
        args.push(format!("--directory={dir}"));
    }
    for path in &options.include_paths {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            let escaped = escape_wildmatch_path(trimmed);
            args.push(format!("--include={escaped}"));
        }
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

fn read_patch_preview_text(file: Option<&Path>, stdin: Option<&[u8]>) -> String {
    if let Some(bytes) = stdin {
        let slice = &bytes[..bytes.len().min(MAX_PATCH_PREVIEW_BYTES)];
        return String::from_utf8_lossy(slice).into_owned();
    }
    let Some(path) = file else {
        return String::new();
    };
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    use std::io::Read;
    let mut buf = Vec::new();
    let _ = f
        .by_ref()
        .take(MAX_PATCH_PREVIEW_BYTES as u64)
        .read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Runs `git apply --check --numstat [options]`: never touches the working tree
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
    let (file, stdin) = match resolve_patch_input(loc, &key.source) {
        Ok(x) => x,
        Err(e) => {
            return Ok(PatchCheck {
                key: key.clone(),
                files: Vec::new(),
                sections: Vec::new(),
                has_index_headers: false,
                error: Some(e),
            });
        }
    };
    let preview_text = read_patch_preview_text(file.as_deref(), stdin.as_deref());
    let (sections, has_index_headers) = jbgit_core::parse_patch_sections_with_options(
        &preview_text,
        key.options.effective_strip(),
        key.options.normalized_directory().as_deref(),
        key.options.reverse,
    );
    let failed = |error: String| PatchCheck {
        key: key.clone(),
        files: Vec::new(),
        sections: sections.clone(),
        has_index_headers,
        error: Some(error),
    };
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    let git = crate::cli::require_git_executable()?;
    let mut cmd = non_interactive_std_command(&git, Some(&loc.workdir));
    cmd.arg("--git-dir")
        .arg(&loc.git_dir)
        .arg("--work-tree")
        .arg(&loc.workdir)
        .args(["-c", "core.quotepath=false"])
        .args(apply_args(&key.options, true, file.as_deref()))
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
    let stdin_pipe = child.stdin.take();
    let output = std::thread::scope(|s| -> std::io::Result<std::process::Output> {
        let writer = stdin.zip(stdin_pipe).map(|(bytes, mut pipe)| {
            s.spawn(move || {
                use std::io::Write;
                // A rejected patch may close stdin early; the exit status reports it.
                let _ = pipe.write_all(&bytes);
            })
        });
        let output = child.wait_with_output()?;
        if let Some(h) = writer {
            let _ = h.join();
        }
        Ok(output)
    })?;
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
        sections,
        has_index_headers,
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use jbgit_core::ApplyMode;

    use super::*;

    #[test]
    fn apply_arguments_per_mode_and_options() {
        assert_eq!(
            apply_args(&ApplyOptions::with_mode(ApplyMode::WorkingTree), false, None),
            vec!["apply", "--whitespace=nowarn", "-"]
        );
        assert_eq!(
            apply_args(
                &ApplyOptions::with_mode(ApplyMode::Index),
                true,
                Some(Path::new("/p/x.patch"))
            ),
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
            apply_args(&ApplyOptions::with_mode(ApplyMode::ThreeWay), false, None),
            vec!["apply", "--3way", "--whitespace=nowarn", "-"]
        );
        let full = ApplyOptions {
            mode: ApplyMode::WorkingTree,
            reverse: true,
            strip: Some(2),
            directory: Some("sub/dir/".into()),
            include_paths: vec!["sub/dir/a.txt".into(), "sub/dir/b.txt".into()],
        };
        assert_eq!(
            apply_args(&full, true, Some(Path::new("x.patch"))),
            vec![
                "apply",
                "--check",
                "--numstat",
                "-R",
                "-p2",
                "--directory=sub/dir",
                "--include=sub/dir/a.txt",
                "--include=sub/dir/b.txt",
                "--whitespace=nowarn",
                "--",
                "x.patch",
            ]
        );
    }
}
