//! Pure domain model of the high-level Git write operations driven by the
//! branches popup, context menus and dialogs: operation catalogue, CLI step
//! builders, failure classification, ref-name validation, reflog `Recent`
//! branch parsing and interactive-rebase todo rendering.

mod ops;
mod outcome;
mod rebase;
mod refs;

pub use ops::{
    FastForward, FixupKind, GitOp, MergeOptionConflict, MergeOptions, OngoingKind, OngoingStep,
    PatchSource, PullOptionConflict, PullOptions, PushForce, PushTagMode, RebaseOptionError,
    RebaseOptions, ResetMode, UpdateMode, infer_clone_dir_name, short_rev,
};
pub use outcome::{
    GitFailure, GitOpOutcome, PushOutcomeSummary, SkippedBranch, StashEntry, UpdateInfo,
    UpdatedRange, classify_failure, parse_push_porcelain,
};
pub use rebase::{
    RewritePlan, SquashTodoPlan, TodoAction, TodoEntry, default_squash_messages, plan_squash_todo,
    render_todo,
};
pub use refs::{
    RefNameError, fuzzy_filter_paths, fuzzy_indices, is_protected_branch,
    matches_protected_branch_patterns, parse_recent_checkouts, sanitize_branch_name,
    sanitize_branch_name_on_typing, validate_ref_name, validate_ref_name_typed,
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
    fn classify_worktree_failures() {
        let line = |s: &str| vec![s.to_string()];
        let expect = |branch: &str, path: &str| GitFailure::BranchCheckedOutInWorktree {
            branch: branch.into(),
            path: path.into(),
        };
        assert_eq!(
            classify_failure(&line("fatal: 'feature/x' is already checked out at '/w/wt-x'")),
            expect("feature/x", "/w/wt-x")
        );
        assert_eq!(
            classify_failure(&line("fatal: 'dev' is already used by worktree at 'C:/w/dev'")),
            expect("dev", "C:/w/dev")
        );
        assert_eq!(
            classify_failure(&[
                "Preparing worktree (checking out 'dev')".into(),
                "error: Cannot delete branch 'dev' checked out at '/w/dev'".into(),
            ]),
            expect("dev", "/w/dev")
        );
        assert_eq!(
            classify_failure(&line(
                "error: cannot delete branch 'dev' used by worktree at '/w/dev'"
            )),
            expect("dev", "/w/dev")
        );
        assert_eq!(
            classify_failure(&line(
                "fatal: '/w/dev' contains modified or untracked files, use --force to delete it"
            )),
            GitFailure::WorktreeDirty
        );
        assert_eq!(
            classify_failure(&[
                "fatal: cannot remove a locked working tree, lock reason: usb".into(),
                "use 'remove -f -f' to override or unlock first".into(),
            ]),
            GitFailure::WorktreeLocked
        );
        let add = GitOp::AddWorktree {
            path: "/w/x".into(),
            branch: crate::WorktreeBranch::Existing("x".into()),
        };
        assert!(!add.is_network());
        assert_eq!(add.refresh_scope(), RefreshScope::RefsAndGraph);
        assert_eq!(GitOp::PruneWorktrees.label(), "Prune worktrees");
    }

    #[test]
    fn apply_patch_failures_and_labels() {
        let lines = |s: &str| s.lines().map(str::to_string).collect::<Vec<_>>();
        assert_eq!(
            classify_failure(&lines(
                "error: patch failed: f.txt:1\nerror: f.txt: patch does not apply"
            )),
            GitFailure::PatchDoesNotApply {
                detail: "error: f.txt: patch does not apply".into()
            }
        );
        assert_eq!(
            classify_failure(&lines(
                "Applied patch to 'f.txt' with conflicts.\nApplied patch to 'g.txt' cleanly.\nU f.txt"
            )),
            GitFailure::PatchConflicts {
                paths: vec!["f.txt".into()]
            }
        );
        let file = GitOp::ApplyPatch {
            source: crate::PatchInput::File("/tmp/fix.patch".into()),
            options: crate::ApplyOptions::with_mode(crate::ApplyMode::ThreeWay),
        };
        assert_eq!(file.label(), "Apply patch 'fix.patch'");
        assert_eq!(file.refresh_scope(), RefreshScope::All);
        assert!(!file.is_network() && !file.is_read_only());
        let clip = GitOp::ApplyPatch {
            source: crate::PatchInput::Clipboard,
            options: crate::ApplyOptions::with_mode(crate::ApplyMode::WorkingTree),
        };
        assert_eq!(clip.label(), "Apply patch from clipboard");
    }

    #[test]
    fn remote_ops_are_classified_for_network_and_refresh() {
        let fetch_one = GitOp::FetchRemote { name: "fork".into() };
        assert!(fetch_one.is_network());
        assert!(
            GitOp::Smart(Box::new(GitOp::Pull {
                options: PullOptions::default(),
            }))
            .is_network()
        );
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
        let text = render_todo(&todo, false, |i| format!("'/tmp/m{i}'"));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 7);
        assert!(lines[0].starts_with("pick 0101"));
        assert!(lines[1].starts_with("pick 0202"));
        assert_eq!(
            lines[2],
            "exec git commit --amend --only --allow-empty -F '/tmp/m1'"
        );
        assert!(lines[3].starts_with("drop 0303"));
        assert!(lines[5].starts_with("fixup 0505"));
        assert!(lines[6].ends_with("'/tmp/m4'"));
        assert!(!text.contains("reword") && !text.contains("squash"));
        assert!(!text.contains("--no-verify"));

        // Explicit no_verify = true emits `--no-verify`.
        let no_verify_text = render_todo(&todo, true, |i| format!("'/tmp/m{i}'"));
        assert!(no_verify_text.contains(
            "exec git commit --amend --only --allow-empty --no-verify -F '/tmp/m1'"
        ));
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

        let restore = GitOp::RestorePaths {
            rev: oid(3).to_string(),
            paths: vec!["src/main.rs".into()],
            staged: false,
            worktree: true,
        };
        assert!(!restore.is_read_only());
        assert!(
            GitOp::PatchToClipboard {
                source: PatchSource::CommitFile {
                    oid: oid(3),
                    path: "src/main.rs".into(),
                },
                reverse: false,
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
            no_verify: true,
            expected_remote_oid: Some(oid(9)),
        };
        assert_eq!(push.label(), "Push 'feat' → 'upstream/feat-v2'");
        assert_eq!(push.refresh_scope(), RefreshScope::RefsAndGraph);

        let seq = GitOp::Sequence(vec![
            GitOp::Fetch,
            GitOp::Rebase {
                options: RebaseOptions::onto("origin/main", None),
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
            update_submodules: true,
        };
        assert_eq!(update.label(), "Update Project");
        assert!(!update.supports_smart());
        assert!(!update.is_read_only());
        assert_eq!(update.refresh_scope(), RefreshScope::All);

        let set_id = GitOp::SetIdentity {
            name: "Alice".into(),
            email: "alice@example.com".into(),
            global: false,
        };
        assert_eq!(set_id.label(), "Configure Git identity");
        assert!(!set_id.is_network());
        assert_eq!(set_id.refresh_scope(), RefreshScope::All);

        let set_up = GitOp::SetUpstream {
            branch: "feat".into(),
            upstream: "origin/feat".into(),
        };
        assert_eq!(set_up.label(), "Set upstream of 'feat' → 'origin/feat'");
        assert!(!set_up.is_network());
        assert_eq!(set_up.refresh_scope(), RefreshScope::RefsAndGraph);

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

    #[test]
    fn pull_options_validate_matrix_and_remember_roundtrip() {
        let ok = |o: PullOptions| assert_eq!(o.validate(), Ok(()), "{o:?}");
        let clash = |o: PullOptions, c: PullOptionConflict| {
            assert_eq!(o.validate(), Err(c), "{o:?}");
        };

        ok(PullOptions::default());
        ok(PullOptions {
            rebase: true,
            no_verify: true,
            remote: Some("origin".into()),
            branch: Some("main".into()),
            ..Default::default()
        });
        ok(PullOptions {
            ff: FastForward::FfOnly,
            no_commit: true,
            no_verify: true,
            ..Default::default()
        });
        ok(PullOptions {
            ff: FastForward::NoFf,
            no_commit: true,
            ..Default::default()
        });
        ok(PullOptions {
            squash: true,
            no_commit: true,
            no_verify: true,
            ..Default::default()
        });

        clash(
            PullOptions {
                rebase: true,
                squash: true,
                ..Default::default()
            },
            PullOptionConflict::RebaseSquash,
        );
        clash(
            PullOptions {
                rebase: true,
                ff: FastForward::NoFf,
                ..Default::default()
            },
            PullOptionConflict::RebaseNoFf,
        );
        clash(
            PullOptions {
                rebase: true,
                ff: FastForward::FfOnly,
                ..Default::default()
            },
            PullOptionConflict::RebaseFfOnly,
        );
        clash(
            PullOptions {
                rebase: true,
                no_commit: true,
                ..Default::default()
            },
            PullOptionConflict::RebaseNoCommit,
        );
        clash(
            PullOptions {
                squash: true,
                ff: FastForward::NoFf,
                ..Default::default()
            },
            PullOptionConflict::SquashNoFf,
        );
        clash(
            PullOptions {
                squash: true,
                ff: FastForward::FfOnly,
                ..Default::default()
            },
            PullOptionConflict::SquashFfOnly,
        );

        assert_eq!(PullOptionConflict::RebaseSquash.flags(), ("--rebase", "--squash"));
        assert_eq!(PullOptionConflict::RebaseNoFf.flags(), ("--rebase", "--no-ff"));
        assert_eq!(PullOptionConflict::RebaseFfOnly.flags(), ("--rebase", "--ff-only"));
        assert_eq!(PullOptionConflict::RebaseNoCommit.flags(), ("--rebase", "--no-commit"));
        assert_eq!(PullOptionConflict::SquashNoFf.flags(), ("--squash", "--no-ff"));
        assert_eq!(PullOptionConflict::SquashFfOnly.flags(), ("--squash", "--ff-only"));

        let saved_src = PullOptions {
            remote: Some("upstream".into()),
            branch: Some("dev".into()),
            rebase: false,
            ff: FastForward::NoFf,
            squash: false,
            no_commit: true,
            no_verify: true,
        };
        let text = saved_src.to_remembered();
        assert!(!text.contains("upstream") && !text.contains("dev"));
        assert_eq!(
            PullOptions::from_remembered(&text),
            PullOptions {
                remote: None,
                branch: None,
                ..saved_src.clone()
            }
        );
        assert_eq!(
            saved_src.summary(),
            "--no-ff --no-commit --no-verify upstream dev"
        );
        assert_eq!(PullOptions::default().summary(), "");
    }

    #[test]
    fn rebase_options_validate_version_and_summary() {
        use crate::GitVersion;

        let v237 = Some(GitVersion::new(2, 37, 0));
        let v238 = Some(GitVersion::new(2, 38, 0));
        let v217 = Some(GitVersion::new(2, 17, 0));

        assert_eq!(
            RebaseOptions::default().validate(v238),
            Err(RebaseOptionError::MissingOnto)
        );
        assert_eq!(
            RebaseOptions {
                root: true,
                ..Default::default()
            }
            .validate(v238),
            Ok(())
        );
        assert_eq!(
            RebaseOptions {
                root: true,
                upstream: Some("main".into()),
                ..Default::default()
            }
            .validate(v238),
            Err(RebaseOptionError::RootWithUpstream)
        );
        assert_eq!(
            RebaseOptions {
                onto: "main".into(),
                update_refs: true,
                ..Default::default()
            }
            .validate(v237),
            Err(RebaseOptionError::UpdateRefsUnsupported(GitVersion::new(2, 37, 0)))
        );
        assert_eq!(
            RebaseOptions {
                onto: "main".into(),
                update_refs: true,
                ..Default::default()
            }
            .validate(v238),
            Ok(())
        );
        assert_eq!(
            RebaseOptions {
                onto: "main".into(),
                rebase_merges: true,
                ..Default::default()
            }
            .validate(v217),
            Err(RebaseOptionError::RebaseMergesUnsupported(GitVersion::new(2, 17, 0)))
        );

        let full = RebaseOptions {
            onto: "main".into(),
            upstream: Some("old-base".into()),
            branch: Some("feat".into()),
            interactive: true,
            rebase_merges: true,
            update_refs: true,
            keep_empty: true,
            autosquash: true,
            root: false,
        };
        assert_eq!(
            full.summary(),
            "-i --rebase-merges --update-refs --keep-empty --autosquash --onto main old-base feat"
        );
        let remembered = RebaseOptions::from_remembered(&full.to_remembered());
        assert_eq!(
            remembered,
            RebaseOptions {
                onto: String::new(),
                upstream: None,
                branch: None,
                ..full
            }
        );
    }

    #[test]
    fn todo_edit_update_ref_and_non_contiguous_squash_plan() {
        let entries = vec![
            TodoEntry {
                action: TodoAction::Edit,
                oid: oid(1),
                subject: "first".into(),
                message: None,
            },
            TodoEntry::update_ref("refs/heads/stacked"),
            TodoEntry::pick(oid(2), "second"),
        ];
        let rendered = render_todo(&entries, false, |_| unreachable!());
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("edit 0101"));
        assert_eq!(lines[1], "update-ref refs/heads/stacked");
        assert!(lines[2].starts_with("pick 0202"));

        assert_eq!(TodoAction::from_key('e'), Some(TodoAction::Edit));
        assert_eq!(TodoAction::from_key('x'), Some(TodoAction::Drop));
        assert_eq!(TodoAction::UpdateRef.next(), TodoAction::UpdateRef);
        assert!(!TodoAction::UpdateRef.is_editable());

        // Non-contiguous squash: [c1, c2, c3, c4, c5] selecting [c2, c4] ->
        // [pick c1, pick c2, squash c4, pick c3, pick c5] with combined msg "m2\n\nm4" at index 2.
        let plan = RewritePlan {
            base: Some(oid(10)),
            entries: vec![
                TodoEntry::pick(oid(1), "c1"),
                TodoEntry::pick(oid(2), "c2"),
                TodoEntry::pick(oid(3), "c3"),
                TodoEntry::pick(oid(4), "c4"),
                TodoEntry::pick(oid(5), "c5"),
            ],
            messages: vec!["m1".into(), "m2".into(), "m3".into(), "m4".into(), "m5".into()],
        };
        let sq = plan_squash_todo(&plan, &[oid(2), oid(4)]).expect("non-contiguous squash");
        assert!(sq.reordered);
        assert_eq!(sq.base, Some(oid(10)));
        assert_eq!(sq.combined_message, "m2\n\nm4");
        assert_eq!(sq.message_entry_idx, 2);
        let actual: Vec<(Oid, TodoAction)> = sq.todo.iter().map(|e| (e.oid, e.action)).collect();
        assert_eq!(
            actual,
            vec![
                (oid(1), TodoAction::Pick),
                (oid(2), TodoAction::Pick),
                (oid(4), TodoAction::Squash),
                (oid(3), TodoAction::Pick),
                (oid(5), TodoAction::Pick),
            ]
        );

        // Selecting a commit outside the plan returns None.
        assert!(plan_squash_todo(&plan, &[oid(2), oid(99)]).is_none());
        // Single commit returns None.
        assert!(plan_squash_todo(&plan, &[oid(2)]).is_none());
    }

    #[test]
    fn branch_name_sanitizer_empty_cherry_pick_and_issue_06_ops() {
        // Real-time space -> `_` and invalid char stripping (GitRefNameValidator).
        assert_eq!(sanitize_branch_name_on_typing("feat my branch", "_"), "feat_my_branch");
        assert_eq!(sanitize_branch_name_on_typing("--..//a..b~^:?*\"[\\@{c", "_"), "a.bc");
        assert_eq!(sanitize_branch_name_on_typing("a//b/.c/d./e", "_"), "a/b/c/d/e");
        assert_eq!(sanitize_branch_name("feat/foo.lock.", "_"), "feat/foo");
        assert_eq!(sanitize_branch_name("my   topic / sub/", "_"), "my_topic_/_sub");

        // EmptyCherryPick stderr classification.
        let empty_lines = vec![
            "The previous cherry-pick is now empty, possibly due to conflict resolution.".to_string(),
            "If you wish to commit it anyway, use:".to_string(),
            "    git commit --allow-empty".to_string(),
            "Otherwise, please use 'git cherry-pick --skip'".to_string(),
        ];
        assert_eq!(classify_failure(&empty_lines), GitFailure::EmptyCherryPick);

        // New / extended GitOp labels & refresh scopes.
        let unset = GitOp::UnsetUpstream {
            branch: "feat".into(),
        };
        assert_eq!(unset.label(), "Unset upstream of 'feat'");
        assert_eq!(unset.refresh_scope(), RefreshScope::RefsAndGraph);
        assert_eq!(
            GitOp::CherryPick {
                oids: vec![oid(1)],
                no_commit: true,
                record_origin: false,
                mainline: Some(1),
            }
            .label(),
            "Cherry-pick (-n) 1 commit(s)"
        );
        assert_eq!(
            GitOp::Revert {
                oids: vec![oid(1), oid(2)],
                no_commit: true,
                mainline: Some(2),
            }
            .label(),
            "Revert (-n) 2 commit(s)"
        );
        assert_eq!(
            GitOp::CherryPickAllowEmpty.label(),
            "Cherry-pick (allow empty)"
        );
        assert_eq!(
            GitOp::StashApply {
                index: 0,
                pop: false,
                reinstate_index: true,
            }
            .label(),
            "Apply stash@{0} (--index)"
        );
    }
}
