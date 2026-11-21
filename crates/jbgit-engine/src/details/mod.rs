//! git2 + imara-diff loaders for the Log tab's right-hand panes: commit
//! metadata + changed files (`CommitDetailsLoader`), single-file diffs
//! (`FrameDiffPreview`) and path-filter matches (`VcsLogFileFilter`).

mod commit_changes;
mod file_diff;
mod file_history;
mod path_matches;

use std::path::Path;
use std::process::{Command, Stdio};

use jbgit_core::{Oid, PersonStamp};

use crate::error::EngineError;

pub use commit_changes::{load_commit_details_sync, verify_commit_signature_sync};
pub use file_diff::{load_file_diff_sync, load_file_diff_with_oids_sync};
pub use file_history::{DEFAULT_FILE_HISTORY_LIMIT, load_file_history_sync, parse_file_history_z};
pub use path_matches::{load_path_matches_cli, load_path_matches_git2, load_path_matches_sync};

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

pub(super) fn non_interactive_git_command(git: &Path, cwd: Option<&Path>) -> Command {
    let mut cmd = Command::new(git);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C.UTF-8")
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
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
}
