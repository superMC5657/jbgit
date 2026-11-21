//! UI state for the `[1 Commit]` workbench: `GitStageTree`,
//! `CommitMessageEditor` + Amend/Signoff/Hooks options, and `StageDiffWorkbench`.

mod commit_editor;
mod workbench;

pub use commit_editor::CommitMessageEditor;
pub use workbench::{CommitBanner, RollbackUndoEntry, StageDiffRow, StageWorkbenchState};

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jbgit_core::{DiffContext, StageStatusSnapshot};
    use jbgit_engine::{StageDiffKey, StageDiffSide, ThreeDocStageDiff};

    use super::*;

    #[test]
    fn commit_editor_cjk_wrapping_paste_and_undo_redo() {
        let mut ed = CommitMessageEditor::default();
        ed.insert_paste("feat(中文): 测试多行提交\n\n详细说明");
        assert_eq!(ed.lines.len(), 3);
        assert_eq!(ed.subject_char_count(), 16);
        assert!(!ed.needs_blank_second_line_hint());

        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.insert_char('x');
        assert!(ed.needs_blank_second_line_hint());

        assert!(ed.undo());
        assert!(!ed.needs_blank_second_line_hint());
        assert!(ed.redo());
        assert!(ed.needs_blank_second_line_hint());

        let (wrapped, cursor) = ed.visual_wrapped_lines(10);
        assert!(wrapped.len() >= 4);
        assert!(cursor.1 <= 10);
    }

    #[test]
    fn sync_rows_caches_when_status_and_opts_unchanged() {
        let mut sw = StageWorkbenchState::default();
        let snap = Arc::new(StageStatusSnapshot {
            staged: vec![jbgit_core::StageEntry {
                path: "foo.rs".into(),
                old_path: None,
                kind: jbgit_core::FileStatusKind::Modified,
            }],
            ..Default::default()
        });

        sw.sync_rows(Some(&snap));
        assert_eq!(sw.rows.len(), 2);

        sw.rows.clear();
        sw.sync_rows(Some(&snap));
        assert_eq!(
            sw.rows.len(),
            0,
            "sync_rows should skip rebuild when status pointer and options are identical"
        );

        sw.tree_opts.show_ignored = true;
        sw.sync_rows(Some(&snap));
        assert_eq!(
            sw.rows.len(),
            2,
            "sync_rows must rebuild when options change"
        );

        let snap2 = Arc::new(StageStatusSnapshot {
            unstaged: vec![jbgit_core::StageEntry {
                path: "bar.rs".into(),
                old_path: None,
                kind: jbgit_core::FileStatusKind::Modified,
            }],
            ..Default::default()
        });
        drop(snap);
        sw.rows.clear();
        sw.sync_rows(Some(&snap2));
        assert_eq!(
            sw.rows.len(),
            2,
            "sync_rows must rebuild for a distinct StageStatusSnapshot Arc"
        );
    }

    #[test]
    fn diff_rows_caches_until_diff_or_side_by_side_changes() {
        let mut sw = StageWorkbenchState::default();
        let tracker = jbgit_core::StageLineTracker::new(b"", b"", b"");
        let diff1 = Arc::new(ThreeDocStageDiff {
            key: StageDiffKey {
                path: "foo.rs".into(),
                old_path: None,
                side: StageDiffSide::Staged,
                context: DiffContext::Three,
            },
            vcs_exists: false,
            staged_exists: true,
            local_exists: false,
            vcs_bytes: vec![],
            staged_bytes: vec![],
            local_bytes: vec![],
            file_mode: 0o100644,
            is_binary: false,
            is_conflicted: false,
            tracker,
            hunks: vec![],
            is_truncated: false,
            is_too_large: false,
        });

        sw.current_diff = Some(Arc::clone(&diff1));
        let r1 = sw.diff_rows();
        let r2 = sw.diff_rows();
        assert!(
            Arc::ptr_eq(&r1, &r2),
            "diff_rows must return cached Arc slice on identical diff and mode"
        );

        sw.diff_side_by_side = true;
        let r3 = sw.diff_rows();
        assert!(
            !Arc::ptr_eq(&r1, &r3),
            "diff_rows must invalidate cache when side-by-side toggles"
        );
    }
}
