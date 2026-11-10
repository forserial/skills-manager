//! Local-change detection (sm-69q.3): which files of a library skill differ
//! from its baseline, and the text diff of those files.
//!
//! The per-file folded `sha256` values of the baseline are the truth. The
//! baseline's `content_hash` is only a hint taken over raw bytes on the
//! recording device: when it equals the live content hash there are no local
//! changes and no file is read again (the cheap path, design §3); a mismatch
//! proves nothing and falls through to the per-file comparison.
//!
//! Rule A (design §11.3): while a skill is pending in the library merge, its
//! baseline file may belong to the other side's content, so the skill reads
//! `baseline_suspect` and no files are reported. Callers treat it like
//! `baseline_unknown`.
//!
//! Results are cached in memory keyed by the skill's live content hash (the
//! way evaluation staleness reads `fork_schema::live_content_hash`), reached
//! through the content-hash stamp cache. An edit on disk changes the stamp
//! and so the hash, and shows at the next call without a restart.

use anyhow::{bail, Context, Result};
use git2::{ObjectType, Oid, Repository, TreeWalkMode, TreeWalkResult};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::{
    base_store_has_tree, base_store_path, fetch_revision, is_git_source, load, manifest_of,
    with_staged, Baseline, BaselineFile, FetchError,
};
use crate::commands::skills::{build_source_diff_entries, SkillSourceDiffEntryDto};
use crate::core::content_hash;
use crate::core::git_fetcher;
use crate::core::skill_store::{SkillRecord, SkillStore};

/// Where a skill stands against its baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalState {
    /// No file differs from the baseline, line endings aside.
    Clean,
    /// At least one file was modified, added or deleted.
    Changed,
    /// A git skill with no baseline (not yet backfilled, or forgotten).
    BaselineUnknown,
    /// A git skill pending in the library merge: its baseline may describe
    /// the other side's content (Rule A). Treat like `BaselineUnknown`.
    BaselineSuspect,
    /// Not a git or skills.sh skill: it has no upstream, so no baseline.
    Untracked,
}

/// How one file differs from the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeState {
    Modified,
    /// In the library, not in the baseline.
    Added,
    /// In the baseline, gone from the library.
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalFileChange {
    /// Relative to the skill directory, with `/` separators.
    pub path: String,
    pub state: FileChangeState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalChanges {
    pub skill_id: String,
    pub state: LocalState,
    /// The baseline's revision, when the skill has a readable baseline.
    pub baseline_revision: Option<String>,
    /// Sorted by path. Empty unless `state` is `changed`.
    pub files: Vec<LocalFileChange>,
}

impl LocalChanges {
    fn new(skill_id: &str, state: LocalState, baseline_revision: Option<String>) -> Self {
        Self {
            skill_id: skill_id.to_string(),
            state,
            baseline_revision,
            files: Vec::new(),
        }
    }

    /// Whether the comparison could be made: `clean` or `changed`.
    pub fn baseline_known(&self) -> bool {
        matches!(self.state, LocalState::Clean | LocalState::Changed)
    }

    /// Number of changed files, or `None` when the baseline is not known.
    pub fn change_count(&self) -> Option<usize> {
        self.baseline_known().then_some(self.files.len())
    }

    pub fn is_changed(&self) -> bool {
        self.state == LocalState::Changed
    }
}

/// Per-file differences between a baseline manifest and a library manifest.
pub fn file_changes(baseline: &[BaselineFile], current: &[BaselineFile]) -> Vec<LocalFileChange> {
    let base: BTreeMap<&str, &str> = baseline
        .iter()
        .map(|f| (f.path.as_str(), f.sha256.as_str()))
        .collect();
    let now: BTreeMap<&str, &str> = current
        .iter()
        .map(|f| (f.path.as_str(), f.sha256.as_str()))
        .collect();
    let mut changes: Vec<LocalFileChange> = Vec::new();
    for (path, sha) in &now {
        let state = match base.get(path) {
            None => FileChangeState::Added,
            Some(base_sha) if base_sha != sha => FileChangeState::Modified,
            Some(_) => continue,
        };
        changes.push(LocalFileChange {
            path: path.to_string(),
            state,
        });
    }
    for path in base.keys().filter(|path| !now.contains_key(*path)) {
        changes.push(LocalFileChange {
            path: path.to_string(),
            state: FileChangeState::Deleted,
        });
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    changes
}

// ── detection ──

/// Ids of skills pending in the library merge.
fn pending_ids(store: &SkillStore) -> Result<HashSet<String>> {
    Ok(store
        .list_pending_conflicts()?
        .into_iter()
        .map(|row| row.skill_id)
        .collect())
}

/// Where `skill` stands against its baseline. Errors only when the library
/// copy cannot be read: that is never reported as clean.
pub fn local_changes(store: &SkillStore, skill: &SkillRecord) -> Result<LocalChanges> {
    local_changes_with(skill, &pending_ids(store)?)
}

/// [`local_changes`] by skill id.
pub fn local_changes_by_id(store: &SkillStore, skill_id: &str) -> Result<LocalChanges> {
    let skill = store
        .get_skill_by_id(skill_id)?
        .with_context(|| format!("skill not found: {skill_id}"))?;
    local_changes(store, &skill)
}

/// [`local_changes`] for many skills, reading the pending set once. A skill
/// whose library copy cannot be read is logged and left out of the map;
/// callers read a missing entry as "baseline not known".
pub fn local_changes_for(
    store: &SkillStore,
    skills: &[SkillRecord],
) -> Result<HashMap<String, LocalChanges>> {
    let pending = pending_ids(store)?;
    let mut out = HashMap::with_capacity(skills.len());
    for skill in skills {
        match local_changes_with(skill, &pending) {
            Ok(changes) => {
                out.insert(skill.id.clone(), changes);
            }
            Err(err) => log::warn!("local changes of {} unknown: {err:#}", skill.id),
        }
    }
    Ok(out)
}

fn local_changes_with(skill: &SkillRecord, pending: &HashSet<String>) -> Result<LocalChanges> {
    if !is_git_source(&skill.source_type) {
        return Ok(LocalChanges::new(&skill.id, LocalState::Untracked, None));
    }
    let baseline = match load(&skill.id) {
        Ok(baseline) => baseline,
        Err(err) => {
            log::warn!("unreadable baseline for {}: {err:#}", skill.id);
            None
        }
    };
    let revision = baseline.as_ref().map(|b| b.revision.clone());
    if pending.contains(&skill.id) {
        return Ok(LocalChanges::new(
            &skill.id,
            LocalState::BaselineSuspect,
            revision,
        ));
    }
    match baseline {
        None => Ok(LocalChanges::new(
            &skill.id,
            LocalState::BaselineUnknown,
            None,
        )),
        Some(baseline) => compare(skill, &baseline),
    }
}

struct CacheEntry {
    central_path: String,
    stamp: Option<String>,
    live_hash: String,
    baseline_key: String,
    changes: LocalChanges,
}

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Identifies the baseline a cached result was computed against.
fn baseline_key(baseline: &Baseline) -> String {
    let mut hasher = Sha256::new();
    for part in [
        baseline.skill_id.as_str(),
        baseline.revision.as_str(),
        baseline.content_hash.as_deref().unwrap_or(""),
    ] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    for file in &baseline.files {
        hasher.update((file.path.len() as u64).to_le_bytes());
        hasher.update(file.path.as_bytes());
        hasher.update(file.sha256.as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn compare(skill: &SkillRecord, baseline: &Baseline) -> Result<LocalChanges> {
    let dir = Path::new(&skill.central_path);
    if !dir.is_dir() {
        bail!("library copy of {} is missing: {}", skill.id, dir.display());
    }
    let key = baseline_key(baseline);
    let previous = {
        let cache = cache().lock().unwrap_or_else(|e| e.into_inner());
        cache
            .get(&skill.id)
            .filter(|entry| entry.central_path == skill.central_path)
            .map(|entry| (entry.stamp.clone(), entry.live_hash.clone()))
    };
    let live = content_hash::hash_directory_reusing(
        dir,
        previous
            .as_ref()
            .and_then(|(stamp, hash)| stamp.as_deref().map(|s| (s, hash.as_str()))),
    );
    {
        let cache = cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = cache.get(&skill.id) {
            if entry.central_path == skill.central_path
                && entry.live_hash == live.hash
                && entry.baseline_key == key
            {
                return Ok(entry.changes.clone());
            }
        }
    }

    let revision = Some(baseline.revision.clone());
    let changes = if baseline.content_hash.as_deref() == Some(live.hash.as_str()) {
        LocalChanges::new(&skill.id, LocalState::Clean, revision)
    } else {
        let files = file_changes(&baseline.files, &manifest_of(dir)?);
        let state = if files.is_empty() {
            LocalState::Clean
        } else {
            LocalState::Changed
        };
        LocalChanges {
            files,
            ..LocalChanges::new(&skill.id, state, revision)
        }
    };

    // A reused hash comes back without a stamp; the old one still holds.
    let stamp = live.stamp.or_else(|| {
        previous
            .filter(|(_, hash)| *hash == live.hash)
            .and_then(|(stamp, _)| stamp)
    });
    cache().lock().unwrap_or_else(|e| e.into_inner()).insert(
        skill.id.clone(),
        CacheEntry {
            central_path: skill.central_path.clone(),
            stamp,
            live_hash: live.hash,
            baseline_key: key,
            changes: changes.clone(),
        },
    );
    Ok(changes)
}

// ── diff against the baseline content ──

/// Where the baseline content of a diff came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseSource {
    /// This device's base store, `cache_dir()/baselines.git`.
    BaseStore,
    /// A by-SHA fetch of the baseline revision.
    Fetch,
}

/// `skills diff --local`: the library copy against its baseline.
#[derive(Debug, Serialize)]
pub struct LocalDiff {
    #[serde(flatten)]
    pub changes: LocalChanges,
    pub base_source: Option<BaseSource>,
    /// One entry per changed file, baseline as `original`, library copy as
    /// `updated`. `None` when the baseline content could not be had; the
    /// changed paths are still in `files`.
    pub entries: Option<Vec<SkillSourceDiffEntryDto>>,
    /// Why there is no diff, when there is none.
    pub message: Option<String>,
}

/// The library copy of `skill` against its baseline content: from the local
/// base store, else by fetching the baseline revision. When neither works the
/// changed paths are listed without a diff.
pub fn local_diff(
    store: &SkillStore,
    skill: &SkillRecord,
    proxy_url: Option<&str>,
) -> Result<LocalDiff> {
    local_diff_inner(store, skill, proxy_url, true)
}

/// `validate_url` is false only in tests, which serve upstreams over `file://`.
fn local_diff_inner(
    store: &SkillStore,
    skill: &SkillRecord,
    proxy_url: Option<&str>,
    validate_url: bool,
) -> Result<LocalDiff> {
    let changes = local_changes(store, skill)?;
    let no_diff = |changes: LocalChanges, entries, message: &str| LocalDiff {
        changes,
        base_source: None,
        entries,
        message: Some(message.to_string()),
    };
    match changes.state {
        LocalState::Changed => {}
        LocalState::Clean => {
            return Ok(no_diff(
                changes,
                Some(Vec::new()),
                "no local changes against the baseline",
            ))
        }
        LocalState::BaselineUnknown => {
            return Ok(no_diff(
                changes,
                None,
                "baseline unknown: run `skills baseline rebuild` to record one",
            ))
        }
        LocalState::BaselineSuspect => {
            return Ok(no_diff(
                changes,
                None,
                "baseline suspect: a library sync conflict on this skill is pending; resolve it first",
            ))
        }
        LocalState::Untracked => {
            return Ok(no_diff(
                changes,
                None,
                "not a git or skills.sh skill: there is no baseline to compare with",
            ))
        }
    }
    let baseline = load(&skill.id)?.context("the baseline vanished while diffing")?;
    let wanted: HashSet<String> = changes.files.iter().map(|f| f.path.clone()).collect();
    let library = PathBuf::from(&skill.central_path);
    let diff = |base: &Path| Ok(diff_entries(base, &library, &wanted));

    if let Some(tree_id) = baseline.tree_id.as_deref().filter(|t| base_store_has_tree(t)) {
        match with_exported_tree(tree_id, diff) {
            Ok(entries) => {
                return Ok(LocalDiff {
                    changes,
                    base_source: Some(BaseSource::BaseStore),
                    entries: Some(entries),
                    message: None,
                })
            }
            Err(err) => log::warn!("base store export for {} failed: {err:#}", skill.id),
        }
    }
    match with_fetched_base(skill, &baseline, proxy_url, validate_url, diff) {
        Ok(entries) => Ok(LocalDiff {
            changes,
            base_source: Some(BaseSource::Fetch),
            entries: Some(entries),
            message: None,
        }),
        Err(err) => Ok(LocalDiff {
            changes,
            base_source: None,
            entries: None,
            message: Some(format!(
                "baseline content unavailable, listing changed paths only: {err:#}"
            )),
        }),
    }
}

/// Diff entries for the changed paths only, with CRLF folded in text bodies:
/// a line-ending flip is not a local change, so it must not fill the diff.
fn diff_entries(
    base: &Path,
    library: &Path,
    wanted: &HashSet<String>,
) -> Vec<SkillSourceDiffEntryDto> {
    build_source_diff_entries(base, library)
        .into_iter()
        .filter(|entry| wanted.contains(&entry.relative_path))
        .map(|mut entry| {
            for text in [&mut entry.original_text, &mut entry.updated_text]
                .into_iter()
                .flatten()
            {
                if text.contains("\r\n") {
                    *text = text.replace("\r\n", "\n");
                }
            }
            entry
        })
        .collect()
}

/// Write a base-store tree into a fresh temp directory, hand it to `f`, and
/// remove it afterwards.
fn with_exported_tree<T>(tree_id: &str, f: impl FnOnce(&Path) -> Result<T>) -> Result<T> {
    let repo = Repository::open_bare(base_store_path())?;
    let tree = repo.find_tree(Oid::from_str(tree_id)?)?;
    let mut blobs: Vec<(String, Oid, i32)> = Vec::new();
    tree.walk(TreeWalkMode::PreOrder, |dir, entry| {
        if entry.kind() == Some(ObjectType::Blob) {
            if let Some(name) = entry.name() {
                blobs.push((format!("{dir}{name}"), entry.id(), entry.filemode()));
            }
        }
        TreeWalkResult::Ok
    })?;
    let root = std::env::temp_dir().join(format!(
        "skills-manager-localdiff-{}",
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        for (rel, oid, _mode) in &blobs {
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, repo.find_blob(*oid)?.content())
                .with_context(|| format!("failed to write {}", path.display()))?;
            #[cfg(unix)]
            if *_mode == 0o100755 {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
            }
        }
        std::fs::create_dir_all(&root)?;
        f(&root)
    })();
    let _ = std::fs::remove_dir_all(&root);
    result
}

/// Fetch the baseline revision by SHA, stage the skill through the installer
/// as the baseline was, hand the staged tree to `f`, and clean up.
fn with_fetched_base<T>(
    skill: &SkillRecord,
    baseline: &Baseline,
    proxy_url: Option<&str>,
    validate_url: bool,
    f: impl FnOnce(&Path) -> Result<T>,
) -> Result<T> {
    let source = crate::commands::skills::git_source_from_skill(skill)
        .map_err(|e| anyhow::anyhow!(e.message))?;
    if validate_url {
        git_fetcher::validate_git_url(&source.clone_url)?;
    }
    let checkout = fetch_revision(&source.clone_url, &baseline.revision, proxy_url).map_err(
        |err| match err {
            FetchError::Gone(msg) => anyhow::anyhow!("revision is gone upstream: {msg}"),
            FetchError::Failed(msg) => anyhow::anyhow!("could not fetch now: {msg}"),
        },
    )?;
    let result = (|| {
        let skill_dir = crate::commands::skills::resolve_skill_dir(
            &checkout,
            source.subpath.as_deref(),
            source.locator_skill_id.as_deref(),
        )
        .map_err(|e| anyhow::anyhow!(e.message))?;
        with_staged(&skill_dir, &skill.name, f)
    })();
    let _ = std::fs::remove_dir_all(&checkout);
    result
}

#[cfg(test)]
mod tests;
