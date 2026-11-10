//! Baselines (sm-69q.2): what upstream shipped for a git-sourced skill at the
//! revision its library copy came from.
//!
//! A baseline is a revision plus a per-file manifest of the installer's
//! staged output (never the raw checkout: the installer drops `.git` and
//! every symlink). Later children of the epic compare the library copy with
//! it to find local changes (.3), hold updates that would overwrite them
//! (.4), pin (.5) and merge (.6). This module only records and exposes it;
//! it changes no update behaviour.
//!
//! Where it lives (epic design §11, approved with the §11.3 amendments):
//! - `.skills-manager/fork-baseline.<skill_id>.json`, through
//!   [`fork_files`]: flat in the metadata root, so it syncs with the library
//!   and survives reindex, rewrite, backup and object merge.
//! - The staged tree's blobs, in an unsynced bare object store at
//!   `cache_dir()/baselines.git`, one ref per skill. That is the pristine
//!   base content for a later merge and for diffs.
//!
//! What is the truth and what is a hint (§11.3, "device-dependent fields"):
//! the per-file `sha256` values are taken with CRLF folded to LF and are the
//! truth. `tree_id` and `content_hash` are taken over raw staged bytes, which
//! vary with line-ending settings and the Unix executable bit. A
//! `content_hash` match proves "no local changes"; a mismatch proves nothing.
//! A `tree_id` missing from the local base store means "fetch the base".
//!
//! Recording never fails the operation that put content into the library:
//! the content is already there. A baseline that cannot be recorded is
//! forgotten instead, which leaves the skill "baseline unknown". That is
//! always the safe state: later children treat every file of such a skill as
//! possibly changed locally.

use anyhow::{bail, Context, Result};
use git2::{Oid, Repository, Sort};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use super::fork_files::{self, ForkConcern};
use super::merge::protocol::{parse_trailer_ids, TRAILER_CONFLICTS};
use super::merge::resolve::ResolveAction;
use super::merge::snapshot::METADATA_DIR;
use super::repo_lock::RepoLock;
use super::skill_store::{SkillRecord, SkillStore};
use super::{central_repo, content_hash, git_fetcher, installer};

/// How a baseline came to be recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// An install, a reinstall, or a re-point with `set-source`.
    Install,
    /// An update to a new upstream revision.
    Update,
    /// A merge of local changes with upstream (child .6).
    Merge,
    /// Rebuilt after the fact from the skill's recorded source revision.
    Backfill,
    /// An origin written by a newer build. Kept readable, never written.
    #[serde(other)]
    Other,
}

/// One file of the staged tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineFile {
    /// Relative to the skill directory, with `/` separators.
    pub path: String,
    /// SHA-256 of the content with CRLF folded to LF (text files only, see
    /// `content_hash::fold_crlf`). This is the truth about the file.
    pub sha256: String,
}

/// The record stored in `fork-baseline.<skill_id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baseline {
    pub skill_id: String,
    /// The commit the files came from. Its own field: it does not mirror the
    /// skill's `source_revision`, which `check` moves forward (#561).
    pub revision: String,
    pub origin: Origin,
    /// Unix seconds.
    pub recorded_at: i64,
    /// Tree id of the raw staged bytes in this device's base store. A hint.
    #[serde(default)]
    pub tree_id: Option<String>,
    /// `content_hash::hash_entries` of the raw staged bytes. A hint.
    #[serde(default)]
    pub content_hash: Option<String>,
    pub files: Vec<BaselineFile>,
}

/// What `skills status --json` and `skills show` report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BaselineSummary {
    pub revision: String,
    pub origin: Origin,
    pub recorded_at: i64,
    pub file_count: usize,
}

impl Baseline {
    pub fn summary(&self) -> BaselineSummary {
        BaselineSummary {
            revision: self.revision.clone(),
            origin: self.origin,
            recorded_at: self.recorded_at,
            file_count: self.files.len(),
        }
    }
}

// ── manifest ──

/// SHA-256 of a file's content with CRLF folded to LF.
pub fn folded_sha256(content: &[u8]) -> String {
    hex::encode(Sha256::digest(content_hash::fold_crlf(content).as_ref()))
}

/// The per-file manifest of a directory, over the same content scope as the
/// library's content hash (`content_hash::list_content_files`). Enumerated
/// strictly: a directory or file that cannot be read is an error, never a
/// file missing from the manifest.
pub fn manifest_of(dir: &Path) -> Result<Vec<BaselineFile>> {
    content_hash::list_content_files_strict(dir)?
        .iter()
        .map(|entry| {
            let content = std::fs::read(&entry.path)
                .with_context(|| format!("failed to read {}", entry.path.display()))?;
            Ok(BaselineFile {
                path: entry.relative_path.clone(),
                sha256: folded_sha256(&content),
            })
        })
        .collect()
}

/// Paths that differ between a baseline manifest and a directory's manifest:
/// changed, added or removed. Sorted.
pub fn differing_paths(baseline: &[BaselineFile], current: &[BaselineFile]) -> Vec<String> {
    let base: BTreeMap<&str, &str> = baseline
        .iter()
        .map(|f| (f.path.as_str(), f.sha256.as_str()))
        .collect();
    let now: BTreeMap<&str, &str> = current
        .iter()
        .map(|f| (f.path.as_str(), f.sha256.as_str()))
        .collect();
    let mut paths: Vec<String> = base
        .keys()
        .chain(now.keys())
        .filter(|path| base.get(*path) != now.get(*path))
        .map(|path| path.to_string())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// Build a baseline from a staged directory: one read per file feeds the
/// folded manifest and the base store. The base store is best-effort; a
/// failure there leaves `tree_id` empty and is logged.
pub fn build(
    skill_id: &str,
    revision: &str,
    origin: Origin,
    staged_dir: &Path,
) -> Result<Baseline> {
    // Validates the id before anything is named after it.
    fork_files::path_for(ForkConcern::Baseline, skill_id)?;
    let entries = content_hash::list_content_files_strict(staged_dir)?;
    if entries.is_empty() {
        bail!("no content files under {}", staged_dir.display());
    }
    let mut store = match BaseStore::open() {
        Ok(store) => Some(store),
        Err(err) => {
            log::warn!("baseline store unavailable, recording {skill_id} without a tree: {err:#}");
            None
        }
    };
    let mut files = Vec::with_capacity(entries.len());
    let mut tree = TreeNode::default();
    for entry in &entries {
        let content = std::fs::read(&entry.path)
            .with_context(|| format!("failed to read {}", entry.path.display()))?;
        files.push(BaselineFile {
            path: entry.relative_path.clone(),
            sha256: folded_sha256(&content),
        });
        if let Some(base) = &store {
            match base.repo.blob(&content) {
                Ok(oid) => tree.insert(&entry.relative_path, oid, entry.is_executable()),
                Err(err) => {
                    log::warn!("baseline store write failed for {skill_id}: {err}");
                    store = None;
                }
            }
        }
    }
    let tree_id = store.and_then(|base| match base.commit_tree(skill_id, revision, &tree) {
        Ok(oid) => Some(oid.to_string()),
        Err(err) => {
            log::warn!("baseline store write failed for {skill_id}: {err:#}");
            None
        }
    });
    Ok(Baseline {
        skill_id: skill_id.to_string(),
        revision: revision.to_string(),
        origin,
        recorded_at: chrono::Utc::now().timestamp(),
        tree_id,
        content_hash: Some(content_hash::hash_entries(&entries)),
        files,
    })
}

// ── record / load / forget ──

/// Record a baseline from a staged directory. Takes the repo lock only for
/// the file write; the hashing and the base store run outside it.
pub fn record(
    skill_id: &str,
    revision: &str,
    origin: Origin,
    staged_dir: &Path,
) -> Result<Baseline> {
    let baseline = build(skill_id, revision, origin, staged_dir)?;
    fork_files::write(ForkConcern::Baseline, skill_id, &baseline)?;
    Ok(baseline)
}

/// [`record`] for callers that already hold the repo lock.
pub fn record_unlocked(
    skill_id: &str,
    revision: &str,
    origin: Origin,
    staged_dir: &Path,
) -> Result<Baseline> {
    let baseline = build(skill_id, revision, origin, staged_dir)?;
    fork_files::write_unlocked(ForkConcern::Baseline, skill_id, &baseline)?;
    Ok(baseline)
}

/// The skill's baseline, or `None` when it has none ("baseline unknown").
pub fn load(skill_id: &str) -> Result<Option<Baseline>> {
    fork_files::read(ForkConcern::Baseline, skill_id)
}

/// For reports: the baseline's summary, `None` when absent or unreadable.
pub fn summary_for(skill_id: &str) -> Option<BaselineSummary> {
    match load(skill_id) {
        Ok(baseline) => baseline.map(|b| b.summary()),
        Err(err) => {
            log::warn!("unreadable baseline for {skill_id}: {err:#}");
            None
        }
    }
}

/// Drop a skill's baseline: its file and its ref in the base store.
pub fn forget(skill_id: &str) -> Result<()> {
    let _lock = RepoLock::acquire_foreground("forget baseline")?;
    forget_unlocked(skill_id)
}

/// [`forget`] for callers that already hold the repo lock.
pub fn forget_unlocked(skill_id: &str) -> Result<()> {
    fork_files::remove_unlocked(ForkConcern::Baseline, skill_id)?;
    forget_base_content(skill_id);
    Ok(())
}

/// Drop a skill's ref in the local base store. Best-effort: the store is a
/// cache, and a missing ref only means "fetch the base".
pub fn forget_base_content(skill_id: &str) {
    if fork_files::path_for(ForkConcern::Baseline, skill_id).is_err() {
        return;
    }
    let path = base_store_path();
    if !path.join("HEAD").exists() {
        return;
    }
    if let Ok(repo) = Repository::open_bare(&path) {
        if let Ok(mut reference) = repo.find_reference(&base_ref(skill_id)) {
            let _ = reference.delete();
        }
    }
}

/// Record, or forget when recording fails. Never fails its caller.
fn record_or_forget_unlocked(skill_id: &str, revision: &str, origin: Origin, staged_dir: &Path) {
    if let Err(err) = record_unlocked(skill_id, revision, origin, staged_dir) {
        log::warn!("could not record the baseline of {skill_id}; it is now unknown: {err:#}");
        if let Err(err) = forget_unlocked(skill_id) {
            log::warn!("could not forget the stale baseline of {skill_id}: {err:#}");
        }
    }
}

// ── hooks into the commands that put upstream content into the library ──

fn is_git_source(source_type: &str) -> bool {
    matches!(source_type, "git" | "skillssh")
}

/// Hook for `store_installed_skill_unlocked`: the installer has just written
/// `installed_dir`, so it is the staged output. A git or skills.sh install
/// with a known revision records it. Anything else replaced the content with
/// something that is not upstream's at a known revision (a local install, or
/// adopt with `--git-url`, which carries no revision), so any earlier
/// baseline is forgotten. Caller holds the repo lock.
pub fn on_installed_unlocked(
    skill_id: &str,
    source_type: &str,
    revision: Option<&str>,
    installed_dir: &Path,
) {
    match revision.filter(|_| is_git_source(source_type)) {
        Some(revision) => {
            record_or_forget_unlocked(skill_id, revision, Origin::Install, installed_dir)
        }
        None => {
            if let Err(err) = forget_unlocked(skill_id) {
                log::warn!("could not forget the baseline of {skill_id}: {err:#}");
            }
        }
    }
}

/// Hook for update and `set-source`, after the new content was accepted:
/// stage the checkout's skill directory through the installer into a
/// throwaway directory outside the library and record that. Covers the case
/// where nothing was swapped because the content was unchanged, which has no
/// staged tree of its own. Caller holds the repo lock.
pub fn on_upstream_accepted_unlocked(
    skill_id: &str,
    skill_name: &str,
    revision: &str,
    origin: Origin,
    checkout_skill_dir: &Path,
) {
    let result = with_staged(checkout_skill_dir, skill_name, |staged| {
        record_unlocked(skill_id, revision, origin, staged)
    });
    if let Err(err) = result {
        log::warn!("could not record the baseline of {skill_id}; it is now unknown: {err:#}");
        if let Err(err) = forget_unlocked(skill_id) {
            log::warn!("could not forget the stale baseline of {skill_id}: {err:#}");
        }
    }
}

/// Run the installer from `source` into a fresh temp directory, hand the
/// staged tree to `f`, and remove it afterwards.
fn with_staged<T>(source: &Path, name: &str, f: impl FnOnce(&Path) -> Result<T>) -> Result<T> {
    let root =
        std::env::temp_dir().join(format!("skills-manager-baseline-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        std::fs::create_dir_all(&root)?;
        let staged = root.join("skill");
        installer::install_skill_dir_to_destination(source, name, &staged)?;
        f(&staged)
    })();
    let _ = std::fs::remove_dir_all(&root);
    result
}

// ── conflict resolution in the library sync (Rule B, design §11.3) ──

/// After a library-sync conflict on `skill_id` is resolved, make the
/// baseline describe the content that was kept:
/// - use remote: the baseline file of the remote side's commit;
/// - keep local: the baseline the local side had before the merge that
///   declared the conflict (its first parent);
/// - keep both: the local copy as for keep local, and the new copy gets the
///   remote side's baseline under its own id.
///
/// Whenever the right file cannot be found, the baseline is forgotten. Runs
/// before the resolution commit, so the change is part of it. Never fails
/// the resolution. Caller holds the repo lock.
pub fn after_conflict_resolved_unlocked(
    skills_dir: &Path,
    skill_id: &str,
    action: ResolveAction,
    theirs_commit: Option<Oid>,
) {
    if let Err(err) = apply_rule_b(skills_dir, skill_id, action, theirs_commit) {
        log::warn!("baseline of {skill_id} after conflict resolution is unknown: {err:#}");
        if let Err(err) = forget_unlocked(skill_id) {
            log::warn!("could not forget the baseline of {skill_id}: {err:#}");
        }
    }
}

fn apply_rule_b(
    skills_dir: &Path,
    skill_id: &str,
    action: ResolveAction,
    theirs_commit: Option<Oid>,
) -> Result<()> {
    let repo = Repository::open(skills_dir).context("failed to open skills repository")?;
    match action {
        ResolveAction::UseRemote => {
            let theirs = theirs_commit.context("remote side of the conflict is unknown")?;
            restore_from_commit(&repo, skill_id, skill_id, theirs)
        }
        ResolveAction::KeepLocal => restore_local_side(&repo, skill_id),
        ResolveAction::KeepBoth => {
            restore_local_side(&repo, skill_id)?;
            let Some(theirs) = theirs_commit else {
                return Ok(());
            };
            for copy_id in new_skill_ids_since_head(&repo, skills_dir)? {
                if copy_id != skill_id {
                    restore_from_commit(&repo, skill_id, &copy_id, theirs)?;
                }
            }
            Ok(())
        }
    }
}

/// Restore the baseline the local side had before the merge that declared
/// the conflict, or forget it when that merge or that file cannot be found.
fn restore_local_side(repo: &Repository, skill_id: &str) -> Result<()> {
    match declaring_merge_first_parent(repo, skill_id)? {
        Some(parent) => restore_from_commit(repo, skill_id, skill_id, parent),
        None => forget_unlocked(skill_id),
    }
}

/// Write `from_id`'s baseline as committed in `commit` as `to_id`'s baseline
/// on disk; forget `to_id`'s when the commit holds none.
fn restore_from_commit(repo: &Repository, from_id: &str, to_id: &str, commit: Oid) -> Result<()> {
    let tree = repo.find_commit(commit)?.tree()?;
    let Ok(entry) = tree.get_path(Path::new(&baseline_rel_path(from_id)?)) else {
        return forget_unlocked(to_id);
    };
    let blob = repo.find_blob(entry.id())?;
    // Parse as a baseline to refuse garbage, but write the raw JSON value so
    // fields a newer build added survive.
    let parsed: Baseline = serde_json::from_slice(blob.content())
        .context("committed baseline file is not a baseline")?;
    if parsed.skill_id != from_id {
        bail!(
            "committed baseline file names {}, not {from_id}",
            parsed.skill_id
        );
    }
    let mut value: serde_json::Value = serde_json::from_slice(blob.content())?;
    value["skill_id"] = serde_json::Value::String(to_id.to_string());
    fork_files::write_unlocked(ForkConcern::Baseline, to_id, &value)
}

/// The first parent of the newest merge commit reachable from HEAD whose
/// `Skills-Manager-Conflicts:` trailer names `skill_id`.
fn declaring_merge_first_parent(repo: &Repository, skill_id: &str) -> Result<Option<Oid>> {
    let head = repo.head()?.peel_to_commit()?.id();
    let mut walk = repo.revwalk()?;
    walk.push(head)?;
    walk.set_sorting(Sort::TOPOLOGICAL)?;
    for oid in walk {
        let commit = repo.find_commit(oid?)?;
        if commit.parent_count() == 2
            && parse_trailer_ids(commit.message().unwrap_or_default(), TRAILER_CONFLICTS)
                .iter()
                .any(|id| id == skill_id)
        {
            return Ok(Some(commit.parent_id(0)?));
        }
    }
    Ok(None)
}

/// Skill ids whose metadata file exists on disk but not in HEAD. The
/// resolution commits everything dirty before it acts, so right after
/// keep-both these are exactly the copies it just created.
fn new_skill_ids_since_head(repo: &Repository, skills_dir: &Path) -> Result<Vec<String>> {
    let head_tree = repo.head()?.peel_to_tree()?;
    let dir = skills_dir.join(METADATA_DIR).join("skills");
    let mut ids = Vec::new();
    if !dir.is_dir() {
        return Ok(ids);
    }
    for entry in std::fs::read_dir(&dir)? {
        let name = entry?.file_name().to_string_lossy().to_string();
        let Some(id) = name.strip_suffix(".json") else {
            continue;
        };
        let rel = format!("{METADATA_DIR}/skills/{name}");
        if head_tree.get_path(Path::new(&rel)).is_err()
            && fork_files::path_for(ForkConcern::Baseline, id).is_ok()
        {
            ids.push(id.to_string());
        }
    }
    ids.sort();
    Ok(ids)
}

/// `.skills-manager/fork-baseline.<id>.json`, relative to the library root.
fn baseline_rel_path(skill_id: &str) -> Result<String> {
    let path = fork_files::path_for(ForkConcern::Baseline, skill_id)?;
    let name = path
        .file_name()
        .context("baseline path has no file name")?
        .to_string_lossy()
        .to_string();
    Ok(format!("{METADATA_DIR}/{name}"))
}

// ── backfill ──

/// What happened to one skill in `skills baseline rebuild`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RebuildStatus {
    Rebuilt,
    /// Not attempted: not a git skill, no recorded revision, a pending sync
    /// conflict, or (with `--all`) it already has a baseline.
    Skipped,
    /// The server no longer has the revision ("not our ref"). Nothing written.
    Unreachable,
    /// Any other failure: the fetch could not be done now, or staging failed.
    /// Nothing written; retry later.
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct RebuildEntry {
    pub skill_id: String,
    pub name: String,
    pub status: RebuildStatus,
    pub revision: Option<String>,
    pub file_count: Option<usize>,
    /// Library files that differ from the rebuilt baseline. Backfill trusts
    /// the recorded source revision, which can be older than the content
    /// (design §11.3), so a large count is worth a look.
    pub differing_files: Option<usize>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RebuildReport {
    pub ok: bool,
    pub rebuilt: usize,
    pub skipped: usize,
    pub unreachable: usize,
    pub failed: usize,
    pub skills: Vec<RebuildEntry>,
}

/// Rebuild the baselines of `skill_ids`, or with `all` of every git and
/// skills.sh skill that has none yet (`force` rebuilds those too). Network
/// work runs off the repo lock. Per-skill failures are reported, not raised.
pub fn rebuild(
    store: &SkillStore,
    skill_ids: &[String],
    all: bool,
    force: bool,
    proxy_url: Option<&str>,
) -> Result<RebuildReport> {
    let targets: Vec<SkillRecord> = if all {
        store
            .get_all_skills()?
            .into_iter()
            .filter(|s| is_git_source(&s.source_type))
            .collect()
    } else {
        skill_ids
            .iter()
            .map(|id| {
                store
                    .get_skill_by_id(id)?
                    .with_context(|| format!("skill not found: {id}"))
            })
            .collect::<Result<_>>()?
    };
    let pending: HashSet<String> = store
        .list_pending_conflicts()?
        .into_iter()
        .map(|row| row.skill_id)
        .collect();

    let mut skills = Vec::with_capacity(targets.len());
    for skill in &targets {
        let entry = if all && !force && matches!(load(&skill.id), Ok(Some(_))) {
            skipped(skill, "already has a baseline (pass --force to rebuild it)")
        } else if pending.contains(&skill.id) {
            skipped(
                skill,
                "a library sync conflict on this skill is pending; resolve it first",
            )
        } else {
            rebuild_skill(store, skill, proxy_url, true)
        };
        skills.push(entry);
    }
    if all {
        sweep_base_store(store);
    }
    let count = |status| skills.iter().filter(|e| e.status == status).count();
    Ok(RebuildReport {
        ok: true,
        rebuilt: count(RebuildStatus::Rebuilt),
        skipped: count(RebuildStatus::Skipped),
        unreachable: count(RebuildStatus::Unreachable),
        failed: count(RebuildStatus::Failed),
        skills,
    })
}

fn entry(skill: &SkillRecord, status: RebuildStatus, message: Option<String>) -> RebuildEntry {
    RebuildEntry {
        skill_id: skill.id.clone(),
        name: skill.name.clone(),
        status,
        revision: skill.source_revision.clone(),
        file_count: None,
        differing_files: None,
        message,
    }
}

fn skipped(skill: &SkillRecord, why: &str) -> RebuildEntry {
    entry(skill, RebuildStatus::Skipped, Some(why.to_string()))
}

fn is_object_id(rev: &str) -> bool {
    matches!(rev.len(), 40 | 64) && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Rebuild one skill's baseline from its recorded source revision.
/// `validate_url` is false only in tests, which serve upstreams over `file://`.
fn rebuild_skill(
    store: &SkillStore,
    skill: &SkillRecord,
    proxy_url: Option<&str>,
    validate_url: bool,
) -> RebuildEntry {
    if !is_git_source(&skill.source_type) {
        return skipped(skill, "not a git or skills.sh skill");
    }
    let Some(revision) = skill.source_revision.clone() else {
        return skipped(
            skill,
            "no recorded source revision: baseline unknown (after a backup clone, run this on the original device)",
        );
    };
    if !is_object_id(&revision) {
        return skipped(skill, "recorded source revision is not a full commit id");
    }
    let source = match crate::commands::skills::git_source_from_skill(skill) {
        Ok(source) => source,
        Err(err) => return skipped(skill, &err.message),
    };
    if validate_url {
        if let Err(err) = git_fetcher::validate_git_url(&source.clone_url) {
            return skipped(skill, &err.to_string());
        }
    }

    // Off the lock: fetch, check out, stage, hash.
    let checkout = match fetch_revision(&source.clone_url, &revision, proxy_url) {
        Ok(dir) => dir,
        Err(FetchError::Gone(msg)) => {
            return entry(
                skill,
                RebuildStatus::Unreachable,
                Some(format!("revision is gone upstream: {msg}")),
            );
        }
        Err(FetchError::Failed(msg)) => {
            return entry(
                skill,
                RebuildStatus::Failed,
                Some(format!("could not fetch now: {msg}")),
            );
        }
    };
    let built = (|| -> Result<Baseline> {
        let skill_dir = crate::commands::skills::resolve_skill_dir(
            &checkout,
            source.subpath.as_deref(),
            source.locator_skill_id.as_deref(),
        )
        .map_err(|e| anyhow::anyhow!(e.message))?;
        with_staged(&skill_dir, &skill.name, |staged| {
            build(&skill.id, &revision, Origin::Backfill, staged)
        })
    })();
    let _ = std::fs::remove_dir_all(&checkout);
    let baseline = match built {
        Ok(baseline) => baseline,
        Err(err) => return entry(skill, RebuildStatus::Failed, Some(format!("{err:#}"))),
    };

    // Under the lock, briefly: write only if the skill still is what was fetched.
    let written = (|| -> Result<()> {
        let _lock = RepoLock::acquire_foreground("rebuild baseline")?;
        let current = store
            .get_skill_by_id(&skill.id)?
            .context("skill was removed while rebuilding its baseline")?;
        if current.source_revision.as_deref() != Some(revision.as_str()) {
            bail!("skill changed while rebuilding its baseline; run it again");
        }
        fork_files::write_unlocked(ForkConcern::Baseline, &skill.id, &baseline)
    })();
    if let Err(err) = written {
        return entry(skill, RebuildStatus::Failed, Some(format!("{err:#}")));
    }

    let differing = manifest_of(Path::new(&skill.central_path))
        .map(|library| differing_paths(&baseline.files, &library).len())
        .ok();
    RebuildEntry {
        file_count: Some(baseline.files.len()),
        differing_files: differing,
        ..entry(skill, RebuildStatus::Rebuilt, None)
    }
}

#[derive(Debug)]
enum FetchError {
    /// "not our ref": the server does not have the revision any more.
    Gone(String),
    /// Anything else. The revision may still exist.
    Failed(String),
}

/// Fetch exactly `revision` at depth 1 into a fresh repository and check it
/// out. A shallow clone cannot reach an older revision (design §11.1, Q4);
/// a by-SHA fetch can, the way `git_fetcher::unchanged_subtrees` does.
fn fetch_revision(
    url: &str,
    revision: &str,
    proxy_url: Option<&str>,
) -> Result<PathBuf, FetchError> {
    let dir =
        std::env::temp_dir().join(format!("skills-manager-backfill-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        Repository::init(&dir).map_err(|e| FetchError::Failed(e.to_string()))?;
        git_fetcher::run_git_watched_at(
            &dir,
            &["fetch", "--no-tags", "--depth=1", url, revision],
            proxy_url,
            None,
            &None,
        )
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("not our ref") {
                FetchError::Gone(msg)
            } else {
                FetchError::Failed(msg)
            }
        })?;
        git_fetcher::checkout_revision(&dir, revision)
            .map_err(|e| FetchError::Failed(e.to_string()))
    })();
    match result {
        Ok(()) => Ok(dir),
        Err(err) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(err)
        }
    }
}

// ── local base store: cache_dir()/baselines.git ──

fn base_store_path() -> PathBuf {
    central_repo::cache_dir().join("baselines.git")
}

fn base_ref(skill_id: &str) -> String {
    format!("refs/baselines/{skill_id}")
}

struct BaseStore {
    repo: Repository,
}

impl BaseStore {
    fn open() -> Result<Self> {
        let path = base_store_path();
        let repo = if path.join("HEAD").exists() {
            Repository::open_bare(&path)?
        } else {
            std::fs::create_dir_all(&path)?;
            Repository::init_bare(&path)?
        };
        Ok(Self { repo })
    }

    /// Write the tree, wrap it in a commit and point the skill's ref at it.
    /// The commit is what keeps the objects reachable; its signature and time
    /// are fixed, so the same tree and revision give the same commit.
    fn commit_tree(&self, skill_id: &str, revision: &str, tree: &TreeNode) -> Result<Oid> {
        let tree_oid = tree.write(&self.repo)?;
        let tree = self.repo.find_tree(tree_oid)?;
        let sig = git2::Signature::new(
            "skills-manager",
            "skills-manager@local",
            &git2::Time::new(0, 0),
        )?;
        let commit = self.repo.commit(
            None,
            &sig,
            &sig,
            &format!("baseline {skill_id}\n\nrevision {revision}\n"),
            &tree,
            &[],
        )?;
        self.repo
            .reference(&base_ref(skill_id), commit, true, "record baseline")?;
        Ok(tree_oid)
    }
}

/// Drop base-store refs whose skill is gone. Best-effort.
pub fn sweep_base_store(store: &SkillStore) {
    let path = base_store_path();
    if !path.join("HEAD").exists() {
        return;
    }
    let Ok(live) = store.get_all_skills() else {
        return;
    };
    let live: HashSet<String> = live.into_iter().map(|s| s.id).collect();
    let Ok(repo) = Repository::open_bare(&path) else {
        return;
    };
    let Ok(refs) = repo.references_glob("refs/baselines/*") else {
        return;
    };
    let gone: Vec<String> = refs
        .flatten()
        .filter_map(|r| r.name().map(str::to_string))
        .filter(|name| {
            name.strip_prefix("refs/baselines/")
                .is_some_and(|id| !live.contains(id))
        })
        .collect();
    for name in gone {
        if let Ok(mut reference) = repo.find_reference(&name) {
            let _ = reference.delete();
        }
    }
}

/// Whether the local base store holds this tree.
pub fn base_store_has_tree(tree_id: &str) -> bool {
    let path = base_store_path();
    let Ok(oid) = Oid::from_str(tree_id) else {
        return false;
    };
    Repository::open_bare(&path)
        .ok()
        .is_some_and(|repo| repo.find_tree(oid).is_ok())
}

/// A directory being assembled for the base store.
#[derive(Default)]
struct TreeNode {
    files: BTreeMap<String, (Oid, bool)>,
    dirs: BTreeMap<String, TreeNode>,
}

impl TreeNode {
    fn insert(&mut self, rel_path: &str, oid: Oid, executable: bool) {
        let mut node = self;
        let mut parts: Vec<&str> = rel_path.split('/').filter(|p| !p.is_empty()).collect();
        let Some(file) = parts.pop() else {
            return;
        };
        for part in parts {
            node = node.dirs.entry(part.to_string()).or_default();
        }
        node.files.insert(file.to_string(), (oid, executable));
    }

    fn write(&self, repo: &Repository) -> Result<Oid> {
        let mut builder = repo.treebuilder(None)?;
        for (name, (oid, executable)) in &self.files {
            let mode = if *executable { 0o100755 } else { 0o100644 };
            builder.insert(name, *oid, mode)?;
        }
        for (name, dir) in &self.dirs {
            builder.insert(name, dir.write(repo)?, 0o040000)?;
        }
        Ok(builder.write()?)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod sync_tests;
