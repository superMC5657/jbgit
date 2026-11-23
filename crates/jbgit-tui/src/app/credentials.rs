//! FIFO of pending `GIT_ASKPASS` / `SSH_ASKPASS` prompts (Spec 12 Issue 08).
//!
//! Prompts from concurrent git processes are queued and answered one at a
//! time through a [`Dialog::Credential`]. Confirming sends the typed value
//! straight to the waiting askpass client; `Esc` / Cancel answers "cancelled"
//! (askpass exits 1). Values are never stored, logged or put into an
//! [`crate::app::Action`].

use std::collections::VecDeque;
use std::fmt;

use jbgit_engine::askpass::AskpassRequest;

use super::{AppState, ModalLayer};
use crate::dialogs::{CredentialDialog, Dialog};

#[derive(Default)]
pub struct CredentialQueue {
    next_id: u64,
    pending: VecDeque<(u64, AskpassRequest)>,
}

impl CredentialQueue {
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    fn push(&mut self, req: AskpassRequest) -> u64 {
        self.next_id += 1;
        self.pending.push_back((self.next_id, req));
        self.next_id
    }

    fn take(&mut self, id: u64) -> Option<AskpassRequest> {
        let pos = self.pending.iter().position(|(i, _)| *i == id)?;
        self.pending.remove(pos).map(|(_, r)| r)
    }

    /// Drops prompts whose git process stopped waiting (timeout / exit).
    fn prune_abandoned(&mut self) -> bool {
        let before = self.pending.len();
        self.pending.retain(|(_, r)| !r.is_abandoned());
        before != self.pending.len()
    }

    fn front(&self) -> Option<(u64, &AskpassRequest)> {
        self.pending.front().map(|(id, r)| (*id, r))
    }
}

impl fmt::Debug for CredentialQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialQueue")
            .field("pending", &self.pending.len())
            .finish()
    }
}

fn credential_id(layer: &ModalLayer) -> Option<u64> {
    match layer {
        ModalLayer::Dialog(d) => match d.as_ref() {
            Dialog::Credential(c) => Some(c.id),
            _ => None,
        },
        _ => None,
    }
}

impl AppState {
    /// Queues a prompt from the askpass bridge and shows it when it is first.
    pub(crate) fn enqueue_credential(&mut self, req: AskpassRequest) {
        self.credentials.push(req);
        self.sync_credential_dialog();
    }

    /// Keeps exactly one credential dialog on screen, for the oldest live
    /// prompt; removes dialogs whose prompt was abandoned.
    pub(crate) fn sync_credential_dialog(&mut self) {
        let pruned = self.credentials.prune_abandoned();
        let front = self
            .credentials
            .front()
            .map(|(id, r)| (id, r.prompt.clone(), r.masked));
        let shown: Vec<u64> = self.modal_stack.iter().filter_map(credential_id).collect();
        let want = front.as_ref().map(|(id, _, _)| *id);
        if shown.iter().any(|id| Some(*id) != want) {
            self.modal_stack
                .retain(|l| credential_id(l).is_none_or(|id| Some(id) == want));
            self.needs_redraw = true;
        }
        if let Some((id, prompt, masked)) = front
            && !shown.contains(&id)
        {
            self.menu_hover = None;
            self.modal_stack
                .push(ModalLayer::Dialog(Box::new(Dialog::Credential(
                    CredentialDialog::new(id, prompt, masked),
                ))));
            self.status_notice = Some(self.i18n.t("toast.credentials_requested").into());
            self.needs_redraw = true;
        } else if pruned {
            self.needs_redraw = true;
        }
    }

    /// Answers the credential dialog on top of the stack (`confirm = false`
    /// cancels). Returns `false` when the top layer is not a credential dialog.
    pub(crate) fn answer_credential(&mut self, confirm: bool) -> bool {
        let Some(id) = self.modal_stack.last().and_then(credential_id) else {
            return false;
        };
        let Some(ModalLayer::Dialog(mut d)) = self.modal_stack.pop() else {
            return false;
        };
        let value = match d.as_mut() {
            Dialog::Credential(c) => c.take_value(),
            _ => return false,
        };
        if let Some(req) = self.credentials.take(id) {
            req.respond(confirm.then_some(value));
        }
        if !confirm {
            self.status_notice = Some(self.i18n.t("toast.credentials_cancelled").into());
        }
        self.sync_credential_dialog();
        true
    }
}
