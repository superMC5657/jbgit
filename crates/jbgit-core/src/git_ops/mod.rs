//! Pure domain model of the high-level Git write operations driven by the
//! branches popup, context menus and dialogs: operation catalogue, CLI step
//! builders, failure classification, ref-name validation, reflog `Recent`
//! branch parsing and interactive-rebase todo rendering.

mod ops;
mod outcome;
mod rebase;
mod refs;

pub use ops::{GitOp, OngoingKind, OngoingStep, PatchSource, ResetMode, short_rev};
pub use outcome::{GitFailure, GitOpOutcome, StashEntry, classify_failure};
pub use rebase::{RewritePlan, TodoAction, TodoEntry, default_squash_messages, render_todo};
pub use refs::{
    RefNameError, fuzzy_indices, is_protected_branch, parse_recent_checkouts, validate_ref_name,
    validate_ref_name_typed,
};

#[cfg(test)]
mod tests {
    use crate::Oid;

    use super::*;

    fn oid(n: u8) -> Oid {
        Oid::from_bytes([n; 20])
    }

    #[test]
    fn recent_checkouts_from_reflog_newest_first_unique_and_existing() {
        let reflog = "\
0 1 A <a> 1 +0000\tcommit (initial): init
1 2 A <a> 2 +0000\tcheckout: moving from main to feature/a
2 3 A <a> 3 +0000\tcheckout: moving from feature/a to bugfix/b
3 4 A <a> 4 +0000\tcommit: x
4 5 A <a> 5 +0000\tcheckout: moving from bugfix/b to gone
5 6 A <a> 6 +0000\tcheckout: moving from gone to main
";
        let existing = ["main", "feature/a", "bugfix/b"];
        let recent = parse_recent_checkouts(reflog, Some("main"), &existing, 50, 5);
        assert_eq!(
            recent,
            vec!["bugfix/b".to_string(), "feature/a".to_string()]
        );
        let one = parse_recent_checkouts(reflog, Some("main"), &existing, 50, 1);
        assert_eq!(one, vec!["bugfix/b".to_string()]);
    }

    #[test]
    fn ref_name_validation_matches_check_ref_format() {
        let existing = ["main", "feature/x"];
        assert!(validate_ref_name("feature/y", &existing, "Branch").is_ok());
        for bad in [
            "", "-x", "a b", "a..b", "a~1", "a^", "a:b", "a?", "a*", "a[", "a\\b", "/a", "a/",
            "a//b", "a.", ".a", "a/.b", "a.lock", "a@{1}", "@", "HEAD", "main", "feature",
            "main/sub",
        ] {
            assert!(
                validate_ref_name(bad, &existing, "Branch").is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn classify_local_changes_conflicts_and_unmerged() {
        let lines: Vec<String> = [
            "error: Your local changes to the following files would be overwritten by checkout:",
            "\tsrc/a.rs",
            "\tsrc/b.rs",
            "Please commit your changes or stash them before you switch branches.",
            "Aborting",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            classify_failure(&lines),
            GitFailure::LocalChangesOverwritten {
                files: vec!["src/a.rs".into(), "src/b.rs".into()]
            }
        );
        assert_eq!(
            classify_failure(&["CONFLICT (content): Merge conflict in a.txt".to_string()]),
            GitFailure::Conflicts
        );
        assert_eq!(
            classify_failure(&["error: The branch 'x' is not fully merged.".to_string()]),
            GitFailure::NotFullyMerged
        );
        assert_eq!(
            classify_failure(&["fatal: bad revision".to_string()]),
            GitFailure::Other("fatal: bad revision".into())
        );
    }

    #[test]
    fn todo_is_editor_free_and_rewords_via_exec() {
        let mut todo = vec![
            TodoEntry::pick(oid(1), "one"),
            TodoEntry {
                action: TodoAction::Reword,
                oid: oid(2),
                subject: "two".into(),
                message: Some("two v2".into()),
            },
            TodoEntry {
                action: TodoAction::Drop,
                oid: oid(3),
                subject: "three".into(),
                message: None,
            },
            TodoEntry::pick(oid(4), "four"),
            TodoEntry {
                action: TodoAction::Squash,
                oid: oid(5),
                subject: "five".into(),
                message: None,
            },
        ];
        default_squash_messages(&mut todo, |o| format!("msg {}", o.short()));
        assert_eq!(
            todo[4].message.as_deref(),
            Some(format!("msg {}\n\nmsg {}", oid(4).short(), oid(5).short()).as_str())
        );
        let text = render_todo(&todo, |i| format!("'/tmp/m{i}'"));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 7);
        assert!(lines[0].starts_with("pick 0101"));
        assert!(lines[1].starts_with("pick 0202"));
        assert_eq!(
            lines[2],
            "exec git commit --amend --only --allow-empty --no-verify -F '/tmp/m1'"
        );
        assert!(lines[3].starts_with("drop 0303"));
        assert!(lines[5].starts_with("fixup 0505"));
        assert!(lines[6].ends_with("'/tmp/m4'"));
        assert!(!text.contains("reword") && !text.contains("squash"));
    }

    #[test]
    fn fuzzy_indices_highlight_path_segments() {
        let (_, idx) = fuzzy_indices("feature/dag-layout", "f/dl").unwrap();
        let chars: Vec<char> = "feature/dag-layout".chars().collect();
        let picked: String = idx.iter().map(|&i| chars[i as usize]).collect();
        assert_eq!(picked, "f/dl");
        assert!(fuzzy_indices("main", "xyz").is_none());
    }

    #[test]
    fn stash_entry_parses_branch_and_message() {
        let stash = StashEntry {
            index: 0,
            oid: oid(1),
            message: "On main: wip parser".into(),
            timestamp: 0,
        };
        assert_eq!(stash.branch(), Some("main"));
        assert_eq!(stash.short_message(), "wip parser");

        let restore = GitOp::CheckoutPaths {
            rev: oid(3).to_string(),
            paths: vec!["src/main.rs".into()],
        };
        assert!(!restore.is_read_only());
        assert!(
            GitOp::PatchToClipboard {
                source: PatchSource::CommitFile {
                    oid: oid(3),
                    path: "src/main.rs".into(),
                }
            }
            .is_read_only()
        );
    }
}
