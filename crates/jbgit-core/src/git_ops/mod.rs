//! Pure domain model of the high-level Git write operations driven by the
//! branches popup, context menus and dialogs: operation catalogue, CLI step
//! builders, failure classification, ref-name validation, reflog `Recent`
//! branch parsing and interactive-rebase todo rendering.

mod ops;
mod outcome;
mod rebase;
mod refs;

pub use ops::{
    FastForward, GitOp, MergeOptionConflict, MergeOptions, OngoingKind, OngoingStep, PatchSource,
    PushForce, PushTagMode, ResetMode, UpdateMode, short_rev,
};
pub use outcome::{
    GitFailure, GitOpOutcome, PushOutcomeSummary, SkippedBranch, StashEntry, UpdateInfo,
    UpdatedRange, classify_failure, parse_push_porcelain,
};
pub use rebase::{RewritePlan, TodoAction, TodoEntry, default_squash_messages, render_todo};
pub use refs::{
    RefNameError, fuzzy_indices, is_protected_branch, matches_protected_branch_patterns,
    parse_recent_checkouts, validate_ref_name, validate_ref_name_typed,
};

#[cfg(test)]
mod tests {
    use crate::{Oid, RefreshScope};

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
    fn classify_authentication_and_remote_exists() {
        let line = |s: &str| vec![s.to_string()];
        for msg in [
            "remote: HTTP Basic: Access denied",
            "fatal: Authentication failed for 'https://h/x.git/'",
            "git@h: Permission denied (publickey).",
            "fatal: could not read Username for 'https://h': terminal prompts disabled",
            "fatal: could not read Password for 'https://u@h': No such device or address",
        ] {
            assert_eq!(classify_failure(&line(msg)), GitFailure::AuthenticationFailed, "{msg}");
        }
        assert_eq!(
            classify_failure(&line("error: remote fork already exists.")),
            GitFailure::RemoteAlreadyExists
        );
        assert_eq!(
            classify_failure(&line("fatal: tag 'v1' already exists")),
            GitFailure::TagAlreadyExists
        );
    }

    #[test]
    fn remote_ops_are_classified_for_network_and_refresh() {
        let fetch_one = GitOp::FetchRemote { name: "fork".into() };
        assert!(fetch_one.is_network());
        assert!(GitOp::Smart(Box::new(GitOp::Pull { rebase: false })).is_network());
        assert!(GitOp::Sequence(vec![GitOp::Fetch, GitOp::Checkout {
            target: "x".into(),
            detach: false
        }])
        .is_network());
        for local in [
            GitOp::AddRemote { name: "f".into(), url: "u".into() },
            GitOp::RenameRemote { old: "f".into(), new: "g".into() },
            GitOp::RemoveRemote { name: "f".into() },
            GitOp::SetRemoteUrl { name: "f".into(), url: "u".into(), push: true },
        ] {
            assert!(!local.is_network(), "{}", local.label());
            assert_eq!(local.refresh_scope(), RefreshScope::RefsAndGraph);
        }
        assert_eq!(fetch_one.refresh_scope(), RefreshScope::RefsAndGraph);
        assert_eq!(fetch_one.label(), "Fetch 'fork'");
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

    #[test]
    fn tag_and_push_ops_labels_scopes_failures_and_porcelain() {
        let del = GitOp::DeleteTag {
            name: "v1.0".into(),
            tip: oid(4),
        };
        assert_eq!(del.label(), "Delete tag 'v1.0'");
        assert_eq!(del.refresh_scope(), RefreshScope::RefsAndGraph);

        let restore = GitOp::RestoreTag {
            name: "v1.0".into(),
            oid: oid(4),
        };
        assert_eq!(restore.label(), "Restore tag 'v1.0'");
        assert_eq!(restore.refresh_scope(), RefreshScope::RefsAndGraph);

        let del_remote = GitOp::DeleteRemoteTag {
            remote: "origin".into(),
            name: "v1.0".into(),
        };
        assert_eq!(del_remote.label(), "Delete remote tag 'origin/v1.0'");
        assert_eq!(del_remote.refresh_scope(), RefreshScope::RefsAndGraph);

        let push_tag = GitOp::PushTag {
            remote: "origin".into(),
            name: "v1.0".into(),
            force: false,
        };
        assert_eq!(push_tag.label(), "Push tag 'v1.0' to 'origin'");
        assert_eq!(push_tag.refresh_scope(), RefreshScope::RefsAndGraph);

        let push = GitOp::Push {
            branch: "feat".into(),
            remote: "upstream".into(),
            remote_branch: "feat-v2".into(),
            set_upstream: true,
            force: PushForce::WithLease,
            tags: PushTagMode::Follow,
            up_to: None,
        };
        assert_eq!(push.label(), "Push 'feat' → 'upstream/feat-v2'");
        assert_eq!(push.refresh_scope(), RefreshScope::RefsAndGraph);

        let seq = GitOp::Sequence(vec![
            GitOp::Fetch,
            GitOp::Rebase {
                onto: "origin/main".into(),
                branch: None,
            },
            push.clone(),
        ]);
        assert!(seq.label().contains("Fetch → Rebase onto 'origin/main' → Push"));
        assert_eq!(seq.refresh_scope(), RefreshScope::All);

        assert_eq!(
            classify_failure(&["fatal: tag 'v1.0' already exists".to_string()]),
            GitFailure::TagAlreadyExists
        );
        assert_eq!(
            classify_failure(&[
                "To /tmp/remote.git".into(),
                " ! [rejected]        v1.0 -> v1.0 (already exists)".into(),
            ]),
            GitFailure::TagAlreadyExists
        );
        assert_eq!(
            classify_failure(&[
                " ! [rejected]        main -> main (non-fast-forward)".into(),
            ]),
            GitFailure::PushRejected
        );

        let parsed = parse_push_porcelain(&[
            "To /tmp/remote.git".into(),
            "*\trefs/heads/feat:refs/heads/feat\t[new branch]".into(),
            "*\trefs/tags/v1.0:refs/tags/v1.0\t[new tag]".into(),
            " \trefs/tags/v1.1:refs/tags/v1.1\ta1b2c3d..e4f5a6b".into(),
            "Done".into(),
        ]);
        assert_eq!(parsed.new_branch.as_deref(), Some("feat"));
        assert_eq!(parsed.pushed_tags, 2);
        assert!(!parsed.branch_up_to_date);

        let parsed_ff = parse_push_porcelain(&[
            " \trefs/heads/main:refs/heads/main\t0123abc..4567def".into(),
        ]);
        assert_eq!(parsed_ff.commit_range.as_deref(), Some("0123abc..4567def"));

        let parsed_up = parse_push_porcelain(&[
            "=\trefs/heads/main:refs/heads/main\t[up to date]".into(),
        ]);
        assert!(parsed_up.branch_up_to_date);

        let patterns = vec!["main".into(), "release/*".into()];
        assert!(matches_protected_branch_patterns("main", &patterns));
        assert!(matches_protected_branch_patterns("release/1.2", &patterns));
        assert!(!matches_protected_branch_patterns("feat/push", &patterns));
    }

    #[test]
    fn merge_options_validate_covers_every_incompatible_pair() {
        let msg = || Some("custom".to_string());
        let ok = |o: MergeOptions| assert_eq!(o.validate(), Ok(()), "{o:?}");
        let clash = |o: MergeOptions, c: MergeOptionConflict| {
            assert_eq!(o.validate(), Err(c), "{o:?}");
        };
        ok(MergeOptions::default());
        for ff in FastForward::ALL {
            ok(MergeOptions {
                ff,
                allow_unrelated: true,
                strategy: Some("ours".into()),
                strategy_option: Some("theirs".into()),
                ..Default::default()
            });
        }
        ok(MergeOptions {
            squash: true,
            no_commit: true,
            ..Default::default()
        });
        ok(MergeOptions {
            ff: FastForward::NoFf,
            no_commit: true,
            ..Default::default()
        });
        ok(MergeOptions {
            ff: FastForward::NoFf,
            message: msg(),
            ..Default::default()
        });
        // Blank messages do not count as `-m`.
        ok(MergeOptions {
            squash: true,
            message: Some("  \n ".into()),
            ..Default::default()
        });

        clash(
            MergeOptions {
                squash: true,
                ff: FastForward::NoFf,
                ..Default::default()
            },
            MergeOptionConflict::SquashNoFf,
        );
        clash(
            MergeOptions {
                squash: true,
                ff: FastForward::FfOnly,
                ..Default::default()
            },
            MergeOptionConflict::SquashFfOnly,
        );
        clash(
            MergeOptions {
                squash: true,
                message: msg(),
                ..Default::default()
            },
            MergeOptionConflict::SquashMessage,
        );
        clash(
            MergeOptions {
                ff: FastForward::FfOnly,
                message: msg(),
                ..Default::default()
            },
            MergeOptionConflict::FfOnlyMessage,
        );
        clash(
            MergeOptions {
                ff: FastForward::FfOnly,
                no_commit: true,
                ..Default::default()
            },
            MergeOptionConflict::FfOnlyNoCommit,
        );
        clash(
            MergeOptions {
                no_commit: true,
                message: msg(),
                ..Default::default()
            },
            MergeOptionConflict::MessageNoCommit,
        );
        assert_eq!(
            MergeOptionConflict::SquashNoFf.flags(),
            ("--squash", "--no-ff")
        );
    }

    #[test]
    fn merge_options_remember_roundtrip_drops_message() {
        let o = MergeOptions {
            ff: FastForward::NoFf,
            squash: false,
            no_commit: true,
            message: Some("secret".into()),
            strategy: Some("ort".into()),
            strategy_option: Some("theirs".into()),
            allow_unrelated: true,
        };
        let text = o.to_remembered();
        assert!(!text.contains("secret"));
        let back = MergeOptions::from_remembered(&text);
        assert_eq!(
            back,
            MergeOptions {
                message: None,
                ..o.clone()
            }
        );
        assert_eq!(MergeOptions::from_remembered("garbage\nff=bogus\n"), MergeOptions::default());
        assert_eq!(o.summary(), "--no-ff --no-commit -s ort -X theirs --allow-unrelated-histories -m");
        assert_eq!(MergeOptions::default().summary(), "");
        assert_eq!(FastForward::Default.next().next().next(), FastForward::Default);
        assert_eq!(FastForward::NoFf.prev(), FastForward::Default);
    }

    #[test]
    fn merge_and_update_project_labels_failures_and_update_info() {
        let squash = GitOp::Merge {
            rev: "feat".into(),
            options: MergeOptions {
                squash: true,
                ..Default::default()
            },
        };
        assert_eq!(squash.label(), "Squash merge 'feat'");
        assert!(squash.supports_smart());
        assert_eq!(squash.refresh_scope(), RefreshScope::All);
        let update = GitOp::UpdateProject {
            mode: Some(UpdateMode::Rebase),
            save_changes: true,
            update_other_branches: true,
        };
        assert_eq!(update.label(), "Update Project");
        assert!(!update.supports_smart());
        assert!(!update.is_read_only());
        assert_eq!(update.refresh_scope(), RefreshScope::All);

        assert_eq!(UpdateMode::from_pull_rebase(None), UpdateMode::Merge);
        assert_eq!(UpdateMode::from_pull_rebase(Some("false")), UpdateMode::Merge);
        for v in ["true", "merges", "interactive", "TRUE"] {
            assert_eq!(UpdateMode::from_pull_rebase(Some(v)), UpdateMode::Rebase, "{v}");
        }

        assert_eq!(
            classify_failure(&["fatal: Not possible to fast-forward, aborting.".to_string()]),
            GitFailure::NotFastForward
        );
        assert_eq!(
            classify_failure(&[" ! [rejected] main -> main (non-fast-forward)".to_string()]),
            GitFailure::PushRejected
        );

        let mut info = UpdateInfo::default();
        assert!(info.is_empty());
        assert_eq!(info.range_label(), "");
        info.ranges.push(UpdatedRange {
            branch: "main".into(),
            before: oid(1),
            after: oid(2),
        });
        assert_eq!(info.range_label(), format!("{}..{}", oid(1).short(), oid(2).short()));
        info.ranges.push(UpdatedRange {
            branch: "dev".into(),
            before: oid(3),
            after: oid(4),
        });
        assert_eq!(info.range_label(), "2 ranges");
        let _ = SkippedBranch {
            branch: "x".into(),
            reason: "diverged".into(),
        };
    }
}
