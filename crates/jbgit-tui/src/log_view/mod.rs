//! UI state for the `[2 Log]` three-pane workbench that is not graph
//! projection: Branches Dashboard, filter bar, presentation settings and the
//! right-hand changes / details / diff panes.

mod branches_pane;
mod details_pane;
mod file_history;
mod filter_bar;
mod presentation;

pub use branches_pane::BranchesPaneState;
pub(crate) use details_pane::FileDiffRowsCache;
#[cfg(test)]
pub use details_pane::DETAILS_CACHE_CAPACITY;
pub use details_pane::{
    ChangesExpandState, DETAILS_DEBOUNCE, DetailsPaneState, DiffPreviewDock, DiffRow,
};
pub use file_history::{FILE_HISTORY_G_WINDOW, FileHistoryFocus, FileHistoryState};
pub use filter_bar::{
    FilterBarState, FilterField, PathMatches, PathRequest, complete_last_token, filter_suggestions,
};
pub use presentation::{
    LogPresentation, PresentationToggle, format_absolute_time, format_relative_time,
};

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
        d.cache.insert(key.clone(), Arc::clone(&bundle1));
        d.current_key = Some(key);

        let r1 = d.change_rows();
        let r2 = d.change_rows();
        assert!(
            Arc::ptr_eq(&r1, &r2),
            "change_rows must return the same Arc slice on identical bundle"
        );

        d.changes_collapsed.insert("src".into());
        let r3 = d.change_rows();
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
            },
            hunks: vec![],
            is_binary: false,
            is_truncated: false,
            is_too_large: false,
        });
        let dr1 = d.diff_rows_for(&diff, false);
        let dr2 = d.diff_rows_for(&diff, false);
        assert!(
            Arc::ptr_eq(&dr1, &dr2),
            "diff_rows_for must cache rows for same diff and mode"
        );
        let dr3 = d.diff_rows_for(&diff, true);
        assert!(
            !Arc::ptr_eq(&dr1, &dr3),
            "diff_rows_for must invalidate when side_by_side changes"
        );
    }
}
