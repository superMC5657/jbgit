use std::sync::Arc;

use jbgit_core::{FileDiffKey, Oid};
use jbgit_engine::{EngineHandle, TaskRequest};

use crate::syntax::HighlightedDiff;

use super::{AppState, Effect, SyntaxTarget, UiAsyncEvent};

fn lookup_file_diff_oids(
    state: &AppState,
    key: &FileDiffKey,
) -> Option<(Option<Oid>, Option<Oid>)> {
    let bundle = state
        .details
        .cache
        .peek(&key.details)
        .or_else(|| state.stash.details.cache.peek(&key.details))?;
    let change = bundle
        .changes
        .iter()
        .find(|c| c.path == key.path && c.old_path == key.old_path)
        .or_else(|| bundle.changes.iter().find(|c| c.path == key.path))?;
    (change.old_oid.is_some() || change.new_oid.is_some())
        .then_some((change.old_oid, change.new_oid))
}

pub fn execute_effects(
    effects: Vec<Effect>,
    state: &mut AppState,
    engine: Option<&EngineHandle>,
    ui_async_tx: Option<&tokio::sync::mpsc::UnboundedSender<UiAsyncEvent>>,
) {
    for effect in effects {
        match effect {
            Effect::CopyToClipboard(text) => {
                crate::terminal::copy_to_clipboard(&text);
            }
            Effect::ComputeContainingBranches(job) => {
                if let Some(tx) = ui_async_tx.cloned() {
                    tokio::task::spawn_blocking(move || {
                        if let Some(names) = job.run() {
                            let _ = tx.send(UiAsyncEvent::ContainingBranches {
                                oid: job.oid,
                                token: job.token,
                                names,
                            });
                        }
                    });
                } else if let Some(names) = job.run() {
                    let _ = state.apply_ui_async_event(UiAsyncEvent::ContainingBranches {
                        oid: job.oid,
                        token: job.token,
                        names,
                    });
                }
            }
            Effect::RebuildLogGraph(job) => {
                if let Some(tx) = ui_async_tx.cloned() {
                    tokio::task::spawn_blocking(move || {
                        if let Some(rebuilt) = job.run() {
                            let _ = tx.send(UiAsyncEvent::LogGraphRebuilt(Box::new(rebuilt)));
                        }
                    });
                } else if let Some(rebuilt) = job.run() {
                    let _ = state.apply_ui_async_event(UiAsyncEvent::LogGraphRebuilt(Box::new(rebuilt)));
                }
            }
            Effect::RequestSyntaxHighlight(job) => {
                if let Some(tx) = ui_async_tx.cloned() {
                    tokio::task::spawn_blocking(move || {
                        let res = match &job.target {
                            SyntaxTarget::File(diff) => {
                                HighlightedDiff::build_cancellable_file(diff, &job.cancel)
                            }
                            SyntaxTarget::Stage(diff) => {
                                HighlightedDiff::build_cancellable_stage(diff, &job.cancel)
                            }
                        };
                        if let Some(highlighted) = res {
                            let _ = tx.send(UiAsyncEvent::SyntaxHighlighted {
                                key: job.key,
                                epoch: job.epoch,
                                highlighted: Arc::new(highlighted),
                            });
                        }
                    });
                }
            }
            Effect::OpenExternalEditorMessage
            | Effect::OpenExternalEditorFile(_)
            | Effect::OpenExternalMergetool(_)
            | Effect::Quit => {}
            other => {
                let Some(handle) = engine else {
                    continue;
                };
                match other {
                    Effect::RefreshRepoAndStage => {
                        let show_ignored = state.stage_workbench.tree_opts.show_ignored;
                        state.latest_repo_epoch =
                            state.latest_repo_epoch.max(handle.request_repo_snapshot());
                        state.latest_stage_epoch = state
                            .latest_stage_epoch
                            .max(handle.request_stage_status(show_ignored));
                    }
                    Effect::RefreshScope(scope) => {
                        let show_ignored = state.stage_workbench.tree_opts.show_ignored;
                        if scope.needs_graph_refresh() {
                            state.latest_repo_epoch =
                                state.latest_repo_epoch.max(handle.request_repo_snapshot());
                        }
                        if scope.needs_stage_refresh() {
                            state.latest_stage_epoch = state
                                .latest_stage_epoch
                                .max(handle.request_stage_status(show_ignored));
                        }
                    }
                    Effect::RequestCommitDetails(key) => {
                        state.latest_commit_details_epoch = state
                            .latest_commit_details_epoch
                            .max(handle.request_commit_details(key));
                    }
                    Effect::RequestFileDiff(key) => {
                        let known_oids = lookup_file_diff_oids(state, &key);
                        state.latest_file_diff_epoch = state
                            .latest_file_diff_epoch
                            .max(handle.request_file_diff_with_oids(key, known_oids));
                    }
                    Effect::RequestFileHistory(key) => {
                        state.latest_file_history_epoch = state
                            .latest_file_history_epoch
                            .max(handle.request_file_history(key));
                    }
                    Effect::RequestPathMatches(paths, snapshot) => {
                        state.latest_path_epoch = state
                            .latest_path_epoch
                            .max(handle.request_path_matches(paths, snapshot));
                    }
                    Effect::RequestStageFileDiff(key) => {
                        state.latest_stage_diff_epoch = state
                            .latest_stage_diff_epoch
                            .max(handle.request_stage_file_diff(key));
                    }
                    Effect::RequestAmendPreview => {
                        handle.request_amend_preview();
                    }
                    Effect::RequestCommitPrecheck => {
                        handle.request_commit_precheck();
                    }
                    Effect::ApplyStageBuffer {
                        path,
                        new_staged_bytes,
                        file_mode,
                        remove_from_index,
                    } => {
                        handle.send(TaskRequest::ApplyStageBuffer {
                            path,
                            new_staged_bytes,
                            file_mode,
                            remove_from_index,
                        });
                    }
                    Effect::WriteWorktreeBuffer {
                        path,
                        new_local_bytes,
                    } => {
                        handle.send(TaskRequest::WriteWorktreeBuffer {
                            path,
                            new_local_bytes,
                        });
                    }
                    Effect::StageTargets(targets) => {
                        handle.send(TaskRequest::StageTargets { targets });
                    }
                    Effect::StageAllTracked => {
                        handle.send(TaskRequest::StageAllTracked);
                    }
                    Effect::UnstageTargets(targets) => {
                        handle.send(TaskRequest::UnstageTargets { targets });
                    }
                    Effect::DiscardTargets(targets) => {
                        handle.send(TaskRequest::DiscardTargets { targets });
                    }
                    Effect::ExecuteCommit(request) => {
                        state.stage_workbench.commit_in_progress = true;
                        let id = state.next_cli_id;
                        state.next_cli_id += 1;
                        handle.send(TaskRequest::ExecuteCommit { id, request });
                    }
                    Effect::UndoLastCommit => {
                        handle.send(TaskRequest::UndoLastCommit);
                    }
                    Effect::LoadConflictDocument(path) => {
                        handle.send(TaskRequest::LoadConflictDocument { path });
                    }
                    Effect::SaveConflictResolution {
                        path,
                        content,
                        stage,
                    } => {
                        handle.send(TaskRequest::SaveConflictResolution {
                            path,
                            content,
                            stage,
                        });
                    }
                    Effect::RunCli {
                        args,
                        stdin_payload,
                        is_read_only,
                        refresh_after,
                    } => {
                        let id = state.next_cli_id;
                        state.next_cli_id += 1;
                        handle.send(TaskRequest::RunGitCli {
                            id,
                            args,
                            stdin_payload,
                            is_read_only,
                            refresh_after,
                        });
                    }
                    Effect::GitOp(op) => {
                        state.git_ops_running += 1;
                        handle.send(TaskRequest::RunGitOp { op });
                    }
                    Effect::CopyToClipboard(_)
                    | Effect::RequestSyntaxHighlight(_)
                    | Effect::ComputeContainingBranches(_)
                    | Effect::RebuildLogGraph(_)
                    | Effect::OpenExternalEditorMessage
                    | Effect::OpenExternalEditorFile(_)
                    | Effect::OpenExternalMergetool(_)
                    | Effect::Quit => unreachable!(),
                }
            }
        }
    }
}
