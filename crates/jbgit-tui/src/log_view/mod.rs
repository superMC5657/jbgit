//! UI state for the `[2 Log]` three-pane workbench that is not graph
//! projection: Branches Dashboard, filter bar, presentation settings and the
//! right-hand changes / details / diff panes.

mod annotate;
mod branches_pane;
mod details_pane;
mod file_history;
mod filter_bar;
mod presentation;
mod push_dialog;
mod reflog;
mod workbench;

pub use annotate::{
    AnnotateColorMode, AnnotateError, AnnotateFocus, AnnotateNavFrame, AnnotateState,
};
pub use branches_pane::BranchesPaneState;
#[cfg(test)]
pub use details_pane::{ChangesExpandState, DETAILS_CACHE_CAPACITY};
pub use details_pane::{
    DETAILS_DEBOUNCE, DetailsPaneState, DiffPreviewDock, MAX_AGGREGATED_COMMITS,
};
pub use file_history::{FILE_HISTORY_G_WINDOW, FileHistoryFocus, FileHistoryState};
pub use filter_bar::{
    FilterBarState, FilterField, LogSpeedSearchState, PathMatches, PathRequest,
    complete_last_token, filter_suggestions_with_context,
};
pub use presentation::{
    LogPresentation, PresentationToggle, format_absolute_time, format_relative_time,
};
pub use push_dialog::{PushDialogFocus, PushDialogState};
pub use reflog::{ReflogFocus, ReflogState};
pub use workbench::{BranchCompareState, LogDispatchCtx, LogReconcileCtx, LogWorkbench};

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jbgit_core::{DetailsKey, DiffContext};

    use super::*;

    #[test]
    fn relative_time_buckets() {
        let now = 1_000_000;
        assert_eq!(format_relative_time(now - 30, now), "just now");
        assert_eq!(format_relative_time(now - 2 * 3_600, now), "2h ago");
        assert_eq!(format_relative_time(now - 90_000, now), "Yesterday");
        assert_eq!(format_relative_time(now - 3 * 86_400, now), "3d ago");
    }

    #[test]
    fn last_token_completion_keeps_sign() {
        assert_eq!(
            complete_last_token("main -dep", "dependabot/x"),
            "main -dependabot/x "
        );
        assert_eq!(complete_last_token("", "@me"), "@me ");
    }

    #[test]
    fn branches_pane_collapses_on_narrow_terminals() {
        let mut p = BranchesPaneState::default();
        assert!(p.is_collapsed(100));
        assert!(!p.is_collapsed(140));
        p.toggle_collapsed(100);
        assert!(!p.is_collapsed(100));
        p.toggle_collapsed(140);
        assert!(p.is_collapsed(140));
    }

    #[test]
    fn details_pane_change_rows_caches_and_invalidates_on_collapsed_or_bundle() {
        let mut d = DetailsPaneState::default();
        let oid = jbgit_core::Oid::from_bytes([1u8; 20]);
        let key = DetailsKey::single(oid);
        let bundle1 = Arc::new(jbgit_core::CommitChangesBundle {
            key: key.clone(),
            details: vec![],
            changes: vec![jbgit_core::FileChange {
                path: "src/a.rs".into(),
                old_path: None,
                kind: jbgit_core::FileStatusKind::Modified,
                additions: 1,
                deletions: 0,
                is_binary: false,
                exists_in_workdir: true,
                old_oid: None,
                new_oid: None,
            }],
        });
        let mut store = crate::diff_session::DiffCacheStore::new();
        store.insert_details(Arc::clone(&bundle1));
        d.changes.current_key = Some(key);

        let r1 = d.change_rows(&store);
        let r2 = d.change_rows(&store);
        assert!(
            Arc::ptr_eq(&r1, &r2),
            "change_rows must return the same Arc slice on identical bundle"
        );

        d.changes.collapsed.insert("src".into());
        let r3 = d.change_rows(&store);
        assert!(
            !Arc::ptr_eq(&r1, &r3),
            "change_rows must invalidate cache when collapsed set changes"
        );

        let diff = Arc::new(jbgit_core::FileDiff {
            key: jbgit_core::FileDiffKey {
                details: DetailsKey::single(oid),
                path: "src/a.rs".into(),
                old_path: None,
                context: DiffContext::Three,
                whitespace: jbgit_core::WhitespaceMode::Default,
            },
            hunks: vec![],
            is_binary: false,
            binary: None,
            is_truncated: false,
            is_too_large: false,
            lfs: None,
            mode_change: None,
        });
        let dr1 = d.diff.rows_cache.rows_for(&diff, false);
        let dr2 = d.diff.rows_cache.rows_for(&diff, false);
        assert!(
            Arc::ptr_eq(&dr1, &dr2),
            "diff_rows_for must cache rows for same diff and mode"
        );
        let dr3 = d.diff.rows_cache.rows_for(&diff, true);
        assert!(
            !Arc::ptr_eq(&dr1, &dr3),
            "diff_rows_for must invalidate when side_by_side changes"
        );
    }
}
