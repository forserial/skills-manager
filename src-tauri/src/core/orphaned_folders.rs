//! Library folders that no skill record points at, and adopting them back
//! (sm-69q.16).
//!
//! A top-level folder in the library with no record is invisible: the app and
//! CLI don't list it, updates and deployments skip it. That happens after a
//! first connect by clone on a build without sm-69q.15, through a folder copied
//! in by hand, or a lost metadata file. [`find`] lists such folders and never
//! writes anything; [`adopt`] gives one a new record in place, as a local skill,
//! through the installer's own registration ([`store_installed_skill_unlocked`]).
//! Nothing is copied, moved or written inside the folder: the record and its
//! metadata file live in the database and `.skills-manager/`.
//!
//! What the folder had before it lost its record (id, source, baseline, pin,
//! change notes, watches) is not recoverable, and [`CANNOT_RECOVER`] says so.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use super::central_repo;
use super::content_hash;
use super::error::AppError;
use super::installer::InstallResult;
use super::repo_lock::RepoLock;
use super::skill_metadata;
use super::skill_store::SkillStore;
use crate::commands::skills::{self as skill_cmd, InstallSourceMetadata};

/// The app's metadata folder at the library root.
const METADATA_DIR: &str = ".skills-manager";

/// Prefixes of the folders a first connect by clone creates for the local
/// library (`skills-backup-before-clone-*`) and for the local versions of
/// skills that differed from the backup (`skills-local-copies-*`, the
/// `CLONE_LOCAL_DIFFERS` set-aside). Both are made *beside* the library today,
/// so they never show up at its root; matched anyway, so one moved in by hand
/// or made by an older build is not offered as a skill.
const SET_ASIDE_PREFIXES: [&str; 2] = ["skills-backup-before-clone-", "skills-local-copies-"];

/// The managed section `git_backup::apply_oversized_exclusions` writes into the
/// library's `.gitignore`. Its lines are bookkeeping for skills kept out of the
/// backup for their size, not rules saying "this is not a skill", so they are
/// not applied here: an oversized folder with no record is still orphaned.
/// Kept in step with `git_backup` by a test.
pub(crate) const OVERSIZED_SECTION_BEGIN: &str =
    "# skills-manager: oversized skills excluded from backup (auto-managed)";
pub(crate) const OVERSIZED_SECTION_END: &str = "# skills-manager: end oversized skills";

/// Said wherever a folder is adopted: the CLI report, the app dialog, SKILL.md.
pub const CANNOT_RECOVER: &str = "Adopting makes the folder a new local skill with a new id. \
What it had before it lost its record cannot be recovered: its old id, source, baseline, pin, \
change notes and watched upstreams are gone, and it starts with no tags, presets or deployments. \
To link it to git again, use `skills set-source`: the baseline is then upstream's current \
revision, not the one the folder came from, so which changes were local can no longer be told \
apart. set-source refuses when the folder differs from upstream (and --force replaces the folder \
with upstream's copy), so check with --dry-run first; afterwards `skills diff --upstream` \
compares the skill with upstream.";

/// What kind of folder [`find`] listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderKind {
    /// A folder no record points at; the only kind that can be adopted.
    Unmanaged,
    /// `.X.staged-<uuid>`: a copy an install or update was making (sm-69q.23).
    Staged,
    /// `.X.backup-<uuid>`: the old copy an update set aside (sm-69q.23).
    Backup,
}

/// A top-level library folder that no skill record points at, or a hidden
/// copy an interrupted install or update left that the startup step neither
/// put back nor removed (see `swap_leftovers`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrphanedFolder {
    /// The folder's name at the library root; what `adopt` takes.
    pub dir_name: String,
    pub path: String,
    /// Has a `SKILL.md` (or `skill.md`). Only then can it be adopted.
    pub has_skill_md: bool,
    /// The name it would get: SKILL.md's `name:`, else the folder name, as the
    /// startup reindex names skills. `None` without a SKILL.md.
    pub name: Option<String>,
    /// Bytes of the files under it (links not followed).
    pub size: u64,
    /// Newest modification time under it, Unix milliseconds.
    pub modified: Option<i64>,
    pub kind: FolderKind,
    /// For a staged or backup folder: what it is and what to do with it.
    pub note: Option<String>,
}

/// A folder [`adopt`] gave a record.
#[derive(Debug, Clone, Serialize)]
pub struct AdoptedFolder {
    pub skill_id: String,
    pub name: String,
    pub dir_name: String,
    pub central_path: String,
}

/// A folder `adopt_all` left alone, and why.
#[derive(Debug, Clone, Serialize)]
pub struct SkippedFolder {
    pub dir_name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdoptAllResult {
    pub adopted: Vec<AdoptedFolder>,
    pub skipped: Vec<SkippedFolder>,
}

/// Top-level library folders no skill record points at. Read-only, takes no
/// lock: a folder adopted or removed meanwhile is caught by [`adopt`], which
/// looks again under the lock.
pub fn find(store: &SkillStore) -> Result<Vec<OrphanedFolder>> {
    find_in(store, &central_repo::skills_dir())
}

fn find_in(store: &SkillStore, root: &Path) -> Result<Vec<OrphanedFolder>> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let claimed = claimed_top_level_names(store, root)?;
    let ignore = IgnoreRules::read(root);
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        // Not followed: a link at the root is not a folder the library holds.
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(dir_name) = entry.file_name().to_str().map(str::to_string) else {
            continue; // not valid Unicode: nothing could name it to adopt it
        };
        if is_excluded_name(&dir_name)
            || ignore.ignores_dir(&dir_name)
            || claimed.contains(&name_key(&dir_name))
        {
            continue;
        }
        let path = entry.path();
        let has_skill_md = skill_metadata::is_valid_skill_dir(&path);
        let name = has_skill_md.then(|| skill_name(&path, &dir_name));
        let (size, modified) = size_and_modified(&path);
        found.push(OrphanedFolder {
            dir_name,
            path: path.to_string_lossy().to_string(),
            has_skill_md,
            name,
            size,
            modified,
            kind: FolderKind::Unmanaged,
            note: None,
        });
    }
    // sm-69q.23: the hidden copies, which no folder name above includes.
    let now = std::time::SystemTime::now();
    for leftover in super::swap_leftovers::find_in(store, root) {
        found.push(leftover_folder(root, &leftover, now));
    }
    found.sort_by(|a, b| a.dir_name.to_lowercase().cmp(&b.dir_name.to_lowercase()));
    Ok(found)
}

fn leftover_folder(
    root: &Path,
    leftover: &super::swap_leftovers::Leftover,
    now: std::time::SystemTime,
) -> OrphanedFolder {
    let path = &leftover.path;
    let dir_name = path
        .strip_prefix(root)
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().to_string());
    let has_skill_md = skill_metadata::is_valid_skill_dir(path);
    let leaf = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let (size, modified) = size_and_modified(path);
    OrphanedFolder {
        name: has_skill_md.then(|| skill_name(path, &leaf)),
        dir_name,
        path: path.to_string_lossy().to_string(),
        has_skill_md,
        size,
        modified,
        kind: match leftover.kind {
            super::swap_leftovers::LeftoverKind::Staged => FolderKind::Staged,
            super::swap_leftovers::LeftoverKind::Backup => FolderKind::Backup,
        },
        note: Some(super::swap_leftovers::describe(leftover, now)),
    }
}

/// `.git`, `.skills-manager`, every other dot-folder (staged and swap
/// directories of an update are `.name.staged-*` / `.name.backup-*`), and the
/// clone's set-aside folders.
fn is_excluded_name(name: &str) -> bool {
    name.starts_with('.') || SET_ASIDE_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Give the folder `dir_name` at the library root a record in place, as a local
/// skill with a new id. Refused when it is not an unmanaged folder (see
/// [`find`]) or has no SKILL.md. The folder's files are only read.
pub fn adopt(store: &SkillStore, dir_name: &str) -> Result<AdoptedFolder, AppError> {
    let dir_name = plain_dir_name(dir_name)?;
    let _lock = RepoLock::acquire_foreground("adopt unmanaged folder").map_err(AppError::db)?;
    let root = central_repo::skills_dir();
    let found = find_in(store, &root).map_err(AppError::io)?;
    // Matched as the file system names folders; the listed spelling is used.
    let folder = found
        .into_iter()
        .find(|f| name_key(&f.dir_name) == name_key(dir_name))
        .ok_or_else(|| not_unmanaged(dir_name))?;
    adopt_unlocked(store, &root, &folder)
}

/// Adopt every unmanaged folder that has a SKILL.md, under one lock. Folders
/// without one are reported as skipped.
pub fn adopt_all(store: &SkillStore) -> Result<AdoptAllResult, AppError> {
    let _lock = RepoLock::acquire_foreground("adopt unmanaged folders").map_err(AppError::db)?;
    let root = central_repo::skills_dir();
    let mut result = AdoptAllResult {
        adopted: Vec::new(),
        skipped: Vec::new(),
    };
    for folder in find_in(store, &root).map_err(AppError::io)? {
        match adopt_unlocked(store, &root, &folder) {
            Ok(adopted) => result.adopted.push(adopted),
            Err(err) => result.skipped.push(SkippedFolder {
                dir_name: folder.dir_name,
                reason: err.message,
            }),
        }
    }
    Ok(result)
}

fn adopt_unlocked(
    store: &SkillStore,
    root: &Path,
    folder: &OrphanedFolder,
) -> Result<AdoptedFolder, AppError> {
    if folder.kind != FolderKind::Unmanaged {
        return Err(AppError::invalid_input(format!(
            "'{}' is the hidden copy an interrupted install or update left, not a skill to adopt. {}",
            folder.dir_name,
            folder.note.as_deref().unwrap_or_default()
        )));
    }
    let dir = root.join(&folder.dir_name);
    if !skill_metadata::is_valid_skill_dir(&dir) {
        return Err(AppError::invalid_input(format!(
            "'{}' has no SKILL.md, so it is not a skill and cannot be adopted. \
             Add a SKILL.md to it, or move it out of the library.",
            folder.dir_name
        )));
    }
    // What the installer would hand over after copying, for the copy that is
    // already in place. Named as the startup reindex will name it, or the next
    // launch would rename it.
    let parsed = skill_metadata::parse_skill_md(&dir);
    let result = InstallResult {
        name: skill_name(&dir, &folder.dir_name),
        description: parsed.description,
        central_path: dir.clone(),
        content_hash: content_hash::hash_directory(&dir).map_err(AppError::io)?,
        // The user's folder: a failed adopt must never remove it.
        created_folder: false,
    };
    // No source_ref: the folder is its own source, and a local skill with one
    // would offer to re-import itself.
    let metadata = InstallSourceMetadata {
        source_type: "local".to_string(),
        source_ref: None,
        source_ref_resolved: None,
        source_subpath: None,
        source_branch: None,
        source_revision: None,
        remote_revision: None,
        update_status: "local_only".to_string(),
    };
    let skill_id = skill_cmd::store_installed_skill_unlocked(store, &result, &metadata, None)?;
    store.log_audit(
        super::audit_log::AuditDraft::new("install")
            .skill(skill_id.clone(), result.name.clone())
            .detail(format!(
                "adopted unmanaged library folder '{}' in place",
                folder.dir_name
            ))
            .ok(),
    );
    Ok(AdoptedFolder {
        skill_id,
        name: result.name,
        dir_name: folder.dir_name.clone(),
        central_path: dir.to_string_lossy().to_string(),
    })
}

/// A single folder name, never a path: `adopt` must not reach outside the root.
fn plain_dir_name(dir_name: &str) -> Result<&str, AppError> {
    let trimmed = dir_name.trim_end_matches(['/', '\\']);
    let mut parts = Path::new(trimmed).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(name)), None) if name.to_str() == Some(trimmed) => Ok(trimmed),
        _ => Err(AppError::invalid_input(format!(
            "'{dir_name}' is not a folder name. Pass the name of a folder at the library root, \
             as `skills orphans` lists it."
        ))),
    }
}

fn not_unmanaged(dir_name: &str) -> AppError {
    AppError::not_found(format!(
        "'{dir_name}' is not an unmanaged folder in the library. `skills orphans` lists the ones \
         that are; a folder a skill already uses, a dot-folder or one the backup ignores is not."
    ))
}

/// SKILL.md's `name:`, else the folder name: the startup reindex's rule.
fn skill_name(dir: &Path, dir_name: &str) -> String {
    skill_metadata::parse_skill_md(dir)
        .name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| dir_name.to_string())
}

/// Top-level folder names a skill uses: the first component of every record's
/// path under the root, and of every metadata file's path (the next reindex
/// would bring that skill back, so its folder is not orphaned either).
fn claimed_top_level_names(store: &SkillStore, root: &Path) -> Result<HashSet<String>> {
    let mut claimed = HashSet::new();
    let canonical_root = root.canonicalize().ok();
    for skill in store.get_all_skills()? {
        let path = PathBuf::from(&skill.central_path);
        let relative = path.strip_prefix(root).map(Path::to_path_buf).ok().or_else(|| {
            let canonical = path.canonicalize().ok()?;
            canonical
                .strip_prefix(canonical_root.as_ref()?)
                .map(Path::to_path_buf)
                .ok()
        });
        if let Some(first) = relative.as_deref().and_then(first_component) {
            claimed.insert(name_key(&first));
        }
    }
    let metadata_skills = root.join(METADATA_DIR).join("skills");
    if let Ok(entries) = std::fs::read_dir(&metadata_skills) {
        for entry in entries.flatten() {
            let Ok(raw) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
                continue;
            };
            if let Some(first) = value
                .get("path")
                .and_then(|p| p.as_str())
                .and_then(|p| p.split(['/', '\\']).find(|part| !part.is_empty()))
            {
                claimed.insert(name_key(first));
            }
        }
    }
    Ok(claimed)
}

fn first_component(relative: &Path) -> Option<String> {
    match relative.components().next()? {
        Component::Normal(name) => Some(name.to_string_lossy().to_string()),
        _ => None,
    }
}

/// Folder names compared as the file system does: case-insensitively on
/// Windows and macOS, so `Foo` is not offered beside a record for `foo`.
fn name_key(name: &str) -> String {
    if cfg!(any(windows, target_os = "macos")) {
        name.to_lowercase()
    } else {
        name.to_string()
    }
}

fn size_and_modified(dir: &Path) -> (u64, Option<i64>) {
    let mut size = 0u64;
    let mut modified: Option<i64> = None;
    for entry in walkdir::WalkDir::new(dir).into_iter().flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_file() {
            size += meta.len();
        }
        let millis = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64);
        if let Some(millis) = millis {
            modified = Some(modified.map_or(millis, |m| m.max(millis)));
        }
    }
    (size, modified)
}

/// The library's own `.gitignore`, applied to top-level folder names: what the
/// backup already leaves out (`__pycache__/` by default, plus the user's own
/// rules) is not offered as a skill. Patterns that only match deeper paths are
/// ignored, as is the managed oversized section (see
/// [`OVERSIZED_SECTION_BEGIN`]). Later rules win, `!` re-includes.
struct IgnoreRules {
    rules: Vec<(String, bool)>,
}

impl IgnoreRules {
    fn read(root: &Path) -> Self {
        let raw = std::fs::read_to_string(root.join(".gitignore")).unwrap_or_default();
        Self::parse(&raw)
    }

    fn parse(raw: &str) -> Self {
        let mut rules = Vec::new();
        let mut in_oversized = false;
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed == OVERSIZED_SECTION_BEGIN {
                in_oversized = true;
                continue;
            }
            if trimmed == OVERSIZED_SECTION_END {
                in_oversized = false;
                continue;
            }
            if in_oversized || trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let (negated, pattern) = match trimmed.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, trimmed),
            };
            let pattern = pattern.strip_prefix("**/").unwrap_or(pattern);
            let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
            let pattern = pattern.trim_end_matches('/');
            // A slash left inside names a deeper path, never a root folder.
            if pattern.is_empty() || pattern.contains('/') {
                continue;
            }
            rules.push((pattern.to_string(), negated));
        }
        Self { rules }
    }

    fn ignores_dir(&self, name: &str) -> bool {
        let mut ignored = false;
        for (pattern, negated) in &self.rules {
            if glob_matches(pattern.as_bytes(), name.as_bytes()) {
                ignored = !negated;
            }
        }
        ignored
    }
}

/// gitignore-style glob for one path component: `*`, `?`, `[...]` and `\`
/// escapes.
fn glob_matches(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|skip| glob_matches(rest, &name[skip..])),
        Some((b'?', rest)) => !name.is_empty() && glob_matches(rest, &name[1..]),
        Some((b'[', rest)) => match (rest.iter().position(|&c| c == b']'), name.split_first()) {
            (Some(close), Some((&c, name_rest))) if close > 0 => {
                let (class, negate) = match rest[..close].split_first() {
                    Some((b'!' | b'^', class)) => (class, true),
                    _ => (&rest[..close], false),
                };
                class_contains(class, c) != negate && glob_matches(&rest[close + 1..], name_rest)
            }
            (None, Some((&c, name_rest))) => c == b'[' && glob_matches(rest, name_rest),
            _ => false,
        },
        Some((b'\\', rest)) if !rest.is_empty() => {
            name.first() == Some(&rest[0]) && glob_matches(&rest[1..], &name[1..])
        }
        Some((&c, rest)) => name.first() == Some(&c) && glob_matches(rest, &name[1..]),
    }
}

fn class_contains(class: &[u8], c: u8) -> bool {
    let mut i = 0;
    while i < class.len() {
        if i + 2 < class.len() && class[i + 1] == b'-' {
            if (class[i]..=class[i + 2]).contains(&c) {
                return true;
            }
            i += 3;
        } else {
            if class[i] == c {
                return true;
            }
            i += 1;
        }
    }
    false
}

#[cfg(test)]
mod tests;
