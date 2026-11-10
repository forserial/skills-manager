//! Evaluate an update before applying it (sm-69q.10).
//!
//! [`stage_incoming`] builds the tree an update of the skill would install,
//! the way `update_git_skill_with` builds it for a person who asked for that
//! update: the upstream checkout at the remote head, merged with the
//! library's local changes by [`skill_update_merge::prepare`] when it has
//! some, then put through [`installer::install_skill_dir_to_destination`].
//! It calls those same functions rather than a copy of them; what it leaves
//! out is everything the update writes: the `updating` status, the repo lock,
//! the swap, the baseline, the pending-merge record.
//!
//! The staged tree lives under the cache dir, never in the library, and is
//! removed when [`IncomingTree`] drops, whatever the run's outcome. The
//! evaluation itself is [`evaluation::evaluate_with`] with that tree as the
//! target, so the sm-9dn rules hold unchanged: read-only, no `RepoLock`
//! taken at any point, nothing written into a skill directory. The run is
//! stored with subject `incoming`, so it shows in the skill's history but
//! never stands for the library copy in list badges.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde::Serialize;

use crate::commands::skills as skill_cmd;
use crate::core::agent_runner::{RunContext, RunEvent};
use crate::core::error::AppError;
use crate::core::evaluation::{
    self, EvaluateOptions, EvaluationRecord, EvaluationSubject, EvaluationTarget,
};
use crate::core::skill_store::{SkillRecord, SkillStore};
use crate::core::skill_update_merge::{self, MergeConflict};
use crate::core::{central_repo, content_hash, git_fetcher, installer};

/// Subdirectory of the cache dir holding staged incoming trees.
const STAGE_DIR_NAME: &str = "evaluations-incoming";

/// What the update would bring, besides the tree itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct IncomingInfo {
    /// The upstream head an update would move to.
    pub revision: String,
    /// The revision the library copy was installed or last updated at.
    pub installed_revision: Option<String>,
    /// The skill's baseline revision now.
    pub baseline_revision: Option<String>,
    /// The staged tree has exactly the library's content: the update would
    /// change nothing.
    pub same_as_library: bool,
    /// Local changes were merged onto the new version, as a person's update
    /// would merge them.
    pub merged_local_changes: bool,
    /// Changed on both sides and merged automatically.
    pub merged: Vec<String>,
    /// Changed locally only, carried over.
    pub kept_local: Vec<String>,
    /// Files an update would hold for a decision. The staged tree has
    /// upstream's version of each, so the run judged those as upstream ships
    /// them.
    pub conflicts: Vec<MergeConflict>,
}

/// A staged incoming tree, removed when dropped.
pub struct IncomingTree {
    root: PathBuf,
    dir: PathBuf,
    pub info: IncomingInfo,
}

impl IncomingTree {
    /// The staged skill directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The evaluation target for this tree.
    pub fn target(&self) -> Result<EvaluationTarget, AppError> {
        Ok(EvaluationTarget {
            dir: self.dir.clone(),
            subject: EvaluationSubject::Incoming,
            source_revision: Some(self.info.revision.clone()),
            baseline_revision: self.info.baseline_revision.clone(),
            content_hash: Some(content_hash::hash_directory(&self.dir).map_err(AppError::io)?),
        })
    }
}

impl Drop for IncomingTree {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.root) {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!(
                    "Could not remove the staged incoming tree {}: {e}",
                    self.root.display()
                );
            }
        }
    }
}

/// Stage what an update of `skill` would install, under `stage_root`.
/// Reads the store and the library; writes neither.
pub fn stage_incoming(
    store: &SkillStore,
    skill: &SkillRecord,
    stage_root: &Path,
    proxy_url: Option<&str>,
    cancel: Option<&Arc<AtomicBool>>,
) -> Result<IncomingTree, AppError> {
    if !matches!(skill.source_type.as_str(), "git" | "skillssh") {
        return Err(AppError::invalid_input(format!(
            "\"{}\" has no upstream to update from, so there is no incoming version to evaluate.",
            skill.name
        )));
    }
    let source = skill_cmd::git_source_from_skill(skill)?;
    git_fetcher::validate_git_url(&source.clone_url).map_err(AppError::git)?;
    let revision = git_fetcher::resolve_remote_revision(
        &source.clone_url,
        source.branch.as_deref(),
        proxy_url,
    )
    .map_err(|e| AppError::git(e.to_string()))?;

    let checkout = git_fetcher::clone_repo_ref_scoped(
        &source.clone_url,
        source.branch.as_deref(),
        source.subpath.as_deref(),
        cancel,
        proxy_url,
        None,
    )
    .map_err(AppError::classify_git_error)?;
    let staged = (|| -> Result<IncomingTree, AppError> {
        git_fetcher::checkout_revision(&checkout, &revision).map_err(AppError::git)?;
        let upstream_dir = skill_cmd::resolve_skill_dir(
            &checkout,
            source.subpath.as_deref(),
            source.locator_skill_id.as_deref(),
        )?;
        // A person's update merges local changes (MergeMode::WhenLocallyChanged).
        let prepared =
            skill_update_merge::prepare(store, skill, &upstream_dir, &revision, proxy_url)
                .map_err(AppError::internal)?;
        let source_dir = prepared
            .as_ref()
            .map_or(upstream_dir.as_path(), |p| p.merged_dir());

        let root = stage_root.join(format!("incoming-{}", uuid::Uuid::new_v4()));
        let dir_name = Path::new(&skill.central_path)
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| skill.name.clone().into());
        let mut tree = IncomingTree {
            dir: root.join(dir_name),
            root,
            info: IncomingInfo {
                revision: revision.clone(),
                installed_revision: skill.source_revision.clone(),
                baseline_revision: evaluation::current_baseline_revision(&skill.id),
                ..Default::default()
            },
        };
        std::fs::create_dir_all(&tree.root).map_err(AppError::io)?;
        let installed =
            installer::install_skill_dir_to_destination(source_dir, &skill.name, &tree.dir)
                .map_err(AppError::io)?;
        let library_hash = evaluation_library_hash(skill);
        tree.info.same_as_library =
            library_hash.as_deref() == Some(installed.content_hash.as_str());
        if let Some(prepared) = &prepared {
            tree.info.merged_local_changes = true;
            tree.info.merged = prepared.report.merged.clone();
            tree.info.kept_local = prepared.report.kept_local.clone();
            tree.info.conflicts = prepared.report.conflicts.clone();
        }
        Ok(tree)
    })();
    git_fetcher::cleanup_temp(&checkout);
    staged
}

/// The library copy's content hash, read from disk now.
fn evaluation_library_hash(skill: &SkillRecord) -> Option<String> {
    let dir = Path::new(&skill.central_path);
    dir.is_dir()
        .then(|| content_hash::hash_directory(dir).ok())
        .flatten()
        .or_else(|| skill.content_hash.clone())
}

/// An incoming evaluation, with the run it is compared with.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IncomingEvaluation {
    /// The stored run. Its `result` is the v1 contract.
    #[serde(flatten)]
    pub evaluation: EvaluationRecord,
    pub incoming: IncomingInfo,
    /// The newest successful run of the library copy, for comparison.
    pub latest_library: Option<EvaluationRecord>,
}

/// Stage the incoming tree of `skill_id`, evaluate it, remove it.
pub fn evaluate_incoming(
    store: &SkillStore,
    skill_id: &str,
    opts: &EvaluateOptions,
    cancel: &Arc<AtomicBool>,
    on_event: &mut dyn FnMut(RunEvent),
) -> Result<IncomingEvaluation, AppError> {
    let skill = store
        .get_skill_by_id(skill_id)
        .map_err(AppError::db)?
        .ok_or_else(|| AppError::not_found(format!("Skill not found: {skill_id}")))?;
    // Settle the agent and the evaluator before fetching anything.
    let checked = evaluation::resolve_agent(store, opts).and_then(|(resolved, adapter)| {
        evaluation::find_evaluator(store, &resolved.evaluator)?;
        Ok((resolved, adapter))
    });
    let (resolved, adapter) = match checked {
        Ok(checked) => checked,
        Err(err) => {
            evaluation::audit_failure(store, &skill, &err);
            return Err(err);
        }
    };
    let context = RunContext::from_store(store, adapter.key());
    let proxy = store.proxy_url();
    evaluate_incoming_with(
        store,
        &skill,
        &central_repo::cache_dir().join(STAGE_DIR_NAME),
        proxy.as_deref(),
        cancel,
        on_event,
        |target, on_event| {
            evaluation::evaluate_with(
                store,
                &skill,
                Some(target),
                &resolved,
                adapter.as_ref(),
                &context,
                &central_repo::cache_dir().join("evaluations"),
                cancel,
                on_event,
            )
        },
    )
}

/// [`evaluate_incoming`] once the agent is chosen; `run` evaluates the
/// staged target. The tree is gone when this returns, success or not.
pub(crate) fn evaluate_incoming_with(
    store: &SkillStore,
    skill: &SkillRecord,
    stage_root: &Path,
    proxy_url: Option<&str>,
    cancel: &Arc<AtomicBool>,
    on_event: &mut dyn FnMut(RunEvent),
    run: impl FnOnce(EvaluationTarget, &mut dyn FnMut(RunEvent)) -> Result<EvaluationRecord, AppError>,
) -> Result<IncomingEvaluation, AppError> {
    on_event(RunEvent::Progress {
        text: "Preparing the version an update would install".into(),
    });
    let tree = match stage_incoming(store, skill, stage_root, proxy_url, Some(cancel)) {
        Ok(tree) => tree,
        Err(err) => {
            evaluation::audit_failure(store, skill, &err);
            return Err(err);
        }
    };
    let short: String = tree.info.revision.chars().take(12).collect();
    on_event(RunEvent::Progress {
        text: format!("Evaluating the incoming version at {short}"),
    });
    let target = tree.target()?;
    let outcome = run(target, on_event);
    let info = tree.info.clone();
    drop(tree);
    let evaluation = outcome?;
    let latest_library = store
        .list_evaluations(&skill.id)
        .map_err(AppError::db)?
        .into_iter()
        .find(|r| {
            r.subject == EvaluationSubject::Library
                && r.status == evaluation::EvaluationStatus::Succeeded
        });
    Ok(IncomingEvaluation {
        evaluation,
        incoming: info,
        latest_library,
    })
}

#[cfg(test)]
mod tests;
