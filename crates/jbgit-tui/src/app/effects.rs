use jbgit_core::RequestSlotId;
use jbgit_engine::{EngineBackend, TaskRequest};

use crate::session::SessionHost;

use super::{AppState, Effect};

pub fn execute_effects_with_host(
    effects: Vec<Effect>,
    state: &mut AppState,
    engine: Option<&dyn EngineBackend>,
    host: &mut dyn SessionHost,
) {
    for effect in effects {
        match effect {
            Effect::CopyToClipboard(text) => {
                host.copy_to_clipboard(&text);
            }
            Effect::ComputeContainingBranches(job) => {
                host.spawn_containing_branches(job, state);
            }
            Effect::RebuildLogGraph(job) => {
                host.spawn_rebuild_log_graph(job, state);
            }
            Effect::RequestSyntaxHighlight(job) => {
                host.spawn_syntax_highlight(job, state);
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
                        let show_ignored = state.stage_workbench.tree_opts().show_ignored;
                        let repo_e = handle.request_repo_snapshot();
                        state
                            .request_slots
                            .note_dispatched(RequestSlotId::RepoSnapshot, repo_e);
                        let stage_e = handle.request_stage_status(show_ignored);
                        state
                            .request_slots
                            .note_dispatched(RequestSlotId::StageStatus, stage_e);
                    }
                    Effect::RefreshScope(scope) => {
                        let show_ignored = state.stage_workbench.tree_opts().show_ignored;
                        if scope.needs_graph_refresh() {
                            let repo_e = handle.request_repo_snapshot();
                            state
                                .request_slots
                                .note_dispatched(RequestSlotId::RepoSnapshot, repo_e);
                        }
                        if scope.needs_stage_refresh() {
                            let stage_e = handle.request_stage_status(show_ignored);
                            state
                                .request_slots
                                .note_dispatched(RequestSlotId::StageStatus, stage_e);
                        }
                    }
                    Effect::RequestCommitDetails { slot, key } => {
                        let epoch = handle.request_commit_details(slot, key);
                        state.request_slots.note_dispatched(slot, epoch);
                    }
                    Effect::RequestFileDiff { slot, key } => {
                        let known_oids = state.diff_caches.known_oids_for(&key);
                        let epoch = handle.request_file_diff_with_oids(slot, key, known_oids);
                        state.request_slots.note_dispatched(slot, epoch);
                    }
                    Effect::RequestFileHistory(key) => {
                        let epoch = handle.request_file_history(key);
                        state
                            .request_slots
                            .note_dispatched(RequestSlotId::FileHistoryList, epoch);
                    }
                    Effect::RequestBlame(key) => {
                        let epoch = handle.request_blame(key);
                        state
                            .request_slots
                            .note_dispatched(RequestSlotId::Annotate, epoch);
                    }
                    Effect::RequestPathMatches(paths, snapshot) => {
                        let epoch = handle.request_path_matches(paths, snapshot);
                        state
                            .request_slots
                            .note_dispatched(RequestSlotId::PathFilter, epoch);
                    }
                    Effect::RequestStageFileDiff(key) => {
                        let epoch = handle.request_stage_file_diff(key);
                        state
                            .request_slots
                            .note_dispatched(RequestSlotId::StageFileDiff, epoch);
                    }
                    Effect::RequestAmendPreview => {
                        let epoch = handle.request_amend_preview();
                        state
                            .request_slots
                            .note_dispatched(RequestSlotId::AmendPreview, epoch);
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
                        let _ = handle.dispatch(TaskRequest::ApplyStageBuffer {
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
                        let _ = handle.dispatch(TaskRequest::WriteWorktreeBuffer {
                            path,
                            new_local_bytes,
                        });
                    }
                    Effect::StageTargets(targets) => {
                        let _ = handle.dispatch(TaskRequest::StageTargets { targets });
                    }
                    Effect::StageAllTracked => {
                        let _ = handle.dispatch(TaskRequest::StageAllTracked);
                    }
                    Effect::UnstageTargets(targets) => {
                        let _ = handle.dispatch(TaskRequest::UnstageTargets { targets });
                    }
                    Effect::DiscardTargets(targets) => {
                        let _ = handle.dispatch(TaskRequest::DiscardTargets { targets });
                    }
                    Effect::ExecuteCommit(request) => {
                        state.stage_workbench.set_commit_in_progress(true);
                        let id = state.next_cli_id;
                        state.next_cli_id += 1;
                        let _ = handle.dispatch(TaskRequest::ExecuteCommit { id, request });
                    }
                    Effect::UndoLastCommit => {
                        let _ = handle.dispatch(TaskRequest::UndoLastCommit);
                    }
                    Effect::LoadConflictDocument(path) => {
                        let epoch = handle.request_conflict_document(path);
                        state
                            .request_slots
                            .note_dispatched(RequestSlotId::ConflictDocument, epoch);
                    }
                    Effect::SaveConflictResolution {
                        path,
                        content,
                        stage,
                    } => {
                        let _ = handle.dispatch(TaskRequest::SaveConflictResolution {
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
                        let _ = handle.dispatch(TaskRequest::RunGitCli {
                            id,
                            args,
                            stdin_payload,
                            is_read_only,
                            refresh_after,
                        });
                    }
                    Effect::GitOp(op) => {
                        state.git_ops_running += 1;
                        let _ = handle.dispatch(TaskRequest::RunGitOp { op });
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
