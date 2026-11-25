//! Unified 4 MiB size-guarded, 8 KiB binary-sniffed blob & worktree reader shared
//! between two-document [`crate::details`] diffs and three-document [`crate::stage`] diffs.

use std::fs;
use std::path::Path;

use git2::{AttrCheckFlags, AttrValue, ObjectType, Repository};
use jbgit_core::{
    GIT_MODE_EXECUTABLE, GIT_MODE_REGULAR, GIT_MODE_SYMLINK, MAX_DIFF_FILE_BYTES, Oid,
    is_binary_bytes,
};

use crate::probe::to_core_oid;

const ATTR_FLAGS: AttrCheckFlags = AttrCheckFlags::FILE_THEN_INDEX.union(AttrCheckFlags::NO_SYSTEM);

/// Returns `true` when git attributes configure `filter=lfs` for `path`.
pub(crate) fn path_has_lfs_filter(repo: &Repository, path: &str) -> bool {
    repo.get_attr(Path::new(path), "filter", ATTR_FLAGS)
        .ok()
        .flatten()
        == Some("lfs")
}

/// Returns `true` when staging `bytes` for `path` needs Git's clean-filter / EOL
/// conversion pipeline (`git hash-object -w --stdin --path=<rel>`) rather than raw
/// `index.add_frombuffer`.
pub(crate) fn path_needs_git_clean_filter(repo: &Repository, path: &str, bytes: &[u8]) -> bool {
    let rel = Path::new(path);
    let attr =
        |name: &str| AttrValue::from_string(repo.get_attr(rel, name, ATTR_FLAGS).ok().flatten());

    if matches!(attr("filter"), AttrValue::String(s) if !s.is_empty()) {
        return true;
    }
    if matches!(attr("eol"), AttrValue::String(s) if !s.is_empty()) {
        return true;
    }
    if matches!(attr("ident"), AttrValue::True) {
        return true;
    }
    if matches!(attr("working-tree-encoding"), AttrValue::String(s) if !s.is_empty()) {
        return true;
    }

    let has_crlf = bytes.windows(2).any(|w| w == b"\r\n");
    if !has_crlf {
        return false;
    }

    match attr("text") {
        AttrValue::False => return false,
        AttrValue::True | AttrValue::String(_) | AttrValue::Bytes(_) => return true,
        AttrValue::Unspecified => {}
    }

    repo.config()
        .ok()
        .and_then(|c| c.get_string("core.autocrlf").ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("input"))
}

/// Resolves the effective worktree file mode, respecting `core.filemode = false` so
/// regular files on Windows (or repos with `core.filemode = false`) do not report
/// spurious `100755 → 100644` changes while preserving real typechanges (`120000`).
pub(crate) fn effective_worktree_mode(
    repo: &Repository,
    worktree_mode: Option<u32>,
    base_mode: Option<u32>,
) -> Option<u32> {
    let wt = worktree_mode?;
    if wt != GIT_MODE_REGULAR && wt != GIT_MODE_EXECUTABLE {
        return Some(wt);
    }
    let track_filemode = repo
        .config()
        .ok()
        .and_then(|c| c.get_bool("core.filemode").ok())
        .unwrap_or(cfg!(unix));
    if !track_filemode
        && let Some(base) = base_mode
        && (base == GIT_MODE_REGULAR || base == GIT_MODE_EXECUTABLE)
    {
        return Some(base);
    }
    Some(wt)
}

/// Reads a symbolic link's raw target path bytes (without following the link).
pub(crate) fn read_symlink_target_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    let target = fs::read_link(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(target.as_os_str().as_bytes().to_vec())
    }
    #[cfg(not(unix))]
    {
        Ok(target.to_string_lossy().replace('\\', "/").into_bytes())
    }
}

pub(crate) enum CheckedBlob {
    Missing,
    TooLarge {
        mode: Option<u32>,
        size: Option<u64>,
        oid: Option<Oid>,
    },
    Loaded {
        bytes: Vec<u8>,
        mode: Option<u32>,
        is_binary: bool,
        is_symlink: bool,
        oid: Option<Oid>,
    },
}

impl CheckedBlob {
    pub(crate) fn is_too_large(&self) -> bool {
        matches!(self, Self::TooLarge { .. })
    }

    pub(crate) fn exists(&self) -> bool {
        !matches!(self, Self::Missing)
    }

    pub(crate) fn mode(&self) -> Option<u32> {
        match self {
            Self::Missing => None,
            Self::TooLarge { mode, .. } | Self::Loaded { mode, .. } => *mode,
        }
    }

    pub(crate) fn metadata(&self) -> (Option<u64>, Option<Oid>) {
        match self {
            Self::Missing => (None, None),
            Self::TooLarge { size, oid, .. } => (*size, *oid),
            Self::Loaded { bytes, oid, .. } => (Some(bytes.len() as u64), *oid),
        }
    }

    pub(crate) fn is_symlink(&self) -> bool {
        match self {
            Self::Loaded {
                is_symlink, mode, ..
            } => *is_symlink || *mode == Some(GIT_MODE_SYMLINK),
            Self::TooLarge { mode, .. } => *mode == Some(GIT_MODE_SYMLINK),
            Self::Missing => false,
        }
    }

    pub(crate) fn into_parts(self) -> (Option<Vec<u8>>, Option<u32>, bool) {
        match self {
            Self::Loaded {
                bytes,
                mode,
                is_binary,
                ..
            } => (Some(bytes), mode, is_binary),
            Self::TooLarge { mode, .. } => (None, mode, false),
            Self::Missing => (None, None, false),
        }
    }

    /// Collapses `Missing` into empty non-binary bytes for two-document diff callers,
    /// returning `None` when the payload exceeded [`MAX_DIFF_FILE_BYTES`].
    pub(crate) fn into_bytes_or_too_large(self) -> Option<(Vec<u8>, bool)> {
        match self {
            Self::TooLarge { .. } => None,
            Self::Missing => Some((Vec::new(), false)),
            Self::Loaded {
                bytes, is_binary, ..
            } => Some((bytes, is_binary)),
        }
    }
}

pub(crate) fn read_blob(repo: &Repository, oid: git2::Oid, mode: Option<u32>) -> CheckedBlob {
    if oid.is_zero() {
        return CheckedBlob::Missing;
    }
    let is_symlink = mode == Some(GIT_MODE_SYMLINK);
    let core_oid = Some(to_core_oid(oid));
    if let Ok(odb) = repo.odb()
        && let Ok((size, ObjectType::Blob)) = odb.read_header(oid)
        && size > MAX_DIFF_FILE_BYTES
    {
        return CheckedBlob::TooLarge {
            mode,
            size: Some(size as u64),
            oid: core_oid,
        };
    }
    match repo.find_blob(oid) {
        Ok(blob) => {
            if blob.size() > MAX_DIFF_FILE_BYTES {
                return CheckedBlob::TooLarge {
                    mode,
                    size: Some(blob.size() as u64),
                    oid: core_oid,
                };
            }
            let content = blob.content();
            let is_binary = !is_symlink && is_binary_bytes(content);
            CheckedBlob::Loaded {
                bytes: content.to_vec(),
                mode,
                is_binary,
                is_symlink,
                oid: core_oid,
            }
        }
        Err(_) => CheckedBlob::Missing,
    }
}

pub(crate) fn read_tree_path(
    repo: &Repository,
    tree: &git2::Tree<'_>,
    rel_path: &str,
) -> CheckedBlob {
    let Ok(entry) = tree.get_path(Path::new(rel_path)) else {
        return CheckedBlob::Missing;
    };
    let mode = entry.filemode() as u32;
    if entry.kind() != Some(ObjectType::Blob) {
        return CheckedBlob::Missing;
    }
    read_blob(repo, entry.id(), Some(mode))
}

pub(crate) fn read_head_tree_path(repo: &Repository, rel_path: &str) -> CheckedBlob {
    let Ok(head) = repo.head() else {
        return CheckedBlob::Missing;
    };
    let Ok(commit) = head.peel_to_commit() else {
        return CheckedBlob::Missing;
    };
    let Ok(tree) = commit.tree() else {
        return CheckedBlob::Missing;
    };
    read_tree_path(repo, &tree, rel_path)
}

pub(crate) fn read_index_entry(repo: &Repository, rel_path: &str) -> (CheckedBlob, bool) {
    let Ok(index) = repo.index() else {
        return (CheckedBlob::Missing, false);
    };
    let path_obj = Path::new(rel_path);
    let is_conflicted =
        index.has_conflicts() && (1..=3).any(|stage| index.get_path(path_obj, stage).is_some());

    if let Some(entry) = index.get_path(path_obj, 0) {
        return (read_blob(repo, entry.id, Some(entry.mode)), is_conflicted);
    }
    (CheckedBlob::Missing, is_conflicted)
}

pub(crate) fn read_worktree_path(workdir: &Path, rel_path: &str) -> CheckedBlob {
    let full = workdir.join(rel_path);
    let Ok(meta) = fs::symlink_metadata(&full) else {
        return CheckedBlob::Missing;
    };
    if meta.file_type().is_symlink() {
        let Ok(bytes) = read_symlink_target_bytes(&full) else {
            return CheckedBlob::Missing;
        };
        return CheckedBlob::Loaded {
            bytes,
            mode: Some(GIT_MODE_SYMLINK),
            is_binary: false,
            is_symlink: true,
            oid: None,
        };
    }
    if meta.file_type().is_dir() {
        return CheckedBlob::Missing;
    }

    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(if meta.permissions().mode() & 0o111 != 0 {
            GIT_MODE_EXECUTABLE
        } else {
            GIT_MODE_REGULAR
        })
    };
    #[cfg(not(unix))]
    let mode = Some(GIT_MODE_REGULAR);

    if meta.len() > MAX_DIFF_FILE_BYTES as u64 {
        return CheckedBlob::TooLarge {
            mode,
            size: Some(meta.len()),
            oid: None,
        };
    }
    let Ok(bytes) = fs::read(&full) else {
        return CheckedBlob::Missing;
    };
    if bytes.len() > MAX_DIFF_FILE_BYTES {
        return CheckedBlob::TooLarge {
            mode,
            size: Some(bytes.len() as u64),
            oid: None,
        };
    }
    let is_binary = is_binary_bytes(&bytes);
    let oid = if is_binary {
        git2::Oid::hash_object(ObjectType::Blob, &bytes)
            .ok()
            .map(to_core_oid)
    } else {
        None
    };
    CheckedBlob::Loaded {
        bytes,
        mode,
        is_binary,
        is_symlink: false,
        oid,
    }
}
