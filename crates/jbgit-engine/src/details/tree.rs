//! Browse Repository at Revision: lazy per-directory tree listings and
//! size-guarded blob previews read directly through git2.

use std::path::Path;

use git2::{ObjectType, Repository};
use jbgit_core::{
    EpochToken, MAX_DIFF_FILE_BYTES, Oid, TreeBlob, TreeBlobContent, TreeBlobKey, TreeEntry,
    TreeEntryKind, TreeKey, TreeListing, sort_tree_entries,
};

use crate::blob_reader::{CheckedBlob, read_tree_path};
use crate::error::EngineError;
use crate::pool::with_thread_repo;
use crate::probe::RepoLocation;

use super::to_git2_oid;

fn commit_tree<'r>(repo: &'r Repository, oid: &Oid) -> Result<git2::Tree<'r>, EngineError> {
    let commit = repo.find_commit(to_git2_oid(oid)?)?;
    Ok(commit.tree()?)
}

/// Lists one directory (`key.dir`, `""` = root) of the commit's tree.
pub(crate) fn load_tree_sync(
    loc: &RepoLocation,
    key: &TreeKey,
    token: &EpochToken,
) -> Result<TreeListing, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    with_thread_repo(&loc.git_dir, |repo| {
        let root = commit_tree(repo, &key.oid)?;
        let dir = key.dir.trim_matches('/');
        let tree = if dir.is_empty() {
            root
        } else {
            let entry = root.get_path(Path::new(dir))?;
            repo.find_tree(entry.id())?
        };
        let mut entries = Vec::with_capacity(tree.len());
        for (i, e) in tree.iter().enumerate() {
            if i % 512 == 0 && token.is_stale() {
                return Err(EngineError::Cancelled);
            }
            let name = String::from_utf8_lossy(e.name_bytes()).into_owned();
            let mode = e.filemode() as u32;
            let kind = match e.kind() {
                Some(ObjectType::Tree) => TreeEntryKind::Tree,
                Some(ObjectType::Commit) => TreeEntryKind::Submodule,
                _ => TreeEntryKind::Blob,
            };
            let size = (kind == TreeEntryKind::Blob)
                .then(|| {
                    repo.odb()
                        .ok()?
                        .read_header(e.id())
                        .ok()
                        .map(|(s, _)| s as u64)
                })
                .flatten();
            let path = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            entries.push(TreeEntry {
                name,
                path,
                kind,
                mode,
                size,
                oid: Oid::from_bytes(e.id().as_bytes().try_into().unwrap_or([0; 20])),
            });
        }
        sort_tree_entries(&mut entries);
        Ok(TreeListing {
            key: key.clone(),
            entries,
        })
    })
}

/// Reads `key.path` at the commit for the preview pane (4 MiB / binary guards).
pub(crate) fn load_tree_blob_sync(
    loc: &RepoLocation,
    key: &TreeBlobKey,
    token: &EpochToken,
) -> Result<TreeBlob, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    with_thread_repo(&loc.git_dir, |repo| {
        let tree = commit_tree(repo, &key.oid)?;
        let size = tree
            .get_path(Path::new(&key.path))
            .ok()
            .and_then(|e| repo.odb().ok()?.read_header(e.id()).ok())
            .map_or(0, |(s, _)| s as u64);
        let content = match read_tree_path(repo, &tree, &key.path) {
            CheckedBlob::Missing => TreeBlobContent::Missing,
            CheckedBlob::TooLarge { .. } => TreeBlobContent::TooLarge {
                limit: MAX_DIFF_FILE_BYTES,
            },
            CheckedBlob::Loaded {
                is_binary: true, ..
            } => TreeBlobContent::Binary,
            CheckedBlob::Loaded { bytes, .. } => {
                if let Some(ptr) = jbgit_core::parse_lfs_pointer(&bytes) {
                    TreeBlobContent::Lfs(ptr)
                } else {
                    TreeBlobContent::Text(String::from_utf8_lossy(&bytes).into_owned())
                }
            }
        };
        Ok(TreeBlob {
            key: key.clone(),
            size,
            content,
        })
    })
}
