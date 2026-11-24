//! Apply Patch and Browse Repository at Revision on the app side (Spec 12
//! Issue 13 + Spec 13 Issue 10): modal actions, engine requests and their replies.

use std::path::Path;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use jbgit_core::{ApplyMode, Oid, PatchCheck, RequestSlotId, TreeBlob, TreeKey, TreeListing};

use crate::patch_view::{ApplyPatchAction, ApplyPatchFocus, ApplyPatchState};
use crate::tree_browser::TreeBrowserState;

use super::{Action, AppState, Effect, ModalLayer, ScrollPane, SyntaxJob, SyntaxTarget};

impl AppState {
    pub(crate) fn dispatch_browse_action(
        &mut self,
        action: &Action,
        effects: &mut Vec<Effect>,
    ) -> bool {
        match action {
            Action::OpenApplyPatch(source) => {
                if let Err(msg) = self.require_no_ongoing() {
                    self.status_notice = Some(msg);
                    return true;
                }
                self.modal_stack.retain(|l| {
                    !matches!(
                        l,
                        ModalLayer::ContextMenu { .. } | ModalLayer::ApplyPatch(_)
                    )
                });
                self.blur_active_pane(false);
                let s = ApplyPatchState::new(source.clone());
                effects.push(Effect::RequestPatchCheck(s.key()));
                self.push_modal(ModalLayer::ApplyPatch(Box::new(s)));
            }
            Action::ApplyPatch(sub) => {
                self.handle_apply_patch_action(sub, effects);
            }
            Action::RememberPatchDir(dir) => {
                self.last_patch_dir = Some(dir.clone());
            }
            Action::ExportTreeBlob { oid, path, dest } => {
                effects.push(Effect::ExportTreeBlob {
                    oid: *oid,
                    path: path.clone(),
                    dest: dest.clone(),
                });
            }
            Action::OpenTreeBrowser { oid } => {
                let title = self
                    .repo_snapshot
                    .as_deref()
                    .and_then(|s| s.commit(oid))
                    .map_or_else(|| oid.short(), |c| format!("{} {}", oid.short(), c.subject));
                self.modal_stack.retain(|l| {
                    !matches!(
                        l,
                        ModalLayer::ContextMenu { .. } | ModalLayer::TreeBrowser(_)
                    )
                });
                self.blur_active_pane(false);
                self.push_modal(ModalLayer::TreeBrowser(Box::new(TreeBrowserState::new(
                    *oid, title,
                ))));
                self.pump_tree_requests(effects);
            }
            Action::BrowseRevisionExpr(expr) => {
                let expr = expr.trim();
                match self
                    .repo_snapshot
                    .as_deref()
                    .and_then(|s| s.resolve_revision(expr))
                {
                    Some(oid) => effects.extend(self.dispatch(Action::OpenTreeBrowser { oid })),
                    None => {
                        self.status_notice =
                            Some(self.i18n.t_fmt("toast.goto_not_found", &[("expr", expr)]));
                    }
                }
            }
            Action::TreeBrowser(sub) => {
                if let Some(s) = self.modal_stack.tree_browser_mut() {
                    s.apply(sub);
                }
                self.pump_tree_requests(effects);
            }
            _ => return false,
        }
        true
    }

    fn handle_apply_patch_action(&mut self, sub: &ApplyPatchAction, effects: &mut Vec<Effect>) {
        let now = self.now();
        match sub {
            ApplyPatchAction::SetMode(mode) => {
                if let Some(s) = self.modal_stack.apply_patch_mut() {
                    let next = mode.unwrap_or_else(|| s.options.mode.next());
                    if next != s.options.mode {
                        s.options.mode = next;
                        s.pending_check_due = None;
                        if !s.all_unchecked() && s.current_check().is_none() {
                            effects.push(Effect::RequestPatchCheck(s.key()));
                        }
                    }
                }
            }
            ApplyPatchAction::ToggleReverse => {
                if let Some(s) = self.modal_stack.apply_patch_mut() {
                    s.options.reverse = !s.options.reverse;
                    s.schedule_check(now);
                }
            }
            ApplyPatchAction::AdjustStrip(delta) => {
                if let Some(s) = self.modal_stack.apply_patch_mut() {
                    let cur = s.options.effective_strip() as i16;
                    let next = (cur + i16::from(*delta)).clamp(0, 9) as u8;
                    let next_opt = (next != 1).then_some(next);
                    if next_opt != s.options.strip {
                        s.options.strip = next_opt;
                        s.on_path_transform_changed();
                        s.schedule_check(now);
                    }
                }
            }
            ApplyPatchAction::StartDirEdit => {
                if let Some(s) = self.modal_stack.apply_patch_mut() {
                    s.editing_dir = Some(s.options.directory.clone().unwrap_or_default());
                }
            }
            ApplyPatchAction::DirEditKey(key) => {
                if let Some(s) = self.modal_stack.apply_patch_mut()
                    && let Some(mut draft) = s.editing_dir.take()
                {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    let alt = key.modifiers.contains(KeyModifiers::ALT);
                    match key.code {
                        KeyCode::Esc => {
                            // Cancel without modifying options.directory.
                        }
                        KeyCode::Enter => {
                            let trimmed = draft.trim().to_string();
                            let next = (!trimmed.is_empty()).then_some(trimmed);
                            if next != s.options.directory {
                                s.options.directory = next;
                                s.on_path_transform_changed();
                                s.schedule_check(now);
                            }
                        }
                        KeyCode::Backspace => {
                            draft.pop();
                            s.editing_dir = Some(draft);
                        }
                        KeyCode::Char('u') if ctrl => {
                            draft.clear();
                            s.editing_dir = Some(draft);
                        }
                        KeyCode::Char(c) if !ctrl && !alt && !c.is_control() => {
                            draft.push(c);
                            s.editing_dir = Some(draft);
                        }
                        _ => {
                            s.editing_dir = Some(draft);
                        }
                    }
                }
            }
            ApplyPatchAction::Move(delta) => {
                if let Some(s) = self.modal_stack.apply_patch_mut() {
                    s.move_selection(*delta);
                }
            }
            ApplyPatchAction::Select(idx) => {
                if let Some(s) = self.modal_stack.apply_patch_mut() {
                    s.select_row(*idx);
                }
            }
            ApplyPatchAction::ToggleFile(idx) => {
                if let Some(s) = self.modal_stack.apply_patch_mut()
                    && s.toggle_file_checked(*idx)
                {
                    s.schedule_check(now);
                }
            }
            ApplyPatchAction::ToggleAll => {
                if let Some(s) = self.modal_stack.apply_patch_mut()
                    && s.toggle_all_checked()
                {
                    s.schedule_check(now);
                }
            }
            ApplyPatchAction::TogglePreview => {
                if let Some(s) = self.modal_stack.apply_patch_mut() {
                    s.show_preview = !s.show_preview;
                    s.preview_scroll.reset();
                    if !s.show_preview {
                        s.focus = ApplyPatchFocus::Files;
                    }
                }
            }
            ApplyPatchAction::OpenPreview => {
                if let Some(s) = self.modal_stack.apply_patch_mut() {
                    s.show_preview = true;
                    s.preview_scroll.reset();
                }
            }
            ApplyPatchAction::ToggleFocus => {
                if let Some(s) = self.modal_stack.apply_patch_mut()
                    && s.show_preview
                {
                    s.focus = match s.focus {
                        ApplyPatchFocus::Files => ApplyPatchFocus::Preview,
                        ApplyPatchFocus::Preview => ApplyPatchFocus::Files,
                    };
                }
            }
            ApplyPatchAction::ScrollPreview(delta) => {
                self.scroll_pane(ScrollPane::ApplyPatchPreview, *delta);
            }
            ApplyPatchAction::Escape => {
                let close_preview = self
                    .modal_stack
                    .apply_patch()
                    .is_some_and(|s| s.show_preview && s.focus == ApplyPatchFocus::Preview);
                if close_preview {
                    if let Some(s) = self.modal_stack.apply_patch_mut() {
                        s.focus = ApplyPatchFocus::Files;
                    }
                } else {
                    effects.extend(self.dispatch(Action::PopModal));
                }
            }
            ApplyPatchAction::Confirm => {
                for a in crate::patch_view::confirm_actions(self) {
                    effects.extend(self.dispatch(a));
                }
            }
        }
    }

    /// Issues the next queued directory listing, fuzzy index, and selected file's preview.
    pub(crate) fn pump_tree_requests(&mut self, effects: &mut Vec<Effect>) {
        let Some(s) = self.modal_stack.tree_browser_mut() else {
            return;
        };
        if let Some(key) = s.next_request() {
            effects.push(Effect::RequestTree(key));
        }
        if let Some(oid) = s.take_wanted_index_request() {
            effects.push(Effect::RequestTreeIndex(oid));
        }
        if let Some(key) = s.wanted_preview() {
            s.preview_requested = Some(key.clone());
            s.preview_syntax = None;
            effects.push(Effect::RequestTreeBlob(key));
        }
    }

    pub(crate) fn on_patch_checked(&mut self, check: Arc<PatchCheck>, epoch: u64) {
        if !self.request_slots.accept(RequestSlotId::PatchCheck, epoch) {
            return;
        }
        if let Some(s) = self.modal_stack.apply_patch_mut()
            && s.key() == check.key
        {
            if !check.applies() {
                if check.needs_three_way_index_fallback() {
                    self.status_notice =
                        Some(self.i18n.t("dialog.apply_patch.three_way_no_index").into());
                } else if s.options.mode != ApplyMode::ThreeWay {
                    self.status_notice = Some(self.i18n.t("dialog.apply_patch.try_3way").into());
                }
            }
            s.on_check(check);
        }
    }

    pub(crate) fn on_tree_loaded(
        &mut self,
        listing: Arc<TreeListing>,
        epoch: u64,
        effects: &mut Vec<Effect>,
    ) {
        if !self.request_slots.accept(RequestSlotId::TreeBrowser, epoch) {
            return;
        }
        if let Some(s) = self.modal_stack.tree_browser_mut() {
            s.on_listing(listing);
        }
        self.pump_tree_requests(effects);
    }

    pub(crate) fn on_tree_load_failed(
        &mut self,
        key: &TreeKey,
        message: String,
        epoch: u64,
        effects: &mut Vec<Effect>,
    ) {
        if !self.request_slots.accept(RequestSlotId::TreeBrowser, epoch) {
            return;
        }
        if let Some(s) = self.modal_stack.tree_browser_mut() {
            s.on_listing_failed(key, message);
        }
        self.pump_tree_requests(effects);
    }

    pub(crate) fn on_tree_index_loaded(&mut self, oid: Oid, paths: Arc<Vec<String>>, epoch: u64) {
        if !self.request_slots.accept(RequestSlotId::TreeIndex, epoch) {
            return;
        }
        if let Some(s) = self.modal_stack.tree_browser_mut() {
            s.on_index_loaded(oid, paths);
        }
    }

    pub(crate) fn on_tree_blob_exported(
        &mut self,
        _path: &str,
        dest: &Path,
        result: Result<u64, String>,
    ) {
        match result {
            Ok(bytes) => {
                let dest_s = dest.display().to_string();
                let bytes_s = bytes.to_string();
                self.status_notice = Some(self.i18n.t_fmt(
                    "view.tree.export_ok",
                    &[("dest", &dest_s), ("bytes", &bytes_s)],
                ));
            }
            Err(err) => {
                self.status_notice = Some(
                    self.i18n
                        .t_fmt("view.tree.export_err", &[("error", &err)]),
                );
            }
        }
    }

    pub(crate) fn on_tree_blob_loaded(
        &mut self,
        blob: Arc<TreeBlob>,
        epoch: u64,
        effects: &mut Vec<Effect>,
    ) {
        if !self.request_slots.accept(RequestSlotId::TreeBlob, epoch) {
            return;
        }
        let Some(s) = self.modal_stack.tree_browser_mut() else {
            return;
        };
        if s.preview_requested.as_ref() != Some(&blob.key) {
            return;
        }
        s.preview = Some(Arc::clone(&blob));
        s.preview_syntax = None;
        s.preview_scroll.reset();
        let jbgit_core::TreeBlobContent::Text(text) = &blob.content else {
            return;
        };
        if !self.config.jbgit.diff.syntax_highlight || text.is_empty() {
            return;
        }
        if let Some((epoch, cancel)) = self.tree_syntax.begin_cancelable(blob.key.clone()) {
            effects.push(Effect::RequestSyntaxHighlight(SyntaxJob {
                epoch,
                target: SyntaxTarget::TreeBlob(Arc::clone(&blob)),
                cancel,
            }));
        }
    }
}
