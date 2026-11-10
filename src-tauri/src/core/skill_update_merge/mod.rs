//! Merge on update (sm-69q.6): carry a skill's local changes onto the new
//! upstream version, file by file against its baseline.
//!
//! Not to be confused with `core::merge`, the library-sync engine.
//!
//! How an update of a locally changed skill goes, for a person who asked
//! for it ([`MergeMode::WhenLocallyChanged`]):
//! 1. Off the repo lock, [`prepare`] stages the new upstream tree through the
//!    installer into a temp directory, plans every path with the table in
//!    design §4 ([`plan`]), gets the base content of the files that need a
//!    three-way merge (the local base store, else a by-SHA fetch, each file
//!    verified against the baseline's hashes), and builds the merged tree in
//!    another temp directory ([`build`]). Choices recorded in a pending-merge
//!    record for exactly these conflicts are applied.
//! 2. Under the lock, the update compares the library with what was read
//!    (anything changed means "run it again"), then either holds, writing the
//!    pending-merge record ([`pending`]) and touching nothing in the library,
//!    or installs the merged tree as the staged tree and swaps it in as
//!    usual. The baseline becomes the new upstream manifest, origin `merge`.
//!
//! What never merges:
//! - unattended callers (the auto-updater, batch update, `update --all`):
//!   they pass [`MergeMode::Never`] and hold as before (sm-69q.4);
//! - a skill whose baseline is unknown or suspect (Rule A, design §11.3):
//!   [`prepare`] returns `None` and the update holds every changed file;
//! - a file whose base content cannot be had: it is a conflict, `base_gone`
//!   when the revision is gone upstream ("not our ref") and
//!   `base_unavailable` otherwise, which a retry may clear. Never a guess.
//!
//! Nothing with conflict markers is written anywhere: libgit2 reports
//! conflicts in an in-memory index, and content supplied by a person is
//! refused while it still has markers.

mod build;
pub mod cleanup;
pub mod pending;
mod plan;

pub use build::{build, merge_text, BaseContent, MergeResult};
pub use pending::{PendingConflict, PendingMerge, Resolution, Take};
pub use plan::{plan, plan_manifests, FileAction, MergePlan, PlanEntry};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::core::audit_log::AuditDraft;
use crate::core::baseline::{
    self, folded_sha256, with_baseline_content, BaseSource, Baseline, LocalState,
};
use crate::core::skill_store::{SkillRecord, SkillStore};
use crate::core::{content_hash, installer};

/// Whether an update may merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMode {
    /// Unattended callers: never merge; a locally changed skill is held.
    Never,
    /// A person asked for this update: merge when the skill has local
    /// changes against a known baseline.
    WhenLocallyChanged,
}

/// Why a file needs a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    /// A text file changed on both sides, and the edits clash.
    Text,
    /// A binary file changed on both sides.
    Binary,
    /// Changed locally, deleted upstream.
    DeletedUpstream,
    /// Deleted locally, changed upstream.
    DeletedLocally,
    /// Added on both sides with different content.
    AddedBoth,
    /// Changed on both sides; the baseline revision is gone upstream, so its
    /// content cannot be had and the file cannot be merged.
    BaseGone,
    /// Changed on both sides; the baseline content could not be fetched now.
    /// Updating again later may merge it.
    BaseUnavailable,
}

impl ConflictKind {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Text => "changed on both sides, and the edits clash",
            Self::Binary => "a binary file changed on both sides",
            Self::DeletedUpstream => "changed locally, deleted upstream",
            Self::DeletedLocally => "deleted locally, changed upstream",
            Self::AddedBoth => "added on both sides with different content",
            Self::BaseGone => {
                "changed on both sides; the baseline revision is gone upstream, so it cannot be merged"
            }
            Self::BaseUnavailable => {
                "changed on both sides; the baseline content could not be fetched now (update again to retry)"
            }
        }
    }
}

/// One file that needs a decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeConflict {
    /// Relative to the skill directory, with `/` separators.
    pub path: String,
    pub kind: ConflictKind,
    /// SHA-256 of the library's bytes, `None` when the library lacks it.
    pub local_sha256: Option<String>,
    /// SHA-256 of upstream's staged bytes, `None` when upstream lacks it.
    pub upstream_sha256: Option<String>,
}

impl MergeConflict {
    /// The same decision: same path, same two copies.
    pub fn same_question(&self, other: &MergeConflict) -> bool {
        self.path == other.path
            && self.local_sha256 == other.local_sha256
            && self.upstream_sha256 == other.upstream_sha256
    }
}

/// What a merge did, in `UpdateSkillResult::merge`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MergeReport {
    pub target_revision: String,
    pub baseline_revision: String,
    /// Changed on both sides and merged automatically.
    pub merged: Vec<String>,
    /// Changed or added locally only, carried over as they are.
    pub kept_local: Vec<String>,
    /// Conflicts settled by a choice recorded with `skills merge resolve`.
    pub resolved: Vec<String>,
    /// Still need a decision. Non-empty means the update was **held** and
    /// nothing in the library changed.
    pub conflicts: Vec<MergeConflict>,
    /// Where the base content came from, when it was needed and found.
    pub base_source: Option<BaseSource>,
    /// Why the base content could not be had, when it could not.
    pub base_message: Option<String>,
}

impl MergeReport {
    pub fn is_held(&self) -> bool {
        !self.conflicts.is_empty()
    }
}

pub(crate) fn raw_sha256(content: &[u8]) -> String {
    hex::encode(Sha256::digest(content))
}

/// Raw SHA-256 of every content file under `dir`.
fn raw_manifest(dir: &Path) -> Result<BTreeMap<String, String>> {
    content_hash::list_content_files_strict(dir)?
        .iter()
        .map(|entry| {
            let bytes = std::fs::read(&entry.path)
                .with_context(|| format!("failed to read {}", entry.path.display()))?;
            Ok((entry.relative_path.clone(), raw_sha256(&bytes)))
        })
        .collect()
}

/// A merged tree ready for the update to stage, or a hold to record. Its
/// temp directories are removed when it is dropped.
pub struct Prepared {
    root: PathBuf,
    upstream: PathBuf,
    merged: PathBuf,
    library: PathBuf,
    library_snapshot: BTreeMap<String, String>,
    base: BaseContent,
    changes_library: bool,
    /// Every conflict, resolved or not: the record keeps all their choices.
    all_conflicts: Vec<MergeConflict>,
    pub report: MergeReport,
}

impl Drop for Prepared {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Prepared {
    /// The merged tree: the source the update stages in place of upstream's.
    pub fn merged_dir(&self) -> &Path {
        &self.merged
    }

    /// Whether applying would change any content file of the library.
    pub fn changes_library(&self) -> bool {
        self.changes_library
    }

    /// Nothing to do at all: the library already is the merge, and the
    /// baseline already is at the target revision.
    pub fn is_noop(&self) -> bool {
        !self.changes_library && self.report.baseline_revision == self.report.target_revision
    }

    pub fn is_held(&self) -> bool {
        self.report.is_held()
    }

    /// The final comparison, under the repo lock: the library still holds
    /// exactly what the merge read.
    pub fn verify_library_unchanged(&self) -> Result<()> {
        if raw_manifest(&self.library)? != self.library_snapshot {
            bail!(
                "the skill's files changed while its update was being merged; nothing was changed, run the update again"
            );
        }
        Ok(())
    }

    /// Record the hold outside the library, with each conflicted file's
    /// versions for "open both versions".
    pub fn save_pending(&self, skill: &SkillRecord) -> Result<PendingMerge> {
        let read = |dir: &Path, path: &str| std::fs::read(dir.join(path)).ok();
        pending::save_hold(
            skill,
            &self.report.target_revision,
            &self.report.baseline_revision,
            &self.report.merged,
            &self.all_conflicts,
            |conflict| {
                Ok(pending::Versions {
                    base: match &self.base {
                        BaseContent::Files(files) => files.get(&conflict.path).cloned(),
                        _ => None,
                    },
                    mine: read(&self.library, &conflict.path),
                    theirs: read(&self.upstream, &conflict.path),
                })
            },
        )
    }
}

/// Off the repo lock: build the merge of `skill`'s library copy with the
/// upstream skill directory `checkout_skill_dir` at `target_revision`.
///
/// `None` when there is nothing to merge: no local changes, or a baseline
/// that is unknown or suspect (Rule A), which is never merged. Errors when
/// a tree cannot be read or written; nothing in the library is touched.
pub fn prepare(
    store: &SkillStore,
    skill: &SkillRecord,
    checkout_skill_dir: &Path,
    target_revision: &str,
    proxy_url: Option<&str>,
) -> Result<Option<Prepared>> {
    let changes = baseline::local_changes(store, skill)?;
    if changes.state != LocalState::Changed {
        return Ok(None);
    }
    let Some(baseline) = baseline::load(&skill.id)? else {
        return Ok(None);
    };

    let root = std::env::temp_dir().join(format!("skills-manager-merge-{}", uuid::Uuid::new_v4()));
    let library = PathBuf::from(&skill.central_path);
    let mut prepared = Prepared {
        upstream: root.join("upstream"),
        merged: root.join("merged"),
        root,
        library_snapshot: BTreeMap::new(),
        base: BaseContent::none_needed(),
        changes_library: false,
        all_conflicts: Vec::new(),
        report: MergeReport {
            target_revision: target_revision.to_string(),
            baseline_revision: baseline.revision.clone(),
            ..Default::default()
        },
        library,
    };
    std::fs::create_dir_all(&prepared.root)?;
    installer::install_skill_dir_to_destination(
        checkout_skill_dir,
        &skill.name,
        &prepared.upstream,
    )?;
    prepared.library_snapshot = raw_manifest(&prepared.library)?;

    let plan = plan::plan(&baseline.files, &prepared.library, &prepared.upstream)?;
    let needed = plan.paths_with(FileAction::Merge);
    if !needed.is_empty() {
        let (base, source, message) = base_content(skill, &baseline, &needed, proxy_url);
        prepared.base = base;
        prepared.report.base_source = source;
        prepared.report.base_message = message;
    }
    let result = build::build(
        &plan,
        &prepared.base,
        &prepared.library,
        &prepared.upstream,
        &prepared.merged,
    )?;
    prepared.report.merged = result.merged;
    prepared.report.kept_local = plan.paths_with(FileAction::KeepLocal);

    let record = pending::load(&skill.id).unwrap_or_else(|err| {
        log::warn!(
            "ignoring the unreadable pending merge of {}: {err:#}",
            skill.id
        );
        None
    });
    prepared.all_conflicts = result.conflicts.clone();
    for conflict in result.conflicts {
        let choice = record
            .as_ref()
            .and_then(|r| r.resolution_for(&baseline.revision, &conflict));
        match choice {
            Some(take) => {
                apply_choice(&skill.id, &conflict.path, take, &prepared)?;
                prepared.report.resolved.push(conflict.path);
            }
            None => prepared.report.conflicts.push(conflict),
        }
    }
    prepared.changes_library = raw_manifest(&prepared.merged)? != prepared.library_snapshot;
    Ok(Some(prepared))
}

/// Put a recorded choice into the merged tree.
fn apply_choice(skill_id: &str, path: &str, take: Take, prepared: &Prepared) -> Result<()> {
    let dest = prepared.merged.join(path);
    let from = |side: &Path| -> Result<()> {
        let src = side.join(path);
        if src.is_file() {
            build::copy_file(&src, &dest)
        } else {
            build::remove_file(&prepared.merged, path)
        }
    };
    match take {
        Take::Local => from(&prepared.library),
        Take::Upstream => from(&prepared.upstream),
        Take::File => build::write_file(&dest, &pending::supplied_content(skill_id, path)?),
    }
}

/// The verified base content of `needed`, or why it could not be had. Only
/// "not our ref" means gone (design §11.3); anything else can be retried.
fn base_content(
    skill: &SkillRecord,
    baseline: &Baseline,
    needed: &[String],
    proxy_url: Option<&str>,
) -> (BaseContent, Option<BaseSource>, Option<String>) {
    let expected: HashMap<&str, &str> = baseline
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.sha256.as_str()))
        .collect();
    let read_verified = |dir: &Path| -> Result<HashMap<String, Vec<u8>>> {
        let mut files = HashMap::with_capacity(needed.len());
        for path in needed {
            let file = dir.join(path);
            let bytes =
                std::fs::read(&file).with_context(|| format!("baseline content lacks {path}"))?;
            if expected.get(path.as_str()) != Some(&folded_sha256(&bytes).as_str()) {
                bail!("baseline content of {path} does not match the baseline's hash");
            }
            files.insert(path.clone(), bytes);
        }
        Ok(files)
    };
    match with_baseline_content(skill, baseline, proxy_url, true, read_verified) {
        Ok((source, files)) => (BaseContent::Files(files), Some(source), None),
        Err(err) => {
            let message = format!("{err:#}");
            log::warn!("base content of {} unavailable: {message}", skill.id);
            if message.contains("revision is gone upstream") {
                (BaseContent::Gone(message.clone()), None, Some(message))
            } else {
                (
                    BaseContent::Unavailable(message.clone()),
                    None,
                    Some(message),
                )
            }
        }
    }
}

/// After an update that merged: one audit entry, and a pending merge that
/// was applied is done with.
pub fn after_update(store: &SkillStore, skill: &SkillRecord, report: &MergeReport, applied: bool) {
    if applied {
        if let Err(err) = pending::remove(&skill.id) {
            log::warn!(
                "could not remove the applied pending merge of {}: {err:#}",
                skill.id
            );
        }
    }
    log_merge(store, skill, report, applied);
}

/// One audit entry per merge, applied or held.
pub fn log_merge(store: &SkillStore, skill: &SkillRecord, report: &MergeReport, applied: bool) {
    let short = &report.target_revision[..report.target_revision.len().min(12)];
    let detail = format!(
        "{}; {} merged, {} kept local, {} resolved, {} conflict(s); target {short}",
        if applied { "applied" } else { "held" },
        report.merged.len(),
        report.kept_local.len(),
        report.resolved.len(),
        report.conflicts.len(),
    );
    store.log_audit(
        AuditDraft::new("merge")
            .skill(skill.id.clone(), skill.name.clone())
            .detail(detail)
            .ok(),
    );
}

// ── `skills merge status / resolve / abort` ──

/// One conflict of a pending merge, as `skills merge status` shows it.
#[derive(Debug, Clone, Serialize)]
pub struct ConflictStatus {
    pub path: String,
    pub kind: ConflictKind,
    pub description: &'static str,
    pub resolution: Option<Take>,
    /// Folder holding `<name>.mine`, `<name>.theirs` and `<name>.base` of
    /// this file: "open both versions".
    pub versions_dir: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MergeStatus {
    pub skill_id: String,
    pub name: String,
    pub pending: bool,
    pub target_revision: Option<String>,
    pub baseline_revision: Option<String>,
    /// The record was made against another baseline than the skill's
    /// current one; its choices no longer apply.
    pub stale: bool,
    pub merged: Vec<String>,
    pub conflicts: Vec<ConflictStatus>,
    /// Conflicts still without a choice.
    pub unresolved: Vec<String>,
}

pub fn status(skill: &SkillRecord) -> Result<MergeStatus> {
    let record = pending::load(&skill.id)?;
    let current = baseline::load(&skill.id).ok().flatten().map(|b| b.revision);
    let mut status = MergeStatus {
        skill_id: skill.id.clone(),
        name: skill.name.clone(),
        pending: record.is_some(),
        target_revision: None,
        baseline_revision: None,
        stale: false,
        merged: Vec::new(),
        conflicts: Vec::new(),
        unresolved: Vec::new(),
    };
    let Some(record) = record else {
        return Ok(status);
    };
    status.stale = current.as_deref() != Some(record.baseline_revision.as_str());
    status.unresolved = record
        .unresolved()
        .iter()
        .map(|c| c.conflict.path.clone())
        .collect();
    status.conflicts = record
        .conflicts
        .iter()
        .map(|c| {
            Ok(ConflictStatus {
                path: c.conflict.path.clone(),
                kind: c.conflict.kind,
                description: c.conflict.kind.describe(),
                resolution: c.resolution.as_ref().map(|r| r.take),
                versions_dir: pending::versions_dir(&skill.id, &c.conflict.path)?
                    .display()
                    .to_string(),
            })
        })
        .collect::<Result<_>>()?;
    status.target_revision = Some(record.target_revision);
    status.baseline_revision = Some(record.baseline_revision);
    status.merged = record.merged;
    Ok(status)
}

/// Record a choice for one conflicted file. `file` supplies the resolved
/// content (with `Take::File`).
pub fn resolve(
    skill: &SkillRecord,
    path: &str,
    take: Take,
    file: Option<&Path>,
) -> Result<MergeStatus> {
    pending::resolve(&skill.id, path, take, file)?;
    status(skill)
}

#[derive(Debug, Clone, Serialize)]
pub struct AbortReport {
    pub skill_id: String,
    pub name: String,
    /// There was a pending merge, and it is gone. The library was never
    /// touched by it.
    pub aborted: bool,
}

pub fn abort(skill: &SkillRecord) -> Result<AbortReport> {
    Ok(AbortReport {
        skill_id: skill.id.clone(),
        name: skill.name.clone(),
        aborted: pending::remove(&skill.id)?,
    })
}

/// `skills merge apply` refuses without a pending merge: there is nothing
/// to apply, and a plain update is `skills update`.
pub fn require_pending(skill: &SkillRecord) -> Result<PendingMerge> {
    pending::load(&skill.id)?.with_context(|| {
        format!(
            "no pending merge for {}: `skills update {}` merges and reports conflicts",
            skill.name, skill.name
        )
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod e2e_tests;
