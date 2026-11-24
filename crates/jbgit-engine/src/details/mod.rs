//! git2 + imara-diff loaders for the Log tab's right-hand panes: commit
//! metadata + changed files (`CommitDetailsLoader`), single-file diffs
//! (`FrameDiffPreview`) and path-filter matches (`VcsLogFileFilter`).

mod blame;
mod cherry_picked;
mod commit_changes;
mod file_diff;
mod file_history;
mod outgoing;
mod patch;
mod path_matches;
mod reflog;
mod remote_tag;
mod tree;

use jbgit_core::{Oid, PersonStamp};

use crate::error::EngineError;

pub(crate) use blame::load_blame_sync;
#[cfg(test)]
pub(crate) use blame::parse_blame_porcelain;
pub use cherry_picked::load_cherry_picked_sync;
pub(crate) use commit_changes::{
    load_commit_details_sync, verify_commit_signature_sync, verify_commit_signatures_batch_sync,
};
pub(crate) use file_diff::load_file_diff_with_oids_sync;
pub(crate) use file_history::load_file_history_sync;
#[cfg(test)]
pub(crate) use file_history::{parse_file_history_z, parse_line_history};
pub(crate) use outgoing::load_outgoing_sync;
pub(crate) use patch::{apply_args, check_patch_sync, resolve_patch_input};
pub(crate) use path_matches::load_path_matches_sync;
pub use reflog::load_reflog_sync;
pub(crate) use remote_tag::check_remote_tag_async;
pub(crate) use tree::{load_tree_blob_sync, load_tree_sync};
#[cfg(test)]
pub(crate) use remote_tag::parse_ls_remote_tag_exists;

pub(super) fn to_git2_oid(oid: &Oid) -> Result<git2::Oid, EngineError> {
    git2::Oid::from_bytes(oid.as_bytes()).map_err(EngineError::from)
}

pub(super) fn person(sig: &git2::Signature<'_>) -> PersonStamp {
    PersonStamp {
        name: sig.name().unwrap_or("Unknown").to_string(),
        email: sig.email().unwrap_or("").to_string(),
        time: sig.when().seconds(),
        offset_minutes: sig.when().offset_minutes(),
    }
}

pub(super) fn delta_path(file: git2::DiffFile<'_>) -> Option<String> {
    file.path().map(|p| p.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use jbgit_core::{FileStatusKind, Oid};

    use super::*;

    fn hex_oid(fill: u8) -> (String, Oid) {
        let oid = Oid::from_bytes([fill; 20]);
        (oid.to_string(), oid)
    }

    #[test]
    fn parses_added_modified_deleted_typechange_and_spaces_cjk() {
        let (h1, o1) = hex_oid(0x11);
        let (h2, o2) = hex_oid(0x22);
        let (h3, o3) = hex_oid(0x33);
        let (h4, o4) = hex_oid(0x44);
        let mut raw = Vec::new();
        raw.extend_from_slice(
            format!(
                "\x1e{h4}\x1fAlice\x1f400\x1fdelete file\x1f\x00\n:100644 000000 1111111 0000000 D\x00dir with space/中文 文件.rs\x000\t5\tdir with space/中文 文件.rs\x00"
            )
            .as_bytes(),
        );
        raw.extend_from_slice(
            format!(
                "\x1e{h3}\x1fBob\x1f300\x1ftypechange\x1f\x00\n:100644 120000 1111111 2222222 T\x00dir with space/中文 文件.rs\x000\t0\tdir with space/中文 文件.rs\x00"
            )
            .as_bytes(),
        );
        raw.extend_from_slice(
            format!(
                "\x1e{h2}\x1fCarol\x1f200\x1fmodify\x1f\x00\n:100644 100644 1111111 2222222 M\x00dir with space/中文 文件.rs\x003\t1\tdir with space/中文 文件.rs\x00"
            )
            .as_bytes(),
        );
        raw.extend_from_slice(
            format!(
                "\x1e{h1}\x1fDave\x1f100\x1fadd\x1f\x00\n:000000 100644 0000000 1111111 A\x00dir with space/中文 文件.rs\x005\t0\tdir with space/中文 文件.rs\x00"
            )
            .as_bytes(),
        );

        let (entries, truncated) = parse_file_history_z(&raw, 50);
        assert!(!truncated);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].oid, o4);
        assert_eq!(entries[0].kind, FileStatusKind::Deleted);
        assert_eq!(entries[0].deletions, 5);
        assert_eq!(entries[0].path_at_commit, "dir with space/中文 文件.rs");

        assert_eq!(entries[1].oid, o3);
        assert_eq!(entries[1].kind, FileStatusKind::Typechange);
        assert_eq!(entries[1].additions, 0);
        assert_eq!(entries[1].deletions, 0);

        assert_eq!(entries[2].oid, o2);
        assert_eq!(entries[2].kind, FileStatusKind::Modified);
        assert_eq!((entries[2].additions, entries[2].deletions), (3, 1));

        assert_eq!(entries[3].oid, o1);
        assert_eq!(entries[3].kind, FileStatusKind::Added);
        assert_eq!((entries[3].additions, entries[3].deletions), (5, 0));
    }

    #[test]
    fn parses_rename_binary_merge_and_truncation() {
        let (h1, o1) = hex_oid(0xaa);
        let (h2, o2) = hex_oid(0xbb);
        let (h3, o3) = hex_oid(0xcc);
        let (h4, _o4) = hex_oid(0xdd);
        let mut raw = Vec::new();
        raw.extend_from_slice(
            format!(
                "\x1e{h1}\x1fAlice\x1f40\x1fbin\x1f\x00\n:100644 100644 111 222 M\x00sub/new name.bin\x00-\t-\tsub/new name.bin\x00"
            )
            .as_bytes(),
        );
        raw.extend_from_slice(
            format!("\x1e{h2}\x1fAlice\x1f30\x1fmerge\x1f\x00").as_bytes(),
        );
        raw.extend_from_slice(
            format!(
                "\x1e{h3}\x1fAlice\x1f20\x1frename\x1f\x00\n:100644 100644 111 222 R092\x00old name.txt\x00sub/new name.bin\x001\t2\t\x00old name.txt\x00sub/new name.bin\x00"
            )
            .as_bytes(),
        );
        raw.extend_from_slice(
            format!(
                "\x1e{h4}\x1fAlice\x1f10\x1finit\x1f\x00\n:000000 100644 000 111 A\x00old name.txt\x0010\t0\told name.txt\x00"
            )
            .as_bytes(),
        );

        let (entries, truncated) = parse_file_history_z(&raw, 3);
        assert!(truncated);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].oid, o1);
        assert!(entries[0].is_binary);
        assert_eq!((entries[0].additions, entries[0].deletions), (0, 0));

        assert_eq!(entries[1].oid, o2);
        assert_eq!(entries[1].path_at_commit, "sub/new name.bin");

        assert_eq!(entries[2].oid, o3);
        assert_eq!(entries[2].kind, FileStatusKind::Renamed);
        assert_eq!(entries[2].old_path.as_deref(), Some("old name.txt"));
        assert_eq!(entries[2].path_at_commit, "sub/new name.bin");
        assert_eq!((entries[2].additions, entries[2].deletions), (1, 2));
    }

    #[test]
    fn parse_blame_porcelain_covers_multihunk_boundary_previous_uncommitted_nonutf8_and_cjk() {
        let (h1, o1) = hex_oid(0x11);
        let (h2, o2) = hex_oid(0x22);
        let (h0, o0) = (Oid::ZERO.to_string(), Oid::ZERO);
        let mut raw = Vec::new();
        raw.extend_from_slice(
            format!(
                "{h1} 1 1 1\nauthor Alice\nauthor-mail <alice@example.com>\nauthor-time 1700000100\nauthor-tz +0800\nsummary initial commit\nboundary\nfilename dir with space/中文 文件.rs\n\tfn main() {{\n"
            )
            .as_bytes(),
        );
        raw.extend_from_slice(
            format!(
                "{h2} 2 2 1\nauthor Bob\nauthor-mail <bob@example.com>\nauthor-time 1700000200\nauthor-tz -0530\nsummary rename and edit\nprevious {h1} old dir/旧 文件.rs\nfilename dir with space/中文 文件.rs\n\t// bad utf8: "
            )
            .as_bytes(),
        );
        raw.extend_from_slice(b"\xff\xfe\n");
        raw.extend_from_slice(
            format!("{h1} 3 3 1\n\tprintln!(\"still h1\");\n").as_bytes(),
        );
        raw.extend_from_slice(
            format!(
                "{h0} 4 4 1\nauthor Not Committed Yet\nauthor-mail <not.committed.yet>\nauthor-time 1700000300\nauthor-tz +0000\nsummary Version of file from working tree\nfilename dir with space/中文 文件.rs\n\t\n"
            )
            .as_bytes(),
        );

        let parsed = parse_blame_porcelain(&raw).unwrap();
        assert!(parsed.uncommitted_oid_present);
        assert_eq!(parsed.lines.len(), 4);
        let bundle = jbgit_core::BlameBundle {
            key: jbgit_core::BlameKey {
                path: "dir with space/中文 文件.rs".into(),
                rev: None,
                options: jbgit_core::BlameOptions::default(),
            },
            lines: parsed.lines.clone(),
            commits: parsed.commits.clone(),
            content: parsed.content.clone(),
            uncommitted_oid_present: parsed.uncommitted_oid_present,
        };
        assert_eq!(bundle.content_lines().len(), 4);
        assert_eq!(bundle.content_lines()[3], "");

        assert_eq!(parsed.lines[0].oid, o1);
        assert_eq!(parsed.lines[0].orig_path, "dir with space/中文 文件.rs");
        assert!(parsed.commits[&o1].is_boundary);
        assert_eq!(parsed.commits[&o1].author.email, "alice@example.com");
        assert_eq!(parsed.commits[&o1].author.offset_minutes, 480);

        assert_eq!(parsed.lines[1].oid, o2);
        assert_eq!(parsed.commits[&o2].author.offset_minutes, -330);
        assert_eq!(
            parsed.commits[&o2].previous.as_ref(),
            Some(&(o1, "old dir/旧 文件.rs".to_string()))
        );
        assert!(bundle.content_lines()[1].starts_with("// bad utf8:"));

        // Second hunk of h1 without repeated headers inherits filename and metadata.
        assert_eq!(parsed.lines[2].oid, o1);
        assert_eq!(parsed.lines[2].orig_path, "dir with space/中文 文件.rs");

        assert_eq!(parsed.lines[3].oid, o0);
        assert_eq!(parsed.commits[&o0].author.name, "Not Committed Yet");
    }

    #[test]
    fn parse_line_history_handles_merge_empty_rename_and_truncation() {
        let (h1, o1) = hex_oid(0xaa);
        let (h2, o2) = hex_oid(0xbb);
        let (h3, o3) = hex_oid(0xcc);
        let (h4, _o4) = hex_oid(0xdd);

        // Empty input yields empty results.
        let (empty, trunc) = parse_line_history(b"", "src/a.rs", 50);
        assert!(empty.is_empty() && !trunc);

        let raw = [
            format!("\x1e{h1}\x1fAlice\x1f300\x1fmerge branches\x1f"),
            "diff --cc dir with space/中文 文件.rs".to_string(),
            "index 111,222..333".to_string(),
            "--- a/dir with space/中文 文件.rs".to_string(),
            "+++ b/dir with space/中文 文件.rs".to_string(),
            "@@@ -1,2 -1,2 +1,3 @@@".to_string(),
            "  fn keep() {".to_string(),
            " +    from_branch();".to_string(),
            " -    removed();".to_string(),
            "  }".to_string(),
            format!("\x1e{h2}\x1fBob\x1f200\x1fmerge no diff\x1f"),
            format!("\x1e{h3}\x1fCarol\x1f100\x1frename file\x1f"),
            "diff --git a/old name.rs b/dir with space/中文 文件.rs".to_string(),
            "similarity index 90%".to_string(),
            "rename from old name.rs".to_string(),
            "rename to dir with space/中文 文件.rs".to_string(),
            "--- a/old name.rs".to_string(),
            "+++ b/dir with space/中文 文件.rs".to_string(),
            "@@ -1,2 +1,2 @@".to_string(),
            "-fn old() {}".to_string(),
            "+fn new() {}".to_string(),
            format!("\x1e{h4}\x1fDave\x1f50\x1finit\x1f"),
            "diff --git a/old name.rs b/old name.rs".to_string(),
            "new file mode 100644".to_string(),
            "--- /dev/null".to_string(),
            "+++ b/old name.rs".to_string(),
            "@@ -0,0 +1,2 @@".to_string(),
            "+fn old() {}".to_string(),
            "".to_string(),
        ]
        .join("\n");

        let (entries, truncated) = parse_line_history(raw.as_bytes(), "dir with space/中文 文件.rs", 3);
        assert!(truncated);
        assert_eq!(entries.len(), 3);

        assert_eq!(entries[0].oid, o1);
        assert_eq!((entries[0].additions, entries[0].deletions), (1, 1));
        assert!(entries[0].range_patch.as_deref().unwrap().contains("@@@"));

        // Merge commit without patch backfills path and has range_patch = None.
        assert_eq!(entries[1].oid, o2);
        assert_eq!(entries[1].path_at_commit, "dir with space/中文 文件.rs");
        assert!(entries[1].range_patch.is_none());

        assert_eq!(entries[2].oid, o3);
        assert_eq!(entries[2].kind, FileStatusKind::Renamed);
        assert_eq!(entries[2].old_path.as_deref(), Some("old name.rs"));
        assert_eq!(entries[2].path_at_commit, "dir with space/中文 文件.rs");
        assert_eq!((entries[2].additions, entries[2].deletions), (1, 1));
    }

    #[test]
    fn parse_ls_remote_tag_exists_matches_lightweight_and_annotated() {
        let out = b"1111111111111111111111111111111111111111\trefs/tags/v1.0\n\
2222222222222222222222222222222222222222\trefs/tags/v2.0\n\
3333333333333333333333333333333333333333\trefs/tags/v2.0^{}\n";
        assert!(parse_ls_remote_tag_exists(out, "v1.0"));
        assert!(parse_ls_remote_tag_exists(out, "v2.0"));
        assert!(!parse_ls_remote_tag_exists(out, "v1.0.0"));
        assert!(!parse_ls_remote_tag_exists(b"", "v1.0"));
    }
}
