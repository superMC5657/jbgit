//! Acceptance tests for Spec 12 Issue 14 repository capabilities:
//! granular signature verification & Log column, read-only submodule handling,
//! Git LFS pointer placeholders, and shallow clone / unshallow workflows.

use std::sync::Arc;

use crossterm::event::MouseEventKind;
use jbgit_core::{
    CommitChangesBundle, CommitSignature, DetailsKey, Oid, SignatureStatus, WorkspaceTab,
};
use jbgit_engine::{EngineEvent, TaskRequest};

use crate::app::Action;
use crate::hit_test::HitTarget;
use crate::log_view::PresentationToggle;

use super::{TestSession, oid, screen_text, snapshot};

fn seeded_log_session() -> TestSession {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::SelectLogRow(0));
    ts
}

fn deliver_commit_sig_details(ts: &mut TestSession, target_oid: Oid, signature: CommitSignature) {
    let mut details = super::make_details(target_oid, vec![], "signed commit subject");
    details.signature = signature;
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle: Arc::new(CommitChangesBundle {
            key: DetailsKey::single(target_oid),
            details: vec![details],
            changes: vec![],
        }),
        epoch: ts
            .state()
            .request_slots
            .latest(jbgit_core::RequestSlotId::LogCommitDetails)
            .max(1),
    });
}

#[test]
fn granular_signature_statuses_render_badges_signer_and_key_in_details() {
    let cases = [
        (
            SignatureStatus::Good,
            Some("Alice <alice@example.com>"),
            Some("0xAAAA1111"),
            "Good signature",
        ),
        (
            SignatureStatus::GoodUnknownValidity,
            Some("Bob <bob@example.com>"),
            Some("0xBBBB2222"),
            "unknown trust validity",
        ),
        (
            SignatureStatus::Expired { key_expired: false },
            Some("Carol <carol@example.com>"),
            Some("0xCCCC3333"),
            "signature has expired",
        ),
        (
            SignatureStatus::Expired { key_expired: true },
            Some("Dave <dave@example.com>"),
            Some("0xDDDD4444"),
            "signing key has expired",
        ),
        (
            SignatureStatus::Revoked,
            None,
            Some("0xEEEE5555"),
            "revoked",
        ),
        (
            SignatureStatus::CannotCheck,
            None,
            Some("0xFFFF6666"),
            "missing key",
        ),
        (
            SignatureStatus::Bad,
            Some("Mallory <evil@example.com>"),
            Some("0x99990000"),
            "BAD signature",
        ),
    ];

    for (status, signer, key, expected_desc) in cases {
        let mut ts = seeded_log_session();
        deliver_commit_sig_details(
            &mut ts,
            oid(12),
            CommitSignature {
                status,
                signer: signer.map(str::to_string),
                key: key.map(str::to_string),
            },
        );
        let text = screen_text(&ts.render(220, 36));
        assert!(
            text.contains(expected_desc),
            "expected `{expected_desc}` for {status:?} in:\n{text}"
        );
        if let Some(s) = signer {
            assert!(text.contains(s), "missing signer `{s}` in:\n{text}");
        }
        if let Some(k) = key {
            assert!(text.contains(k), "missing key `{k}` in:\n{text}");
        }
    }
}

#[test]
fn log_signature_column_requests_batch_and_renders_icons_and_tooltip() {
    let mut ts = seeded_log_session();
    let _ = ts.take_requests();

    // Disabled by default: no signature verification batch dispatched on snapshot load.
    assert!(!ts.state().log.presentation.show_signature_column);

    // Enable the signature column: requests verification for visible commits.
    ts.dispatch(Action::TogglePresentation(
        PresentationToggle::ShowSignatureColumn,
    ));
    assert!(ts.state().log.presentation.show_signature_column);

    let sig_req = ts
        .take_requests()
        .into_iter()
        .find_map(|(req, _)| match req {
            TaskRequest::VerifyCommitSignatures { oids } => Some(oids),
            _ => None,
        })
        .expect("expected TaskRequest::VerifyCommitSignatures when enabling column");
    assert!(sig_req.contains(&oid(12)));
    assert!(sig_req.contains(&oid(11)));

    // Deliver granular verification results for the top commits.
    ts.deliver_engine_event(EngineEvent::CommitSignaturesLoaded {
        signatures: vec![
            (
                oid(12),
                CommitSignature {
                    status: SignatureStatus::Good,
                    signer: Some("Alice <alice@x.io>".into()),
                    key: Some("1111AAAA".into()),
                },
            ),
            (
                oid(11),
                CommitSignature {
                    status: SignatureStatus::Bad,
                    signer: Some("Bad Actor <bad@x.io>".into()),
                    key: Some("2222BBBB".into()),
                },
            ),
            (oid(10), CommitSignature::unsigned()),
        ],
        epoch: ts
            .state()
            .request_slots
            .latest(jbgit_core::RequestSlotId::LogSignatures),
    });

    let buf = ts.render(120, 30);
    let rendered = screen_text(&buf);
    assert!(rendered.contains('✓'), "expected ✓ icon in Log:\n{rendered}");
    assert!(rendered.contains('✕'), "expected ✕ icon in Log:\n{rendered}");

    // Hover over row 0's signature cell shows the signer + key tooltip.
    let (cx, cy) = ts
        .hit_center_of(|t| matches!(t, HitTarget::CommitSignatureCell { visible_row: 0, .. }))
        .expect("CommitSignatureCell hit target on row 0");
    ts.send_mouse(MouseEventKind::Moved, cx, cy);
    let hover_text = screen_text(&ts.render(120, 30));
    assert!(hover_text.contains("Commit Signature"), "{hover_text}");
    assert!(hover_text.contains("Alice <alice@x.io>"), "{hover_text}");
    assert!(hover_text.contains("1111AAAA"), "{hover_text}");
}
