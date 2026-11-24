//! Apply Patch and Browse Repository at Revision on the app side (Spec 12
//! Issue 13): modal actions, engine requests and their replies.

use std::sync::Arc;

use jbgit_core::{ApplyMode, PatchCheck, RequestSlotId, TreeBlob, TreeKey, TreeListing};

use crate::patch_view::ApplyPatchState;
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
            Action::ApplyPatchSetMode(mode) => {
                if let Some(s) = self.modal_stack.apply_patch_mut() {
                    let next = mode.unwrap_or_else(|| s.mode.next());
                    if next != s.mode {
                        s.mode = next;
                        s.scroll.reset();
                        if s.current_check().is_none() {
                            effects.push(Effect::RequestPatchCheck(s.key()));
                        }
                    }
                }
            }
            Action::ApplyPatchScroll(delta) => {
                self.scroll_pane(ScrollPane::ApplyPatch, *delta);
            }
            Action::ApplyPatchConfirm => {
                for a in crate::patch_view::confirm_actions(self) {
                    effects.extend(self.dispatch(a));
                }
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

    /// Issues the next queued directory listing and the selected file's preview.
    pub(crate) fn pump_tree_requests(&mut self, effects: &mut Vec<Effect>) {
        let Some(s) = self.modal_stack.tree_browser_mut() else {
            return;
        };
        if let Some(key) = s.next_request() {
            effects.push(Effect::RequestTree(key));
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
            // 3-way is the natural fallback: suggest it right away.
            if !check.applies() && s.mode != ApplyMode::ThreeWay {
                self.status_notice = Some(self.i18n.t("dialog.apply_patch.try_3way").into());
            }
            s.check = Some(check);
            s.scroll.reset();
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
