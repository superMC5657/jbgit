use chrono::{DateTime, Local};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleLine {
    pub stream: ConsoleStream,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleLineSeverity {
    /// Normal standard output (e.g. merge diffstat, git status, file lists).
    Normal,
    /// Normal progress, remote summary, or status messages from stderr
    /// (e.g. `From https://...`, `Switched to branch ...`).
    Info,
    /// Warnings or hints (`warning:` / `hint:`).
    Warning,
    /// Explicit error lines, or stderr diagnostic messages when the command exits non-zero.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitConsoleEntry {
    pub id: u64,
    pub started_at: DateTime<Local>,
    pub duration_ms: Option<u64>,
    pub command_line: String,
    pub output_lines: Vec<ConsoleLine>,
    pub exit_code: Option<i32>,
}

impl GitConsoleEntry {
    pub fn push_line(&mut self, stream: ConsoleStream, text: String) {
        if let Some(stripped) = text.strip_prefix('\r') {
            if stripped.is_empty() {
                return;
            }
            if let Some(last) = self.output_lines.last_mut()
                && last.stream == stream
            {
                last.text = stripped.to_string();
                return;
            }
            self.output_lines.push(ConsoleLine {
                stream,
                text: stripped.to_string(),
            });
            return;
        }
        self.output_lines.push(ConsoleLine { stream, text });
    }

    pub fn has_output(&self) -> bool {
        !self.output_lines.is_empty()
    }

    pub fn is_running(&self) -> bool {
        self.exit_code.is_none()
    }

    pub fn is_success(&self) -> bool {
        self.exit_code == Some(0)
    }

    pub fn timestamp_label(&self) -> String {
        self.started_at.format("%H:%M:%S").to_string()
    }
}

const STDOUT_ERROR_PREFIXES: &[&str] = &["conflict (", "fatal:", "error:"];
const WARNING_PREFIXES: &[&str] = &["warning:", "hint:"];

// Mirrors JetBrains git4idea `GitImplBase.ERROR_INDICATORS` with tightened
// phrases to avoid false positives on unrelated progress output.
const STDERR_ERROR_INDICATORS: &[&str] = &[
    "fatal:",
    "error:",
    "remote: error",
    "cannot ",
    "could not ",
    "unable to ",
    "refusing to pull",
    "cannot rebase:",
    "conflict",
    "interactive rebase already started",
    "the file will have its original",
    "runnerw:",
];

fn starts_with_any_ignore_ascii_case(text: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| {
        text.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    })
}

/// Classifies a single Git console line into a semantic severity level based
/// on its origin stream, leading prefix keywords, and command exit code.
pub fn classify_console_line(line: &ConsoleLine, exit_code: Option<i32>) -> ConsoleLineSeverity {
    let trimmed = line.text.trim_start();
    let error_prefixes = match line.stream {
        ConsoleStream::Stdout => STDOUT_ERROR_PREFIXES,
        ConsoleStream::Stderr => STDERR_ERROR_INDICATORS,
    };
    if starts_with_any_ignore_ascii_case(trimmed, error_prefixes) {
        return ConsoleLineSeverity::Error;
    }
    if starts_with_any_ignore_ascii_case(trimmed, WARNING_PREFIXES) {
        return ConsoleLineSeverity::Warning;
    }
    match line.stream {
        ConsoleStream::Stdout => ConsoleLineSeverity::Normal,
        ConsoleStream::Stderr => match exit_code {
            Some(code) if code != 0 => ConsoleLineSeverity::Error,
            _ => ConsoleLineSeverity::Info,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(stream: ConsoleStream, text: &str) -> ConsoleLine {
        ConsoleLine {
            stream,
            text: text.to_string(),
        }
    }

    #[test]
    fn git_pull_success_stderr_is_info_while_running_and_on_zero_exit() {
        let from_line = line(ConsoleStream::Stderr, "From https://github.com/sxyazi/yazi");
        let range_line = line(
            ConsoleStream::Stderr,
            "   40a91421..d5e4beb6  main       -> origin/main",
        );

        for exit in [None, Some(0)] {
            assert_eq!(
                classify_console_line(&from_line, exit),
                ConsoleLineSeverity::Info
            );
            assert_eq!(
                classify_console_line(&range_line, exit),
                ConsoleLineSeverity::Info
            );
        }
    }

    #[test]
    fn checkout_and_push_success_stderr_are_info() {
        let checkout = line(ConsoleStream::Stderr, "Switched to branch 'main'");
        let push = line(ConsoleStream::Stderr, "To origin");

        assert_eq!(
            classify_console_line(&checkout, Some(0)),
            ConsoleLineSeverity::Info
        );
        assert_eq!(
            classify_console_line(&push, Some(0)),
            ConsoleLineSeverity::Info
        );
    }

    #[test]
    fn warning_and_hint_prefixes_are_warning_case_insensitively() {
        let cases = [
            (
                ConsoleStream::Stderr,
                "warning: pulling without specifying how to reconcile divergent branches",
            ),
            (
                ConsoleStream::Stderr,
                "  WARNING: LF will be replaced by CRLF",
            ),
            (ConsoleStream::Stderr, "hint: You can also pass --rebase"),
            (ConsoleStream::Stdout, "  Hint: see 'git help config'"),
            (ConsoleStream::Stdout, "warning: something on stdout"),
        ];

        for (stream, text) in cases {
            let l = line(stream, text);
            assert_eq!(
                classify_console_line(&l, Some(0)),
                ConsoleLineSeverity::Warning,
                "failed for {text}"
            );
            assert_eq!(
                classify_console_line(&l, Some(128)),
                ConsoleLineSeverity::Warning,
                "failed for {text} with non-zero exit"
            );
        }
    }

    #[test]
    fn explicit_error_indicators_on_stderr_and_stdout_are_error() {
        let error_cases = [
            (
                ConsoleStream::Stderr,
                "fatal: Not possible to fast-forward, aborting.",
            ),
            (ConsoleStream::Stderr, "  FATAL: repository not found"),
            (ConsoleStream::Stderr, "error: failed to push some refs"),
            (
                ConsoleStream::Stderr,
                "remote: error: GH006: Protected branch update failed.",
            ),
            (ConsoleStream::Stderr, "cannot lock ref 'refs/heads/main'"),
            (
                ConsoleStream::Stderr,
                "Could not read from remote repository.",
            ),
            (
                ConsoleStream::Stderr,
                "unable to access 'https://example.com/': Could not resolve host",
            ),
            (
                ConsoleStream::Stderr,
                "refusing to pull unrelated histories",
            ),
            (
                ConsoleStream::Stderr,
                "cannot rebase: You have unstaged changes.",
            ),
            (ConsoleStream::Stderr, "conflict in file.rs"),
            (ConsoleStream::Stderr, "Interactive rebase already started"),
            (
                ConsoleStream::Stderr,
                "The file will have its original line endings in your working directory",
            ),
            (ConsoleStream::Stderr, "runnerw: CreateProcess failed"),
            (
                ConsoleStream::Stdout,
                "CONFLICT (content): Merge conflict in src/main.rs",
            ),
            (ConsoleStream::Stdout, "fatal: bad object HEAD"),
            (ConsoleStream::Stdout, "error: pathspec 'foo' did not match"),
        ];

        for (stream, text) in error_cases {
            let l = line(stream, text);
            assert_eq!(
                classify_console_line(&l, Some(0)),
                ConsoleLineSeverity::Error,
                "expected Error for {text}"
            );
            assert_eq!(
                classify_console_line(&l, None),
                ConsoleLineSeverity::Error,
                "expected Error while running for {text}"
            );
        }
    }

    #[test]
    fn nonzero_exit_falls_back_to_error_for_stderr_but_keeps_stdout_normal() {
        let stderr_unknown = line(ConsoleStream::Stderr, "Permission denied (publickey).");
        let stdout_normal = line(ConsoleStream::Stdout, "Updating 40a91421..d5e4beb6");

        assert_eq!(
            classify_console_line(&stderr_unknown, Some(128)),
            ConsoleLineSeverity::Error
        );
        assert_eq!(
            classify_console_line(&stdout_normal, Some(128)),
            ConsoleLineSeverity::Normal
        );
    }

    #[test]
    fn push_line_preserves_interleaved_arrival_order() {
        let mut entry = GitConsoleEntry {
            id: 1,
            started_at: Local::now(),
            duration_ms: None,
            command_line: "git pull".into(),
            output_lines: Vec::new(),
            exit_code: None,
        };
        assert!(!entry.has_output());

        entry.push_line(
            ConsoleStream::Stderr,
            "From https://example.com/repo".into(),
        );
        entry.push_line(
            ConsoleStream::Stderr,
            "   40a91421..d5e4beb6  main -> origin/main".into(),
        );
        entry.push_line(ConsoleStream::Stdout, "Updating 40a91421..d5e4beb6".into());
        entry.push_line(
            ConsoleStream::Stdout,
            " create mode 100644 src/new_file.rs".into(),
        );
        entry.push_line(ConsoleStream::Stderr, "warning: minor notice".into());

        assert!(entry.has_output());
        assert_eq!(
            entry.output_lines,
            vec![
                line(ConsoleStream::Stderr, "From https://example.com/repo"),
                line(
                    ConsoleStream::Stderr,
                    "   40a91421..d5e4beb6  main -> origin/main"
                ),
                line(ConsoleStream::Stdout, "Updating 40a91421..d5e4beb6"),
                line(ConsoleStream::Stdout, " create mode 100644 src/new_file.rs"),
                line(ConsoleStream::Stderr, "warning: minor notice"),
            ]
        );
    }

    #[test]
    fn push_line_coalesces_carriage_return_progress_updates() {
        let mut entry = GitConsoleEntry {
            id: 2,
            started_at: Local::now(),
            duration_ms: None,
            command_line: "git fetch --unshallow --progress origin".into(),
            output_lines: Vec::new(),
            exit_code: None,
        };

        entry.push_line(
            ConsoleStream::Stderr,
            "remote: Enumerating objects: 10, done.".into(),
        );
        entry.push_line(
            ConsoleStream::Stderr,
            "Receiving objects:  10% (1/10)".into(),
        );
        entry.push_line(
            ConsoleStream::Stderr,
            "\rReceiving objects:  50% (5/10)".into(),
        );
        entry.push_line(
            ConsoleStream::Stderr,
            "\rReceiving objects: 100% (10/10), done.".into(),
        );
        entry.push_line(ConsoleStream::Stderr, "\r".into());

        assert_eq!(
            entry.output_lines,
            vec![
                line(
                    ConsoleStream::Stderr,
                    "remote: Enumerating objects: 10, done."
                ),
                line(
                    ConsoleStream::Stderr,
                    "Receiving objects: 100% (10/10), done."
                ),
            ]
        );
    }
}
