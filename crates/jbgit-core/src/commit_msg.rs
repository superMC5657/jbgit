//! Pure commit-message helpers: comment stripping (`--cleanup=strip`),
//! `core.commentChar` parsing, 72-column body reformatting, and push refspec
//! destination matching.

use unicode_width::UnicodeWidthStr;

/// Default body wrap column for commit messages (50/72 convention).
pub const COMMIT_BODY_MAX_WIDTH: usize = 72;

/// Parses `git config core.commentChar` into the effective comment prefix char.
///
/// Git accepts a single character or `"auto"` (which picks `#` unless the
/// message itself conflicts; we default to `#` here).
pub fn parse_comment_char(raw: Option<&str>) -> char {
    let Some(s) = raw.map(str::trim) else {
        return '#';
    };
    if s.is_empty() || s.eq_ignore_ascii_case("auto") {
        return '#';
    }
    s.chars().next().unwrap_or('#')
}

/// Strips comment lines and normalizes whitespace identically to
/// `git commit --cleanup=strip`.
///
/// - Normalizes `\r\n` to `\n`.
/// - Removes lines beginning with `comment_char`.
/// - Trims trailing spaces/tabs on each remaining line.
/// - Collapses consecutive blank lines into one blank line.
/// - Removes leading and trailing blank lines.
pub fn strip_commit_comments(message: &str, comment_char: char) -> String {
    let normalized = message.replace("\r\n", "\n").replace('\r', "\n");
    let mut out: Vec<&str> = Vec::new();
    let mut prev_blank = true;

    for raw_line in normalized.split('\n') {
        if raw_line.starts_with(comment_char) {
            continue;
        }
        let trimmed = raw_line.trim_end_matches([' ', '\t']);
        if trimmed.is_empty() {
            if !prev_blank {
                out.push("");
                prev_blank = true;
            }
        } else {
            out.push(trimmed);
            prev_blank = false;
        }
    }

    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }

    out.join("\n")
}

/// Reformats a commit message's body lines to wrap at `max_width` display
/// columns while preserving:
/// - Line 0 (the subject line) verbatim
/// - Blank paragraph separators
/// - Comment lines starting with `comment_char`
/// - Fenced code blocks (` ``` ` / `~~~`)
/// - Indented code blocks (≥ 4 spaces or tab) and block quotes (`>`)
/// - Git trailers (`Signed-off-by: ...`, `Co-authored-by: ...`, etc.)
/// - Bullet and numbered list items (with hanging indentation)
/// - Single long words/URLs without breaking mid-token
pub fn reformat_commit_message(message: &str, max_width: usize, comment_char: char) -> String {
    let normalized = message.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    if lines.len() <= 1 || max_width == 0 {
        return normalized;
    }

    let mut result: Vec<String> = Vec::with_capacity(lines.len());
    // Subject line (line 0) is always preserved verbatim.
    result.push(lines[0].to_string());

    let body = &lines[1..];
    let mut i = 0usize;
    let mut in_fence = false;
    let mut fence_marker = "";

    while i < body.len() {
        let line = body[i];
        let trimmed = line.trim();

        // Fenced code blocks: pass through verbatim.
        if in_fence {
            result.push(line.to_string());
            if trimmed.starts_with(fence_marker) {
                in_fence = false;
                fence_marker = "";
            }
            i += 1;
            continue;
        }
        if trimmed.starts_with("```") {
            in_fence = true;
            fence_marker = "```";
            result.push(line.to_string());
            i += 1;
            continue;
        }
        if trimmed.starts_with("~~~") {
            in_fence = true;
            fence_marker = "~~~";
            result.push(line.to_string());
            i += 1;
            continue;
        }

        // Blank line, comment line, indented code, blockquote, or git trailer: preserve verbatim.
        if trimmed.is_empty()
            || line.starts_with(comment_char)
            || line.starts_with("    ")
            || line.starts_with('\t')
            || trimmed.starts_with('>')
            || is_git_trailer(line)
        {
            result.push(line.to_string());
            i += 1;
            continue;
        }

        // Bullet or numbered list item: wrap this item (including any continuation lines
        // indented to at least the item content column).
        if let Some((prefix, rest)) = split_list_item(line) {
            let cont_indent = " ".repeat(prefix.width());
            let mut joined = rest.trim().to_string();
            i += 1;
            while i < body.len() {
                let next = body[i];
                let next_trimmed = next.trim();
                if next_trimmed.is_empty()
                    || next.starts_with(comment_char)
                    || next_trimmed.starts_with("```")
                    || next_trimmed.starts_with("~~~")
                    || next_trimmed.starts_with('>')
                    || is_git_trailer(next)
                    || split_list_item(next).is_some()
                {
                    break;
                }
                if !joined.is_empty() {
                    joined.push(' ');
                }
                joined.push_str(next_trimmed);
                i += 1;
            }
            result.extend(wrap_with_prefix(prefix, &cont_indent, &joined, max_width));
            continue;
        }

        // Regular paragraph: collect consecutive plain lines and wrap together.
        let leading_ws_len = line.len() - line.trim_start_matches(' ').len();
        let indent = &line[..leading_ws_len];
        let mut para_parts = vec![trimmed];
        i += 1;
        while i < body.len() {
            let next = body[i];
            let next_trimmed = next.trim();
            if next_trimmed.is_empty()
                || next.starts_with(comment_char)
                || next.starts_with("    ")
                || next.starts_with('\t')
                || next_trimmed.starts_with("```")
                || next_trimmed.starts_with("~~~")
                || next_trimmed.starts_with('>')
                || is_git_trailer(next)
                || split_list_item(next).is_some()
            {
                break;
            }
            para_parts.push(next_trimmed);
            i += 1;
        }
        let joined = para_parts.join(" ");
        result.extend(wrap_with_prefix(indent, indent, &joined, max_width));
    }

    result.join("\n")
}

/// Returns `true` if `line` looks like a Git trailer (`Token-Key: value`).
fn is_git_trailer(line: &str) -> bool {
    let Some((key, rest)) = line.split_once(": ") else {
        return false;
    };
    if rest.trim().is_empty() || key.is_empty() {
        return false;
    }
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    // Trailers use hyphenated/alphanumeric single-token keys (no spaces), e.g.,
    // Signed-off-by, Co-authored-by, Change-Id, Fixes, Closes, Refs, Reviewed-by.
    chars.all(|c| c.is_ascii_alphanumeric() || c == '-')
        && (key.contains('-')
            || matches!(
                key,
                "Fixes" | "Closes" | "Refs" | "See" | "Bug" | "Issue" | "Ticket" | "Cc"
            ))
}

/// Splits a bullet (`- `, `* `, `+ `) or numbered (`1. `, `1) `) list item into
/// `(prefix_including_leading_spaces, rest)`.
fn split_list_item(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() && bytes[idx] == b' ' {
        idx += 1;
    }
    if idx >= 4 {
        return None;
    }
    let after_ws = &line[idx..];
    if let Some(first) = after_ws.as_bytes().first().copied()
        && matches!(first, b'-' | b'*' | b'+')
        && after_ws.as_bytes().get(1) == Some(&b' ')
    {
        let split = idx + 2;
        return Some((&line[..split], &line[split..]));
    }
    let digits = after_ws
        .bytes()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if digits > 0
        && let Some(punct) = after_ws.as_bytes().get(digits).copied()
        && matches!(punct, b'.' | b')')
        && after_ws.as_bytes().get(digits + 1) == Some(&b' ')
    {
        let split = idx + digits + 2;
        return Some((&line[..split], &line[split..]));
    }
    None
}

fn wrap_with_prefix(
    first_prefix: &str,
    cont_prefix: &str,
    text: &str,
    max_width: usize,
) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return vec![first_prefix.trim_end().to_string()];
    }

    let mut lines = Vec::new();
    let mut current = String::from(first_prefix);
    let mut cur_w = first_prefix.width();
    let mut has_word = false;

    for word in words {
        let word_w = word.width();
        if !has_word {
            current.push_str(word);
            cur_w += word_w;
            has_word = true;
        } else if cur_w + 1 + word_w <= max_width {
            current.push(' ');
            current.push_str(word);
            cur_w += 1 + word_w;
        } else {
            lines.push(current);
            current = String::from(cont_prefix);
            current.push_str(word);
            cur_w = cont_prefix.width() + word_w;
        }
    }
    lines.push(current);
    lines
}

/// Resolves the default remote push target branch for `branch` from a remote's
/// configured `remote.<name>.push` refspecs.
///
/// Returns the target branch name (with leading `refs/heads/` stripped, or full
/// `refs/...` path for non-branch namespaces like `refs/for/main`), or `None` if
/// no refspec matches `branch`.
pub fn resolve_push_refspec_target(branch: &str, refspecs: &[String]) -> Option<String> {
    let branch = branch.trim();
    if branch.is_empty() {
        return None;
    }
    let full_src = if branch.starts_with("refs/") {
        branch.to_string()
    } else {
        format!("refs/heads/{branch}")
    };

    // Negative refspecs (`^<pattern>`) exclude matching branches regardless of order.
    for raw in refspecs {
        let spec = raw.trim();
        if let Some(neg) = spec.strip_prefix('^')
            && !neg.is_empty()
            && match_refspec_pattern(neg, &full_src, branch).is_some()
        {
            return None;
        }
    }

    for raw in refspecs {
        let spec = raw.trim().trim_start_matches('+');
        if spec.is_empty() || spec.starts_with('^') {
            continue;
        }
        let Some((src, dst)) = spec.split_once(':') else {
            continue;
        };
        if src.is_empty() || dst.is_empty() {
            continue;
        }
        if let Some(matched) = match_refspec_pattern(src, &full_src, branch) {
            let expanded = if let Some((dst_pre, dst_post)) = dst.split_once('*') {
                format!("{dst_pre}{matched}{dst_post}")
            } else {
                dst.to_string()
            };
            let target = expanded
                .strip_prefix("refs/heads/")
                .unwrap_or(&expanded)
                .to_string();
            if !target.is_empty() {
                return Some(target);
            }
        }
    }
    None
}

fn match_refspec_pattern<'a>(pattern: &str, full_src: &'a str, short_src: &'a str) -> Option<&'a str> {
    if let Some((pre, post)) = pattern.split_once('*') {
        if let Some(rest) = full_src.strip_prefix(pre)
            && let Some(mid) = rest.strip_suffix(post)
        {
            return Some(mid);
        }
        if !pattern.starts_with("refs/")
            && let Some(rest) = short_src.strip_prefix(pre)
            && let Some(mid) = rest.strip_suffix(post)
        {
            return Some(mid);
        }
        None
    } else if pattern == full_src || pattern == short_src || pattern == "HEAD" {
        Some("")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_comments_handles_default_and_custom_comment_char() {
        let msg = "# leading comment\n\nfeat: subject   \n\n# comment in middle\nbody line 1\n\n\nbody line 2\n# trailing comment\n";
        assert_eq!(
            strip_commit_comments(msg, '#'),
            "feat: subject\n\nbody line 1\n\nbody line 2"
        );

        let custom = "; template comment\n# markdown heading kept\n\n; another\ncontent\n";
        assert_eq!(
            strip_commit_comments(custom, ';'),
            "# markdown heading kept\n\ncontent"
        );

        assert_eq!(strip_commit_comments("# only comments\n# here\n", '#'), "");
        assert_eq!(parse_comment_char(None), '#');
        assert_eq!(parse_comment_char(Some("auto")), '#');
        assert_eq!(parse_comment_char(Some(";")), ';');
    }

    #[test]
    fn reformat_wraps_body_at_72_and_preserves_subject_lists_code_and_trailers() {
        let input = "\
feat: a subject line that intentionally stays intact even if it is longer than seventy two characters

This is a very long commit message body paragraph that definitely exceeds seventy-two columns and should be cleanly wrapped onto multiple lines without splitting words.

- This is a bullet list item that also exceeds seventy-two columns and needs to wrap cleanly with hanging indentation preserved.
  continuation line of the same bullet.

```rust
let untouched_code_line_that_is_intentionally_much_longer_than_seventy_two_columns = 42;
```

    indented_code_line_that_is_also_longer_than_seventy_two_columns_should_stay_intact();

Signed-off-by: Very Long Developer Name With Long Domain <developer.name@very-long-organization-domain.example.com>
# Comment line that is longer than seventy-two columns should also stay untouched";

        let out = reformat_commit_message(input, 72, '#');
        let lines: Vec<&str> = out.lines().collect();

        // Subject untouched
        assert_eq!(
            lines[0],
            "feat: a subject line that intentionally stays intact even if it is longer than seventy two characters"
        );
        // Wrapped paragraph lines <= 72
        assert!(lines[2].len() <= 72, "line[2]: {}", lines[2]);
        assert!(lines[3].len() <= 72, "line[3]: {}", lines[3]);
        // Bullet item wrapped with 2-space hanging indent
        let bullet_idx = lines.iter().position(|l| l.starts_with("- This is a bullet")).unwrap();
        assert!(lines[bullet_idx].len() <= 72);
        assert!(lines[bullet_idx + 1].starts_with("  "), "{}", lines[bullet_idx + 1]);
        assert!(lines[bullet_idx + 1].len() <= 72);
        // Code block, indented code, trailer, and comment preserved verbatim
        assert!(out.contains("let untouched_code_line_that_is_intentionally_much_longer_than_seventy_two_columns = 42;"));
        assert!(out.contains("    indented_code_line_that_is_also_longer_than_seventy_two_columns_should_stay_intact();"));
        assert!(out.contains("Signed-off-by: Very Long Developer Name With Long Domain <developer.name@very-long-organization-domain.example.com>"));
        assert!(out.contains("# Comment line that is longer than seventy-two columns should also stay untouched"));

        // Prose line with a hyphen before `: ` must NOT be misclassified as a Git trailer; it must wrap.
        let prose_with_hyphen = "feat: subject\n\nNote on non-empty input: this prose body line contains a hyphen before the colon and exceeds seventy-two columns so it must wrap cleanly.";
        let wrapped_prose = reformat_commit_message(prose_with_hyphen, 72, '#');
        let prose_lines: Vec<&str> = wrapped_prose.lines().collect();
        assert!(prose_lines.len() >= 4, "expected wrapped prose: {wrapped_prose}");
        for body_line in &prose_lines[2..] {
            assert!(body_line.len() <= 72, "body line exceeded 72 cols: {body_line}");
        }
    }

    #[test]
    fn push_refspec_target_resolves_exact_wildcard_and_gerrit() {
        let specs = vec![
            "+refs/heads/main:refs/heads/trunk".to_string(),
            "refs/heads/feat/*:refs/heads/team/*".to_string(),
            "refs/heads/review:refs/for/main".to_string(),
        ];
        assert_eq!(
            resolve_push_refspec_target("main", &specs).as_deref(),
            Some("trunk")
        );
        assert_eq!(
            resolve_push_refspec_target("feat/login", &specs).as_deref(),
            Some("team/login")
        );
        assert_eq!(
            resolve_push_refspec_target("review", &specs).as_deref(),
            Some("refs/for/main")
        );
        assert_eq!(resolve_push_refspec_target("other", &specs), None);
    }

    #[test]
    fn push_refspec_target_honors_negative_refspecs() {
        let specs = vec![
            "refs/heads/*:refs/heads/*".to_string(),
            "^refs/heads/wip/*".to_string(),
            "^refs/heads/secret".to_string(),
        ];
        assert_eq!(resolve_push_refspec_target("wip/test", &specs), None);
        assert_eq!(resolve_push_refspec_target("secret", &specs), None);
        assert_eq!(
            resolve_push_refspec_target("feat/ok", &specs).as_deref(),
            Some("feat/ok")
        );
    }
}
