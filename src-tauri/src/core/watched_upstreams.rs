//! Watched upstreams and fork: the link a skill keeps to places it borrowed from.
//!
//! Some skills will never merge with upstream again: a rewrite into another
//! language, or the owner's own skill that lifted one method from someone
//! else's. They can still watch one or more git locations and be told when
//! those move. A watched upstream is informational only: it is checked for
//! movement and never merged, and nothing is ever fetched into the library.
//! It is independent of the skill's install source and works for every
//! source type.
//!
//! Storage is the fork-file store (`fork_files`, concern `Watch`), so the list
//! lives in `.skills-manager/fork-watch.<skill_id>.json`, syncs with the
//! library (newest commit wins, epic design §11.3), and is removed with the
//! skill and by the orphan sweep. Nothing here writes into a skill directory
//! except [`fork_skill`], which writes only the new skill's own directory.
//!
//! Remote revisions are resolved before the repo lock is taken; the lock is
//! held only for the read-modify-write of the watch file.
//!
//! `remote_revision`, `moved` and `check_error` are written only when a check
//! changes one of them (with `checked_at` alongside), so a routine check of an
//! unmoved upstream doesn't produce a backup commit on every run.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::central_repo;
use super::error::AppError;
use super::fork_files::{self, ForkConcern};
use super::git_fetcher;
use super::installer;
use super::repo_lock::RepoLock;
use super::skill_metadata;
use super::skill_store::SkillStore;
use crate::commands::skills as skill_cmd;

const WATCH_FILE_VERSION: u32 = 1;

/// One git location a skill borrowed from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchedUpstream {
    /// Short id, unique within the skill.
    pub id: String,
    /// Clone URL (or a local repository path / `file://` URL).
    pub git_url: String,
    #[serde(default)]
    pub subpath: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    /// The revision the owner last looked at. `None` when it was never known
    /// (a fork of a skill whose revision was unknown on this device).
    #[serde(default)]
    pub last_seen_revision: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// Unix milliseconds.
    pub added_at: i64,
    /// Head revision found by the last check that changed something.
    #[serde(default)]
    pub remote_revision: Option<String>,
    /// Whether that check found the upstream moved since `last_seen_revision`.
    #[serde(default)]
    pub moved: bool,
    /// Unix milliseconds of the last check that changed the stored result.
    #[serde(default)]
    pub checked_at: Option<i64>,
    /// Why the last check could not resolve the remote.
    #[serde(default)]
    pub check_error: Option<String>,
    /// Fields written by a newer build, kept through a rewrite.
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct WatchFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    upstreams: Vec<WatchedUpstream>,
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

/// What `watch add` asks for.
#[derive(Debug, Clone, Default)]
pub struct NewWatch {
    pub git_url: String,
    pub subpath: Option<String>,
    pub branch: Option<String>,
    /// The revision to treat as seen. When absent, the remote's current head.
    pub revision: Option<String>,
    pub note: Option<String>,
}

/// The result of `skills fork`.
#[derive(Debug, Clone, Serialize)]
pub struct ForkResult {
    pub skill_id: String,
    pub name: String,
    pub central_path: String,
    pub forked_from: String,
    /// The original's source, now watched. `None` when the original has no git
    /// source to watch (a local, imported or website skill).
    pub watched: Option<WatchedUpstream>,
}

// ── Reading ───────────────────────────────────────────────────────────────

/// A skill's watched upstreams, as stored. No network.
pub fn list(skill_id: &str) -> Result<Vec<WatchedUpstream>, AppError> {
    Ok(read_file(skill_id)?.upstreams)
}

/// Whether any of `watches` moved since it was last seen.
pub fn any_moved(watches: &[WatchedUpstream]) -> bool {
    watches.iter().any(|w| w.moved)
}

fn read_file(skill_id: &str) -> Result<WatchFile, AppError> {
    Ok(fork_files::read::<WatchFile>(ForkConcern::Watch, skill_id)
        .map_err(AppError::io)?
        .unwrap_or_default())
}

/// Read, change and write a skill's watch file under the repo lock. An empty
/// list removes the file.
fn modify<T>(
    skill_id: &str,
    operation: &str,
    f: impl FnOnce(&mut Vec<WatchedUpstream>) -> Result<(T, bool), AppError>,
) -> Result<T, AppError> {
    let _lock = RepoLock::acquire_foreground(operation).map_err(AppError::db)?;
    modify_unlocked(skill_id, f)
}

/// [`modify`] for a caller that already holds the repo lock.
fn modify_unlocked<T>(
    skill_id: &str,
    f: impl FnOnce(&mut Vec<WatchedUpstream>) -> Result<(T, bool), AppError>,
) -> Result<T, AppError> {
    let mut file = read_file(skill_id)?;
    let (out, changed) = f(&mut file.upstreams)?;
    if changed {
        if file.upstreams.is_empty() {
            fork_files::remove_unlocked(ForkConcern::Watch, skill_id).map_err(AppError::io)?;
        } else {
            file.version = WATCH_FILE_VERSION;
            fork_files::write_unlocked(ForkConcern::Watch, skill_id, &file)
                .map_err(AppError::io)?;
        }
    }
    Ok(out)
}

fn require_skill(store: &SkillStore, skill_id: &str) -> Result<(), AppError> {
    store
        .get_skill_by_id(skill_id)
        .map_err(AppError::db)?
        .map(|_| ())
        .ok_or_else(|| AppError::not_found("Skill not found"))
}

// ── add / remove / seen ───────────────────────────────────────────────────

/// Add a watched upstream. Without `revision`, the remote's current head is
/// resolved (off the lock) and recorded as seen: "watch from now on".
pub fn add(
    store: &SkillStore,
    skill_id: &str,
    input: NewWatch,
    proxy_url: Option<&str>,
) -> Result<WatchedUpstream, AppError> {
    require_skill(store, skill_id)?;
    let (git_url, branch, subpath) =
        normalize_location(&input.git_url, input.branch, input.subpath)?;
    let revision = match clean(input.revision) {
        Some(revision) => {
            validate_revision(&revision)?;
            Some(revision)
        }
        None => Some(
            git_fetcher::resolve_remote_revision(&git_url, branch.as_deref(), proxy_url).map_err(
                |err| {
                    AppError::network(format!(
                        "Could not resolve {git_url}: {err}. Pass --revision to add it without a lookup."
                    ))
                },
            )?,
        ),
    };
    let watch = WatchedUpstream {
        id: String::new(),
        git_url,
        subpath,
        branch,
        last_seen_revision: revision,
        note: clean(input.note),
        added_at: chrono::Utc::now().timestamp_millis(),
        remote_revision: None,
        moved: false,
        checked_at: None,
        check_error: None,
        extra: Map::new(),
    };
    add_unresolved(skill_id, watch)
}

/// Store `watch` with a fresh id, refusing a duplicate location.
fn add_unresolved(skill_id: &str, mut watch: WatchedUpstream) -> Result<WatchedUpstream, AppError> {
    modify(skill_id, "add watched upstream", |list| {
        if list.iter().any(|w| same_location(w, &watch)) {
            return Err(AppError::invalid_input(format!(
                "This skill already watches {}",
                describe_location(&watch)
            )));
        }
        watch.id = new_id(list);
        list.push(watch.clone());
        Ok((watch, true))
    })
}

/// Remove a watched upstream by id.
pub fn remove(
    store: &SkillStore,
    skill_id: &str,
    watch_id: &str,
) -> Result<WatchedUpstream, AppError> {
    require_skill(store, skill_id)?;
    modify(skill_id, "remove watched upstream", |list| {
        let index = find_index(list, watch_id)?;
        Ok((list.remove(index), true))
    })
}

/// Mark a watched upstream as seen: its current remote revision becomes the
/// last seen one, so it no longer reads as moved. The remote is resolved live;
/// if that fails, the revision found by the last check is used.
pub fn mark_seen(
    store: &SkillStore,
    skill_id: &str,
    watch_id: &str,
    proxy_url: Option<&str>,
) -> Result<WatchedUpstream, AppError> {
    require_skill(store, skill_id)?;
    let current = list(skill_id)?;
    let watch = current[find_index(&current, watch_id)?].clone();
    let live =
        git_fetcher::resolve_remote_revision(&watch.git_url, watch.branch.as_deref(), proxy_url);
    let (revision, error) = match live {
        Ok(revision) => (revision, None),
        Err(err) => match watch.remote_revision.clone() {
            Some(stored) => (stored, Some(err.to_string())),
            None => {
                return Err(AppError::network(format!(
                    "Could not resolve {}: {err}",
                    watch.git_url
                )));
            }
        },
    };
    let now = chrono::Utc::now().timestamp_millis();
    modify(skill_id, "mark watched upstream seen", |list| {
        let index = find_index(list, watch_id)?;
        let entry = &mut list[index];
        entry.last_seen_revision = Some(revision.clone());
        entry.remote_revision = Some(revision);
        entry.moved = false;
        if error.is_none() {
            entry.check_error = None;
            entry.checked_at = Some(now);
        }
        Ok((entry.clone(), true))
    })
}

// ── check ─────────────────────────────────────────────────────────────────

/// Resolve every watched upstream of a skill and store what moved. Resolution
/// runs off the repo lock; nothing is fetched into the library. A failure to
/// resolve one upstream is stored on that entry, not returned.
pub fn check(skill_id: &str, proxy_url: Option<&str>) -> Result<Vec<WatchedUpstream>, AppError> {
    let watches = list(skill_id)?;
    if watches.is_empty() {
        return Ok(watches);
    }
    let results: Vec<Resolved> = watches.iter().map(|w| resolve(w, proxy_url)).collect();
    let _lock = RepoLock::acquire_foreground("check watched upstreams").map_err(AppError::db)?;
    apply_unlocked(skill_id, &results, chrono::Utc::now().timestamp_millis())
}

/// One watch's check result, resolved off the lock: its id, the
/// `last_seen_revision` it was resolved against, and the head plus whether it
/// moved (or why the remote could not be resolved).
pub(crate) type Resolved = (String, Option<String>, Result<(String, bool), String>);

/// Resolve one watch. Network; never takes the repo lock.
pub(crate) fn resolve(watch: &WatchedUpstream, proxy_url: Option<&str>) -> Resolved {
    (
        watch.id.clone(),
        watch.last_seen_revision.clone(),
        resolve_movement(watch, proxy_url),
    )
}

/// Store `results` in the skill's watch file, as of `now`. The caller holds the
/// repo lock. Returns the list as it now stands, with `checked_at` set to `now`
/// on every entry this check resolved.
pub(crate) fn apply_unlocked(
    skill_id: &str,
    results: &[Resolved],
    now: i64,
) -> Result<Vec<WatchedUpstream>, AppError> {
    modify_unlocked(skill_id, |list| {
        let mut changed = false;
        for (id, seen, result) in results {
            // Removed or re-seen meanwhile: leave it to the next check.
            let Some(entry) = list
                .iter_mut()
                .find(|w| &w.id == id && &w.last_seen_revision == seen)
            else {
                continue;
            };
            let before = (
                entry.remote_revision.clone(),
                entry.moved,
                entry.check_error.clone(),
            );
            match result {
                Ok((head, moved)) => {
                    entry.remote_revision = Some(head.clone());
                    entry.moved = *moved;
                    entry.check_error = None;
                }
                Err(err) => entry.check_error = Some(err.clone()),
            }
            let after = (
                entry.remote_revision.clone(),
                entry.moved,
                entry.check_error.clone(),
            );
            if before != after {
                entry.checked_at = Some(now);
                changed = true;
            }
        }
        Ok((list.clone(), changed))
    })
    .map(|mut list| {
        // Report this check's time even where nothing needed writing.
        for entry in &mut list {
            if results
                .iter()
                .any(|(id, _, r)| id == &entry.id && r.is_ok())
            {
                entry.checked_at = Some(now);
            }
        }
        list
    })
}

/// The remote head, and whether it moved since `last_seen_revision`. For a
/// subpath, a head that left that directory's tree unchanged has not moved
/// (the same comparison `skills check` uses for monorepo skills, #561).
fn resolve_movement(
    watch: &WatchedUpstream,
    proxy_url: Option<&str>,
) -> Result<(String, bool), String> {
    let head =
        git_fetcher::resolve_remote_revision(&watch.git_url, watch.branch.as_deref(), proxy_url)
            .map_err(|err| err.to_string())?;
    let Some(seen) = watch.last_seen_revision.as_deref() else {
        // Never known: report it, so the owner looks once and marks it seen.
        return Ok((head, true));
    };
    if seen == head {
        return Ok((head, false));
    }
    let Some(subpath) = watch.subpath.as_deref().filter(|s| !s.is_empty()) else {
        return Ok((head, true));
    };
    let pair = (seen.to_string(), subpath.to_string());
    let installed: HashSet<(String, String)> = [pair.clone()].into_iter().collect();
    // A failed comparison reads as moved: a false "look at this" is cheaper
    // than a missed change.
    let unchanged = git_fetcher::unchanged_subtrees(&watch.git_url, &head, &installed, proxy_url)
        .map(|set| set.contains(&pair))
        .unwrap_or(false);
    Ok((head, !unchanged))
}

// ── fork ──────────────────────────────────────────────────────────────────

/// Copy a library skill to a new local skill named `new_name` that watches the
/// original's git source at the original's revision. The original is not
/// touched. Refused when a skill or directory of that name already exists.
pub fn fork_skill(
    store: &SkillStore,
    skill_id: &str,
    new_name: &str,
) -> Result<ForkResult, AppError> {
    let original = store
        .get_skill_by_id(skill_id)
        .map_err(AppError::db)?
        .ok_or_else(|| AppError::not_found("Skill not found"))?;
    let name = skill_metadata::sanitize_skill_name(new_name.trim())
        .filter(|n| !n.is_empty() && n == new_name.trim())
        .ok_or_else(|| AppError::invalid_input(format!("Invalid skill name: '{new_name}'")))?;
    let source_dir = PathBuf::from(&original.central_path);
    if !source_dir.is_dir() {
        return Err(AppError::not_found(format!(
            "The library copy of '{}' is missing",
            original.name
        )));
    }
    let watched = fork_watch_for(&original);

    let _lock = RepoLock::acquire_foreground("fork skill").map_err(AppError::db)?;
    let destination = central_repo::skills_dir().join(&name);
    let taken_by_name = store
        .get_all_skills()
        .map_err(AppError::db)?
        .iter()
        .any(|s| s.name.eq_ignore_ascii_case(&name));
    let taken_by_path = store
        .get_skill_by_central_path(&destination.to_string_lossy())
        .map_err(AppError::db)?
        .is_some();
    if taken_by_name || taken_by_path || destination.exists() {
        return Err(AppError::invalid_input(format!(
            "A skill named '{name}' already exists"
        )));
    }

    let mut result = installer::install_skill_dir_to_destination(&source_dir, &name, &destination)
        .map_err(AppError::io)?;
    // The reindex names a skill after its SKILL.md `name:`, so a byte-identical
    // copy would come back as the original's name. Only that line changes.
    let renamed = rename_in_frontmatter(&destination, &name).and_then(|changed| {
        if changed {
            result.content_hash = crate::core::content_hash::hash_directory(&destination)?;
        }
        Ok(())
    });
    if let Err(err) = renamed {
        let _ = std::fs::remove_dir_all(&destination);
        return Err(AppError::io(err));
    }
    let metadata = skill_cmd::InstallSourceMetadata {
        source_type: "local".to_string(),
        source_ref: None,
        source_ref_resolved: None,
        source_subpath: None,
        source_branch: None,
        source_revision: None,
        remote_revision: None,
        update_status: "local_only".to_string(),
    };
    let new_id = match skill_cmd::store_installed_skill_unlocked(store, &result, &metadata, None) {
        Ok(id) => id,
        Err(err) => {
            let _ = std::fs::remove_dir_all(&destination);
            return Err(err);
        }
    };

    let watched = match watched {
        Some(mut watch) => {
            watch.id = new_id_from(&[]);
            let file = WatchFile {
                version: WATCH_FILE_VERSION,
                upstreams: vec![watch.clone()],
                extra: Map::new(),
            };
            fork_files::write_unlocked(ForkConcern::Watch, &new_id, &file).map_err(AppError::io)?;
            Some(watch)
        }
        None => None,
    };

    store.log_audit(
        crate::core::audit_log::AuditDraft::new("install")
            .skill(new_id.clone(), name.clone())
            .detail(format!("fork of {}", original.name))
            .ok(),
    );

    Ok(ForkResult {
        skill_id: new_id,
        name,
        central_path: result.central_path.to_string_lossy().to_string(),
        forked_from: original.id,
        watched,
    })
}

/// The watched upstream a fork of `original` starts with: its git source at
/// the revision its library content came from. Prefers the baseline file's
/// revision (it syncs), then `source_revision` (null on a device that got the
/// library by backup clone, spike Q5).
fn fork_watch_for(original: &crate::core::skill_store::SkillRecord) -> Option<WatchedUpstream> {
    let source = skill_cmd::git_source_from_skill(original).ok()?;
    let revision = baseline_revision(&original.id).or_else(|| original.source_revision.clone());
    Some(WatchedUpstream {
        id: String::new(),
        git_url: source.clone_url,
        subpath: clean(source.subpath),
        branch: clean(source.branch),
        last_seen_revision: revision,
        note: Some(format!("forked from {}", original.name)),
        added_at: chrono::Utc::now().timestamp_millis(),
        remote_revision: None,
        moved: false,
        checked_at: None,
        check_error: None,
        extra: Map::new(),
    })
}

/// The `revision` of a skill's baseline file (epic design §2), if one exists.
/// Read loosely so this doesn't depend on the baseline module's types.
fn baseline_revision(skill_id: &str) -> Option<String> {
    let value = fork_files::read::<Value>(ForkConcern::Baseline, skill_id).ok()??;
    value
        .get("revision")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|r| !r.is_empty())
}

/// Set the top-level `name:` of the frontmatter in `dir`'s SKILL.md to
/// `new_name`, keeping every other byte, line endings included. Returns
/// whether the file changed. A file without frontmatter or without a `name:`
/// key is left alone: the reindex then names the skill after its directory.
fn rename_in_frontmatter(dir: &Path, new_name: &str) -> anyhow::Result<bool> {
    let Some(path) = ["SKILL.md", "skill.md"]
        .iter()
        .map(|f| dir.join(f))
        .find(|p| p.is_file())
    else {
        return Ok(false);
    };
    let content = std::fs::read_to_string(&path)?;
    let Some(updated) = frontmatter_with_name(&content, new_name) else {
        return Ok(false);
    };
    if updated == content {
        return Ok(false);
    }
    std::fs::write(&path, updated)?;
    Ok(true)
}

fn frontmatter_with_name(content: &str, new_name: &str) -> Option<String> {
    let plain = !new_name.is_empty()
        && new_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && new_name.starts_with(|c: char| c.is_ascii_alphanumeric());
    let value = if plain {
        new_name.to_string()
    } else {
        // A JSON string is a valid YAML double-quoted scalar.
        serde_json::to_string(new_name).ok()?
    };
    let mut out = String::with_capacity(content.len() + value.len());
    let mut state = 0; // 0: before the opening fence, 1: inside, 2: after
    let mut replaced = false;
    for line in content.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        let ending = &line[body.len()..];
        match state {
            0 if body.trim().is_empty() => {}
            0 if body.trim_end() == "---" => state = 1,
            0 => return None,
            1 if body.starts_with("---") => state = 2,
            1 if !replaced && body.starts_with("name:") => {
                out.push_str("name: ");
                out.push_str(&value);
                out.push_str(ending);
                replaced = true;
                continue;
            }
            _ => {}
        }
        out.push_str(line);
    }
    (replaced && state == 2).then_some(out)
}

// ── helpers ───────────────────────────────────────────────────────────────

fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Validate and normalize a location. Remote URLs and `owner/repo` shorthand
/// go through `parse_git_source`, so a GitHub tree URL supplies branch and
/// subpath unless they are given explicitly. A `file://` URL or an existing
/// local directory is kept as given.
fn normalize_location(
    url: &str,
    branch: Option<String>,
    subpath: Option<String>,
) -> Result<(String, Option<String>, Option<String>), AppError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(AppError::invalid_input("A git URL is required"));
    }
    // Reaches `git ls-remote` as an argument: never let it read as an option.
    if url.starts_with('-') {
        return Err(AppError::invalid_input(format!("Invalid git URL: {url}")));
    }
    let subpath = clean(subpath).map(|s| s.replace('\\', "/").trim_matches('/').to_string());
    if subpath
        .as_deref()
        .is_some_and(|s| s.split('/').any(|part| part == ".." || part == "."))
    {
        return Err(AppError::invalid_input("Invalid subpath"));
    }
    let branch = clean(branch);
    if git_fetcher::validate_git_url(url).is_ok() {
        let parsed = git_fetcher::parse_git_source(url);
        return Ok((
            parsed.clone_url,
            branch.or(clean(parsed.branch)),
            subpath.or(clean(parsed.subpath)).filter(|s| !s.is_empty()),
        ));
    }
    if url.to_ascii_lowercase().starts_with("file://") || is_local_repo(Path::new(url)) {
        return Ok((url.to_string(), branch, subpath.filter(|s| !s.is_empty())));
    }
    Err(AppError::invalid_input(format!(
        "Not a git location: {url}. Use https, ssh, git@, owner/repo, file:// or a local repository path."
    )))
}

fn is_local_repo(path: &Path) -> bool {
    path.is_absolute() && path.is_dir()
}

fn validate_revision(revision: &str) -> Result<(), AppError> {
    if revision.len() >= 4
        && revision.len() <= 64
        && revision.bytes().all(|b| b.is_ascii_hexdigit())
    {
        Ok(())
    } else {
        Err(AppError::invalid_input(format!(
            "Not a commit id: {revision}"
        )))
    }
}

fn same_location(a: &WatchedUpstream, b: &WatchedUpstream) -> bool {
    let url = |u: &str| {
        u.trim()
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .to_string()
    };
    url(&a.git_url) == url(&b.git_url) && a.subpath == b.subpath && a.branch == b.branch
}

fn describe_location(watch: &WatchedUpstream) -> String {
    let mut out = watch.git_url.clone();
    if let Some(branch) = &watch.branch {
        out.push_str(&format!(" ({branch})"));
    }
    if let Some(subpath) = &watch.subpath {
        out.push_str(&format!(" /{subpath}"));
    }
    out
}

fn find_index(list: &[WatchedUpstream], watch_id: &str) -> Result<usize, AppError> {
    list.iter()
        .position(|w| w.id == watch_id)
        .ok_or_else(|| AppError::not_found(format!("No watched upstream with id '{watch_id}'")))
}

fn new_id(list: &[WatchedUpstream]) -> String {
    new_id_from(&list.iter().map(|w| w.id.as_str()).collect::<Vec<_>>())
}

fn new_id_from(taken: &[&str]) -> String {
    loop {
        let candidate = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        if !taken.contains(&candidate.as_str()) {
            return candidate;
        }
    }
}

pub mod batch;

#[cfg(test)]
mod tests;
