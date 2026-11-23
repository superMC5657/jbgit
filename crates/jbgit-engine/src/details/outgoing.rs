use std::collections::HashSet;

use git2::Sort;
use jbgit_core::{
    CommitSummary, EpochToken, MAX_OUTGOING_COMMITS, Oid, OutgoingBundle, OutgoingKey,
};

use crate::error::EngineError;
use crate::pool::with_thread_repo;
use crate::probe::{RepoLocation, to_core_oid};

use super::to_git2_oid;

pub(crate) fn load_outgoing_sync(
    loc: &RepoLocation,
    key: &OutgoingKey,
    token: &EpochToken,
) -> Result<OutgoingBundle, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }

    with_thread_repo(&loc.git_dir, |repo| {
        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }

        let source_oid = match key.up_to {
            Some(oid) => to_git2_oid(&oid)?,
            None => {
                let local_ref = format!("refs/heads/{}", key.branch);
                if let Ok(r) = repo.find_reference(&local_ref)
                    && let Ok(c) = r.peel_to_commit()
                {
                    c.id()
                } else if let Ok(obj) = repo.revparse_single(&key.branch)
                    && let Ok(c) = obj.peel_to_commit()
                {
                    c.id()
                } else {
                    return Ok(OutgoingBundle {
                        key: key.clone(),
                        commits: Vec::new(),
                        files_changed: 0,
                        remote_exists: false,
                        truncated: false,
                    });
                }
            }
        };

        let target_branch = if key.remote_branch.is_empty() {
            key.branch.as_str()
        } else {
            key.remote_branch.as_str()
        };
        let remote_ref_name = format!("refs/remotes/{}/{}", key.remote, target_branch);
        let remote_oid = repo
            .find_reference(&remote_ref_name)
            .ok()
            .and_then(|r| r.peel_to_commit().ok())
            .map(|c| c.id());
        let remote_exists = remote_oid.is_some();

        let mut revwalk = repo.revwalk()?;
        revwalk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
        revwalk.push(source_oid)?;
        if let Some(roid) = remote_oid {
            revwalk.hide(roid)?;
        } else if !key.remote.is_empty() {
            let prefix = format!("refs/remotes/{}/", key.remote);
            if let Ok(refs) = repo.references() {
                for r in refs.flatten() {
                    if let Ok(name) = r.name()
                        && name.starts_with(&prefix)
                        && let Ok(c) = r.peel_to_commit()
                    {
                        let _ = revwalk.hide(c.id());
                    }
                }
            }
        }

        let mut commits = Vec::new();
        let mut truncated = false;
        for (idx, oid_res) in revwalk.enumerate() {
            if idx % 128 == 0 && token.is_stale() {
                return Err(EngineError::Cancelled);
            }
            let Ok(raw_oid) = oid_res else {
                continue;
            };
            if commits.len() >= MAX_OUTGOING_COMMITS {
                truncated = true;
                break;
            }
            let Ok(commit) = repo.find_commit(raw_oid) else {
                continue;
            };
            let author = commit.author();
            commits.push(CommitSummary {
                oid: to_core_oid(commit.id()),
                parents: commit.parent_ids().map(to_core_oid).collect(),
                author_name: author.name().unwrap_or("Unknown").to_string(),
                author_email: author.email().unwrap_or("").to_string(),
                timestamp: commit.time().seconds(),
                author_timestamp: author.when().seconds(),
                subject: commit.summary().ok().flatten().unwrap_or("").to_string(),
                body: String::new(),
            });
        }

        let files_changed = if commits.is_empty() {
            0
        } else if let Ok(tip_commit) = repo.find_commit(source_oid)
            && let Ok(tip_tree) = tip_commit.tree()
        {
            let base_tree = if let Some(roid) = remote_oid {
                repo.merge_base(source_oid, roid)
                    .ok()
                    .and_then(|mb| repo.find_commit(mb).ok())
                    .and_then(|c| c.tree().ok())
            } else {
                let outgoing_oids: HashSet<Oid> = commits.iter().map(|c| c.oid).collect();
                commits
                    .last()
                    .and_then(|oldest| {
                        oldest
                            .parents
                            .iter()
                            .find(|p| !outgoing_oids.contains(p))
                            .copied()
                    })
                    .and_then(|base_oid| to_git2_oid(&base_oid).ok())
                    .and_then(|goid| repo.find_commit(goid).ok())
                    .and_then(|c| c.tree().ok())
            };
            repo.diff_tree_to_tree(base_tree.as_ref(), Some(&tip_tree), None)
                .map(|d| d.deltas().len())
                .unwrap_or(0)
        } else {
            0
        };

        Ok(OutgoingBundle {
            key: key.clone(),
            commits,
            files_changed,
            remote_exists,
            truncated,
        })
    })
}
