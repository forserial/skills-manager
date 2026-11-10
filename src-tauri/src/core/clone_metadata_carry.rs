//! First connect by clone: carry local-only library metadata into the clone
//! (sm-69q.15).
//!
//! `git_backup::clone_into_unlocked` moves an existing library aside, clones,
//! and `merge_backup` copies in the top-level entries the clone lacks. The
//! clone has its own `.skills-manager/`, so the local one was skipped whole:
//! a skill only this device had kept its folder but lost its record (the
//! reindex reads metadata only), its fork files (baseline, pin, notes,
//! watch), and local-only presets and memberships went with it. This module
//! runs right after `merge_backup` and before any reindex or metadata
//! rewrite (`write_all_from_db` sweeps fork files whose skill has no record,
//! so later is too late), and merges `.skills-manager/` file by file:
//!
//! - **Skill record** (`skills/<id>.json`): carried when its folder is in the
//!   merged library and the clone has no record for that folder (by path
//!   key) and none under that id. One record per folder, never two: when
//!   both devices installed the same skill under different ids, the clone's
//!   record is the one that stays. A folder the clone has without any record
//!   (one a device orphaned before this fix, then backed up) is adopted by
//!   the local record, the only one there is.
//! - **Fork files** (`fork-<concern>.<id>.json`, any concern, known or not)
//!   come with a carried record.
//! - **Preset** (`scenarios/<id>.json`): carried when the clone has no preset
//!   with that id. When the clone has one with the same name under another
//!   id, the local preset is merged into it instead of kept beside it: a
//!   preset name is unique in the database, so two would fail the reindex.
//! - **Membership** (`scenario-skills/<preset>/<skill>.json`): carried when
//!   its preset and its skill both exist after the merge (mapped to the
//!   clone's ids where they were matched) and at least one of them came from
//!   this device. A preset and a skill the clone already had keep the
//!   clone's membership list. Appended after the clone's members.
//! - Everything else (`schema.json`, `protocol.json`, any file the clone
//!   has) stays the clone's. Nothing is ever overwritten or written through
//!   a link.
//!
//! A clone with no `schema.json` keeps no skill index; the app then rebuilds
//! the index from the database, which still holds the local records, so only
//! their fork files are carried.
//!
//! Writes go into the clone only, never into the moved-aside library, so the
//! caller's rollback (drop the clone, move the library back) undoes all of
//! it. Local metadata that cannot be read stops the clone: that library
//! cannot be reindexed either, and skipping a record would orphan its skill.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path};

use super::sync_metadata::{self, ScenarioMetaFile, ScenarioSkillMetaFile, SkillMetaFile};

const METADATA_DIR: &str = ".skills-manager";

/// Carry what only `backup` (the library moved aside before the clone) has
/// in its `.skills-manager/` into `clone`, after `merge_backup` has copied
/// in the local-only folders. See the module docs for the rules.
pub(crate) fn carry_local_metadata(backup: &Path, clone: &Path) -> Result<()> {
    carry(&backup.join(METADATA_DIR), clone, &clone.join(METADATA_DIR))
        .context("Failed to carry the local library's records into the clone")
}

fn carry(local: &Path, clone: &Path, theirs: &Path) -> Result<()> {
    if !is_real_dir(local)? {
        return Ok(());
    }
    // `merge_backup` copied the whole folder in when the clone had none.
    if !is_real_dir(theirs)? {
        bail!("{} in the clone is not a folder", theirs.display());
    }
    let local_records: Vec<Json<SkillMetaFile>> = read_json_dir(&local.join("skills"), Strict::Yes)?;
    if !is_real_file(&theirs.join("schema.json"))? {
        for record in &local_records {
            let parsed = record.parsed();
            if folder_exists(clone, &parsed.path)? {
                carry_fork_files(local, theirs, &parsed.skill_id)?;
            }
        }
        return Ok(());
    }
    let skills = carry_skill_records(local, clone, theirs, &local_records)?;
    let presets = carry_presets(local, theirs)?;
    carry_memberships(local, theirs, &skills, &presets)
}

/// Where a local skill or preset ended up after the merge: its id in the
/// merged library, and whether it came from this device.
#[derive(Debug, Clone)]
struct Placed {
    id: String,
    from_local: bool,
    /// The local file was carried as it is (a preset: its sort orders stand).
    whole: bool,
}

fn carry_skill_records(
    local: &Path,
    clone: &Path,
    theirs: &Path,
    local_records: &[Json<SkillMetaFile>],
) -> Result<HashMap<String, Placed>> {
    let dir = theirs.join("skills");
    let mut ids = HashSet::new();
    let mut by_key = HashMap::new();
    for record in read_json_dir::<SkillMetaFile>(&dir, Strict::No)? {
        ids.insert(record.stem.clone());
        if let Some(parsed) = record.value {
            ids.insert(parsed.skill_id.clone());
            by_key.insert(sync_metadata::path_key(&parsed.path), parsed.skill_id);
        }
    }
    let mut placed = HashMap::new();
    for record in local_records {
        let parsed = record.parsed();
        let key = sync_metadata::path_key(&parsed.path);
        if let Some(id) = by_key.get(&key) {
            let at = Placed { id: id.clone(), from_local: false, whole: false };
            placed.insert(parsed.skill_id.clone(), at);
            continue;
        }
        if ids.contains(&parsed.skill_id) || ids.contains(&record.stem) {
            log::warn!(
                "git clone: not carrying the record of {} ({}): the clone has another skill under that id",
                parsed.path,
                parsed.skill_id
            );
            continue;
        }
        if !folder_exists(clone, &parsed.path)? {
            continue;
        }
        ensure_real_dir(&dir)?;
        copy_new(&local.join("skills").join(&record.file_name), &dir.join(&record.file_name))?;
        carry_fork_files(local, theirs, &parsed.skill_id)?;
        ids.insert(parsed.skill_id.clone());
        by_key.insert(key, parsed.skill_id.clone());
        let at = Placed { id: parsed.skill_id.clone(), from_local: true, whole: true };
        placed.insert(parsed.skill_id.clone(), at);
    }
    Ok(placed)
}

fn carry_presets(local: &Path, theirs: &Path) -> Result<HashMap<String, Placed>> {
    let dir = theirs.join("scenarios");
    let mut ids = HashSet::new();
    let mut by_name = HashMap::new();
    for preset in read_json_dir::<ScenarioMetaFile>(&dir, Strict::No)? {
        ids.insert(preset.stem.clone());
        if let Some(parsed) = preset.value {
            ids.insert(parsed.scenario_id.clone());
            by_name.insert(parsed.name, parsed.scenario_id);
        }
    }
    let mut placed = HashMap::new();
    for preset in read_json_dir::<ScenarioMetaFile>(&local.join("scenarios"), Strict::Yes)? {
        let parsed = preset.parsed();
        let at = if ids.contains(&parsed.scenario_id) || ids.contains(&preset.stem) {
            Placed { id: parsed.scenario_id.clone(), from_local: false, whole: false }
        } else if let Some(id) = by_name.get(&parsed.name) {
            Placed { id: id.clone(), from_local: true, whole: false }
        } else {
            ensure_real_dir(&dir)?;
            copy_new(&local.join("scenarios").join(&preset.file_name), &dir.join(&preset.file_name))?;
            ids.insert(parsed.scenario_id.clone());
            by_name.insert(parsed.name.clone(), parsed.scenario_id.clone());
            Placed { id: parsed.scenario_id.clone(), from_local: true, whole: true }
        };
        placed.insert(parsed.scenario_id.clone(), at);
    }
    Ok(placed)
}

fn carry_memberships(
    local: &Path,
    theirs: &Path,
    skills: &HashMap<String, Placed>,
    presets: &HashMap<String, Placed>,
) -> Result<()> {
    let root = theirs.join("scenario-skills");
    let mut members = Vec::new();
    let local_root = local.join("scenario-skills");
    if is_real_dir(&local_root)? {
        for entry in fs::read_dir(&local_root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                for member in read_json_dir::<ScenarioSkillMetaFile>(&entry.path(), Strict::Yes)? {
                    members.push(member.value.expect("strict reads parse every file"));
                }
            }
        }
    }
    // Local order within each preset, so appended members keep it.
    members.sort_by(|a, b| (&a.scenario_id, a.sort_order).cmp(&(&b.scenario_id, b.sort_order)));
    let mut next_sort: BTreeMap<String, i32> = BTreeMap::new();
    for member in members {
        let (Some(preset), Some(skill)) = (presets.get(&member.scenario_id), skills.get(&member.skill_id))
        else {
            continue;
        };
        if !preset.from_local && !skill.from_local {
            continue;
        }
        let dir = root.join(&preset.id);
        let target = dir.join(format!("{}.json", skill.id));
        if exists(&target)? {
            continue;
        }
        let sort_order = if preset.whole {
            member.sort_order
        } else {
            let next = match next_sort.get(&preset.id) {
                Some(next) => *next,
                None => last_sort_order(&dir)?.map_or(0, |last| last.saturating_add(1)),
            };
            next_sort.insert(preset.id.clone(), next.saturating_add(1));
            next
        };
        ensure_real_dir(&root)?;
        ensure_real_dir(&dir)?;
        write_new(
            &target,
            &ScenarioSkillMetaFile {
                schema_version: member.schema_version,
                scenario_id: preset.id.clone(),
                skill_id: skill.id.clone(),
                sort_order,
                tools: member.tools,
            },
        )?;
    }
    Ok(())
}

/// The highest sort order among the clone's members of one preset.
fn last_sort_order(dir: &Path) -> Result<Option<i32>> {
    Ok(read_json_dir::<ScenarioSkillMetaFile>(dir, Strict::No)?
        .into_iter()
        .filter_map(|member| member.value.map(|m| m.sort_order))
        .max())
}

/// Copy every `fork-<concern>.<skill_id>.json` the clone does not have.
fn carry_fork_files(local: &Path, theirs: &Path, skill_id: &str) -> Result<()> {
    let suffix = format!(".{skill_id}.json");
    for entry in fs::read_dir(local)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let is_fork_file = name
            .strip_suffix(&suffix)
            .is_some_and(|stem| stem.starts_with("fork-") && !stem.contains('.'));
        if is_fork_file && entry.file_type()?.is_file() && !exists(&theirs.join(name))? {
            copy_new(&entry.path(), &theirs.join(name))?;
        }
    }
    Ok(())
}

/// Whether `relative` (a record's library path) names a folder in the merged
/// library. A path that could leave the library names none.
fn folder_exists(library: &Path, relative: &str) -> Result<bool> {
    let path = Path::new(relative);
    if relative.is_empty() || !path.components().all(|c| matches!(c, Component::Normal(_))) {
        return Ok(false);
    }
    Ok(fs::metadata(library.join(path)).is_ok_and(|m| m.is_dir()))
}

#[derive(Clone, Copy, PartialEq)]
enum Strict {
    /// Every `*.json` must parse (the local library's own metadata).
    Yes,
    /// Unreadable files are kept by name only (the clone's: they are its to
    /// keep, and its own reindex reports them).
    No,
}

struct Json<T> {
    file_name: OsString,
    stem: String,
    value: Option<T>,
}

impl<T> Json<T> {
    fn parsed(&self) -> &T {
        self.value.as_ref().expect("strict reads parse every file")
    }
}

/// The regular `*.json` files directly in `dir` (none when it is absent).
fn read_json_dir<T: DeserializeOwned>(dir: &Path, strict: Strict) -> Result<Vec<Json<T>>> {
    if !is_real_dir(dir)? {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() || path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parsed = fs::read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|raw| serde_json::from_slice(&raw).map_err(anyhow::Error::from));
        let value = match (parsed, strict) {
            (Ok(value), _) => Some(value),
            (Err(err), Strict::Yes) => {
                return Err(err.context(format!(
                    "the local library's metadata file {} cannot be read",
                    path.display()
                )));
            }
            (Err(err), Strict::No) => {
                log::warn!("git clone: unreadable metadata in the clone {}: {err:#}", path.display());
                None
            }
        };
        files.push(Json { file_name: entry.file_name(), stem, value });
    }
    files.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    Ok(files)
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

/// A folder, not a link to one. Absent is `false`; anything else is an error.
fn is_real_dir(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(true),
        Ok(_) => bail!("{} is not a folder", path.display()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

fn is_real_file(path: &Path) -> Result<bool> {
    Ok(fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file()))
}

fn ensure_real_dir(path: &Path) -> Result<()> {
    if !is_real_dir(path)? {
        fs::create_dir(path).with_context(|| format!("Failed to create {}", path.display()))?;
    }
    Ok(())
}

/// Copy a file to a path that must not exist yet: nothing the clone has is
/// overwritten, and no link is written through.
fn copy_new(src: &Path, dst: &Path) -> Result<()> {
    write_bytes_new(dst, &fs::read(src).with_context(|| format!("Failed to read {}", src.display()))?)
}

fn write_new<T: Serialize>(dst: &Path, value: &T) -> Result<()> {
    write_bytes_new(dst, &sync_metadata::canonical_json_bytes(value)?)
}

fn write_bytes_new(dst: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)
        .with_context(|| format!("Failed to create {}", dst.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("Failed to write {}", dst.display()))
}

#[cfg(test)]
mod tests;
