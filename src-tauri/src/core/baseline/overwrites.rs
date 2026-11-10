//! Overwrite holds (sm-69q.4): which files an update would replace although
//! they were changed in the library since the baseline.
//!
//! An update stages upstream's new tree and swaps it in. Before this child,
//! the swap replaced every file upstream still ships, edited or not, and only
//! paths that would disappear were asked about (`pending_removals_for`). Now
//! the update also compares the staged tree with the library and the library
//! with its baseline, and holds when a file the user changed would be
//! replaced. Until merging lands (child .6), every row of the per-file table
//! in design §4 that is not "keep" or "take upstream" is a hold.
//!
//! - A file counts when the staged tree has it and its content differs from
//!   the library's (CRLF folded), and the library's copy differs from the
//!   baseline: modified, deleted (the update would bring it back) or added
//!   (upstream added a file of the same name). A local edit that already
//!   matches the new upstream content is not an overwrite.
//! - Files the library has and the staged tree lacks are removals, already
//!   held by `pending_removals_for`, and are not repeated here.
//! - When the baseline is unknown or suspect (Rule A, design §11.3), every
//!   file the staged tree would change is held, with the reason named: not
//!   yet backfilled, unrecoverable (the revision is gone upstream), or
//!   suspect.
//!
//! Nothing here writes anything: it only reads the staged tree, the library
//! and the baseline file.

use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use super::{is_object_id, local_changes, manifest_of, FileChangeState, LocalState};
use crate::core::git_fetcher;
use crate::core::skill_store::{SkillRecord, SkillStore};

/// Why an update holds files it would overwrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverwriteReason {
    /// The baseline is known and these files were changed locally.
    LocalChanges,
    /// No baseline yet. `skills baseline rebuild` can record one, after
    /// which only files that were really changed are held.
    BaselineNotBackfilled,
    /// No baseline, and the revision it would be rebuilt from is gone
    /// upstream ("not our ref"), so a rebuild cannot help. Approving takes
    /// upstream's version once; declining keeps the library's.
    BaselineUnrecoverable,
    /// A library sync conflict on this skill is pending, so its baseline may
    /// describe the other device's content (Rule A). Resolve it first.
    BaselineSuspect,
}

impl OverwriteReason {
    /// One line for logs and the CLI.
    pub fn describe(self) -> &'static str {
        match self {
            Self::LocalChanges => "these files were changed locally since the last update",
            Self::BaselineNotBackfilled => {
                "the baseline is unknown (not yet backfilled): every changed file is held; run `skills baseline rebuild` to narrow it"
            }
            Self::BaselineUnrecoverable => {
                "the baseline is unknown and its revision is gone upstream: every changed file is held, and a rebuild cannot help"
            }
            Self::BaselineSuspect => {
                "the baseline is suspect: a library sync conflict on this skill is pending; resolve it first"
            }
        }
    }
}

/// One library file an update would replace. Shaped like `PendingRemoval`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingOverwrite {
    /// Always `"library"`: the baseline describes the library copy.
    pub location: String,
    /// Relative to the skill directory, with `/` separators.
    pub path: String,
    /// How the library copy differs from the baseline; `None` when the
    /// baseline is unknown or suspect, so it cannot be told.
    pub local: Option<FileChangeState>,
}

/// What an update would overwrite. Empty `files` means nothing is held.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverwriteCheck {
    pub reason: Option<OverwriteReason>,
    pub files: Vec<PendingOverwrite>,
}

impl OverwriteCheck {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Paths the staged tree has whose content differs from the library's,
/// including those the library lacks. Sorted.
fn replaced_paths(staged: &Path, library: &Path) -> Result<Vec<String>> {
    let library: HashMap<String, String> = manifest_of(library)?
        .into_iter()
        .map(|f| (f.path, f.sha256))
        .collect();
    let mut paths: Vec<String> = manifest_of(staged)?
        .into_iter()
        .filter(|f| library.get(&f.path) != Some(&f.sha256))
        .map(|f| f.path)
        .collect();
    paths.sort();
    Ok(paths)
}

/// The files an update of `skill` to the tree at `staged` would overwrite
/// although they changed locally, and why. Errors when the library copy or
/// the staged tree cannot be read; the update must not go ahead then.
pub fn pending_overwrites(
    store: &SkillStore,
    skill: &SkillRecord,
    staged: &Path,
) -> Result<OverwriteCheck> {
    let replaced = replaced_paths(staged, Path::new(&skill.central_path))?;
    if replaced.is_empty() {
        return Ok(OverwriteCheck::default());
    }
    let changes = local_changes(store, skill)?;
    let (reason, files) = match changes.state {
        LocalState::Clean | LocalState::Untracked => return Ok(OverwriteCheck::default()),
        LocalState::Changed => {
            let local: BTreeMap<&str, FileChangeState> = changes
                .files
                .iter()
                .map(|f| (f.path.as_str(), f.state))
                .collect();
            let files: Vec<PendingOverwrite> = replaced
                .iter()
                .filter_map(|path| {
                    local
                        .get(path.as_str())
                        .map(|state| overwrite(path, Some(*state)))
                })
                .collect();
            (OverwriteReason::LocalChanges, files)
        }
        LocalState::BaselineUnknown => (
            OverwriteReason::BaselineNotBackfilled,
            replaced.iter().map(|p| overwrite(p, None)).collect(),
        ),
        LocalState::BaselineSuspect => (
            OverwriteReason::BaselineSuspect,
            replaced.iter().map(|p| overwrite(p, None)).collect(),
        ),
    };
    if files.is_empty() {
        return Ok(OverwriteCheck::default());
    }
    Ok(OverwriteCheck {
        reason: Some(reason),
        files,
    })
}

fn overwrite(path: &str, local: Option<FileChangeState>) -> PendingOverwrite {
    PendingOverwrite {
        location: crate::commands::skills::LIBRARY_LOCATION.to_string(),
        path: path.to_string(),
        local,
    }
}

/// For a hold whose baseline is unknown, tell "not yet backfilled" from
/// "unrecoverable": probe the skill's recorded revision with a blobless,
/// depth-1 by-SHA fetch. Only "not our ref" means gone (design §11.3); any
/// other failure leaves it "not yet backfilled", which can be retried. Run
/// off the repo lock: it is a network round trip.
pub fn refine_unknown_reason(check: &mut OverwriteCheck, skill: &SkillRecord, proxy_url: Option<&str>) {
    if check.reason != Some(OverwriteReason::BaselineNotBackfilled) {
        return;
    }
    let Some(revision) = skill.source_revision.as_deref().filter(|r| is_object_id(r)) else {
        return;
    };
    let Ok(source) = crate::commands::skills::git_source_from_skill(skill) else {
        return;
    };
    if revision_is_gone(&source.clone_url, revision, proxy_url) {
        check.reason = Some(OverwriteReason::BaselineUnrecoverable);
    }
}

/// The log line unattended callers write for a skill held over overwrites.
pub fn log_held_overwrites(skill_id: &str, result: &crate::commands::skills::UpdateSkillResult) {
    if result.pending_overwrites.is_empty() {
        return;
    }
    let reason = result
        .overwrite_reason
        .map(OverwriteReason::describe)
        .unwrap_or("files were changed locally");
    let shown: Vec<&str> = result
        .pending_overwrites
        .iter()
        .take(5)
        .map(|o| o.path.as_str())
        .collect();
    log::info!(
        "holding back {skill_id} — updating would replace {} locally changed file(s) ({}{}): {reason}; update it by hand to review",
        result.pending_overwrites.len(),
        shown.join(", "),
        if result.pending_overwrites.len() > shown.len() { ", …" } else { "" },
    );
}

fn revision_is_gone(url: &str, revision: &str, proxy_url: Option<&str>) -> bool {
    let dir = std::env::temp_dir().join(format!(
        "skills-manager-overwrite-probe-{}",
        uuid::Uuid::new_v4()
    ));
    let gone = (|| {
        git2::Repository::init_bare(&dir).ok()?;
        let result = git_fetcher::run_git_watched_at(
            &dir,
            &[
                "fetch",
                "--no-tags",
                "--depth=1",
                "--filter=blob:none",
                url,
                revision,
            ],
            proxy_url,
            None,
            &None,
        );
        Some(matches!(result, Err(err) if err.to_string().contains("not our ref")))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    gone.unwrap_or(false)
}

#[cfg(test)]
mod tests;
