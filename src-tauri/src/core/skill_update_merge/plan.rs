//! The per-file plan: design §4, one row per path.
//!
//! "Upstream" compares the baseline with the new upstream tree, "local"
//! compares the baseline with the library copy. Both comparisons use the
//! baseline's per-file folded `sha256` values, which are the truth (§11.3):
//! a line-ending flip is no change on either side.
//!
//! The plan decides every path (§11.3, decision 5). Only a text file changed
//! differently on both sides is three-way merged; every other file is copied
//! byte for byte from the side the plan names, and a renamed file is two
//! unrelated paths, never a rename.

use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::ConflictKind;
use crate::core::baseline::{manifest_of, BaselineFile};

/// What happens to one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    /// Unchanged on both sides: the library's bytes.
    Keep,
    /// Changed or added upstream only: upstream's bytes.
    TakeUpstream,
    /// Changed or added locally only: the library's bytes.
    KeepLocal,
    /// Changed on both sides to the same content: the library's bytes (the
    /// content is upstream's, so it is no local change against the new
    /// baseline).
    TakeEither,
    /// A text file changed differently on both sides: three-way merge.
    Merge,
    /// Deleted upstream, unchanged locally: left out. The update lists it
    /// for approval as a removal, as it always has.
    Delete,
    /// Deleted locally, unchanged upstream: stays deleted.
    StaysDeleted,
    /// Deleted on both sides.
    BothDeleted,
    /// A person has to decide.
    Hold(ConflictKind),
}

impl FileAction {
    /// The merged tree leaves this path out.
    pub fn is_absent(self) -> bool {
        matches!(self, Self::Delete | Self::StaysDeleted | Self::BothDeleted)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    /// Relative to the skill directory, with `/` separators.
    pub path: String,
    pub action: FileAction,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergePlan {
    /// Sorted by path, one entry per path in the baseline, the library or
    /// upstream.
    pub entries: Vec<PlanEntry>,
}

impl MergePlan {
    pub fn paths_with(&self, action: FileAction) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| e.action == action)
            .map(|e| e.path.clone())
            .collect()
    }

    pub fn action_for(&self, path: &str) -> Option<FileAction> {
        self.entries
            .iter()
            .find(|e| e.path == path)
            .map(|e| e.action)
    }
}

/// The rule `content_hash::fold_crlf` folds by: NUL-free UTF-8. Everything
/// else is binary, so it is never folded and never merged.
pub(crate) fn is_text(content: &[u8]) -> bool {
    !content.contains(&0) && std::str::from_utf8(content).is_ok()
}

/// Plan an update of the library copy at `local` to the staged upstream tree
/// at `upstream`, against the baseline manifest `base`.
pub fn plan(base: &[BaselineFile], local: &Path, upstream: &Path) -> Result<MergePlan> {
    let local_files = manifest_of(local)?;
    let upstream_files = manifest_of(upstream)?;
    plan_manifests(base, &local_files, &upstream_files, |path| {
        let read = |dir: &Path| {
            std::fs::read(dir.join(path))
                .with_context(|| format!("failed to read {}", dir.join(path).display()))
        };
        Ok(is_text(&read(local)?) && is_text(&read(upstream)?))
    })
}

/// [`plan`] over manifests. `both_text(path)` says whether the library's and
/// upstream's copies of a path changed on both sides are text.
pub fn plan_manifests(
    base: &[BaselineFile],
    local: &[BaselineFile],
    upstream: &[BaselineFile],
    both_text: impl Fn(&str) -> Result<bool>,
) -> Result<MergePlan> {
    let index = |files: &[BaselineFile]| -> BTreeMap<String, String> {
        files
            .iter()
            .map(|f| (f.path.clone(), f.sha256.clone()))
            .collect()
    };
    let (base, local, upstream) = (index(base), index(local), index(upstream));
    let paths: BTreeSet<&String> = base
        .keys()
        .chain(local.keys())
        .chain(upstream.keys())
        .collect();

    let mut entries = Vec::with_capacity(paths.len());
    for path in paths {
        let action = match (base.get(path), local.get(path), upstream.get(path)) {
            (Some(b), Some(l), Some(u)) => match (l == b, u == b) {
                (true, true) => FileAction::Keep,
                (true, false) => FileAction::TakeUpstream,
                (false, true) => FileAction::KeepLocal,
                (false, false) if l == u => FileAction::TakeEither,
                (false, false) if both_text(path)? => FileAction::Merge,
                (false, false) => FileAction::Hold(ConflictKind::Binary),
            },
            // Deleted upstream.
            (Some(b), Some(l), None) if l == b => FileAction::Delete,
            (Some(_), Some(_), None) => FileAction::Hold(ConflictKind::DeletedUpstream),
            // Deleted locally.
            (Some(b), None, Some(u)) if u == b => FileAction::StaysDeleted,
            (Some(_), None, Some(_)) => FileAction::Hold(ConflictKind::DeletedLocally),
            (Some(_), None, None) => FileAction::BothDeleted,
            // Not in the baseline.
            (None, None, Some(_)) => FileAction::TakeUpstream,
            (None, Some(_), None) => FileAction::KeepLocal,
            (None, Some(l), Some(u)) if l == u => FileAction::TakeEither,
            (None, Some(_), Some(_)) => FileAction::Hold(ConflictKind::AddedBoth),
            (None, None, None) => continue,
        };
        entries.push(PlanEntry {
            path: path.clone(),
            action,
        });
    }
    Ok(MergePlan { entries })
}
