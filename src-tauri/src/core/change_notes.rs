//! Change notes and patch export (sm-69q.8).
//!
//! A locally changed file of a git skill can carry a one-line reason, keyed
//! by skill and path. Notes live in `.skills-manager/fork-notes.<id>.json`
//! (the `Notes` fork-file concern, decided in the sm-69q spike): they sync
//! with the library and merge whole-file, newest commit wins.
//!
//! A note belongs to a local change, so it is dropped when its file no longer
//! differs from the baseline: reverting the file, or an update that brings
//! upstream level with it. Dropping happens whenever the notes are read
//! against a known baseline. While the baseline is unknown or suspect the
//! notes are kept as they are, because nothing can be said about the files.
//! Notes are keyed by path, so a note stays with its file across any change
//! of baseline that leaves the file differing.
//!
//! The patch export is a unified diff of the library copy against the
//! baseline content, for the changed files only, with paths from the root of
//! the upstream repository so `git apply` takes it at the root of a checkout
//! of the baseline revision. Each note is a `# <path>: <note>` comment line
//! directly above its file's `diff --git` header: git skips text between file
//! patches, but rejects a stray line between `+++` and the first `@@`.

use anyhow::{anyhow, bail, Context, Result};
use git2::{DiffFormat, DiffOptions, Index, IndexEntry, IndexTime, Odb, Repository};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use super::baseline::{
    self, with_baseline_content, BaseSource, FileChangeState, LocalChanges, LocalFileChange,
    LocalState,
};
use super::fork_files::{self, ForkConcern};
use super::repo_lock::RepoLock;
use super::skill_store::{SkillRecord, SkillStore};

/// Longest note kept, in characters. Notes are one line.
pub const MAX_NOTE_CHARS: usize = 500;

/// One file's reason for differing from the baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeNote {
    /// Relative to the skill directory, with `/` separators.
    pub path: String,
    pub note: String,
    /// Unix milliseconds.
    pub updated_at: i64,
}

/// `fork-notes.<id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct NotesFile {
    skill_id: String,
    /// Sorted by path, one per path.
    notes: Vec<ChangeNote>,
}

/// A changed file with its note, if it has one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotedChange {
    pub path: String,
    pub state: FileChangeState,
    pub note: Option<String>,
    pub note_updated_at: Option<i64>,
}

/// `skills changes <skill>`: the local changes with their notes.
#[derive(Debug, Clone, Serialize)]
pub struct ChangesReport {
    pub skill_id: String,
    pub name: String,
    pub state: LocalState,
    pub baseline_revision: Option<String>,
    /// Sorted by path. Empty unless `state` is `changed`.
    pub files: Vec<NotedChange>,
    /// Notes that could not be matched to a file because the baseline is
    /// unknown or suspect. They are kept until the baseline is known again.
    pub held_notes: Vec<ChangeNote>,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Paths are compared as the manifest writes them: `/`, no leading `./`.
pub fn normalize_path(path: &str) -> String {
    let mut path = path.trim().replace('\\', "/");
    while let Some(rest) = path.strip_prefix("./") {
        path = rest.to_string();
    }
    path.trim_matches('/').to_string()
}

/// One line, whitespace collapsed. Empty means "no note".
fn normalize_note(note: &str) -> String {
    note.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The stored notes of a skill, sorted by path. Missing file: none.
pub fn load(skill_id: &str) -> Result<Vec<ChangeNote>> {
    Ok(fork_files::read::<NotesFile>(ForkConcern::Notes, skill_id)?
        .map(|file| file.notes)
        .unwrap_or_default())
}

/// Write `notes`, or remove the file when there are none. Caller holds the
/// repo lock.
fn save_unlocked(skill_id: &str, mut notes: Vec<ChangeNote>) -> Result<()> {
    if notes.is_empty() {
        return fork_files::remove_unlocked(ForkConcern::Notes, skill_id);
    }
    notes.sort_by(|a, b| a.path.cmp(&b.path));
    notes.dedup_by(|a, b| a.path == b.path);
    let file = NotesFile {
        skill_id: skill_id.to_string(),
        notes,
    };
    fork_files::write_unlocked(ForkConcern::Notes, skill_id, &file)
}

/// The notes still matching a changed file. Against a known baseline, notes
/// for files that no longer differ are dropped from disk; otherwise the
/// notes are returned untouched.
fn reconcile(skill_id: &str, changes: &LocalChanges) -> Result<Vec<ChangeNote>> {
    let notes = load(skill_id)?;
    if notes.is_empty() || !changes.baseline_known() {
        return Ok(notes);
    }
    let changed: HashSet<&str> = changes.files.iter().map(|f| f.path.as_str()).collect();
    if notes.iter().all(|n| changed.contains(n.path.as_str())) {
        return Ok(notes);
    }
    let _lock = RepoLock::acquire_foreground("drop change notes")?;
    // Re-read under the lock: another writer may have got in first.
    let kept: Vec<ChangeNote> = load(skill_id)?
        .into_iter()
        .filter(|n| changed.contains(n.path.as_str()))
        .collect();
    save_unlocked(skill_id, kept.clone())?;
    Ok(kept)
}

fn report(skill: &SkillRecord, changes: LocalChanges, notes: Vec<ChangeNote>) -> ChangesReport {
    let by_path: BTreeMap<&str, &ChangeNote> = notes.iter().map(|n| (n.path.as_str(), n)).collect();
    let files = changes
        .files
        .iter()
        .map(|f| {
            let note = by_path.get(f.path.as_str());
            NotedChange {
                path: f.path.clone(),
                state: f.state,
                note: note.map(|n| n.note.clone()),
                note_updated_at: note.map(|n| n.updated_at),
            }
        })
        .collect();
    let held_notes = if changes.baseline_known() {
        Vec::new()
    } else {
        notes
    };
    ChangesReport {
        skill_id: skill.id.clone(),
        name: skill.name.clone(),
        state: changes.state,
        baseline_revision: changes.baseline_revision,
        files,
        held_notes,
    }
}

/// The local changes of `skill` with their notes. Drops the notes of files
/// that no longer differ. An unreadable notes file is logged and read as
/// empty; it is left on disk.
pub fn changes_report(store: &SkillStore, skill: &SkillRecord) -> Result<ChangesReport> {
    let changes = baseline::local_changes(store, skill)?;
    let notes = match reconcile(&skill.id, &changes) {
        Ok(notes) => notes,
        Err(err) => {
            log::warn!("change notes of {} unreadable: {err:#}", skill.id);
            Vec::new()
        }
    };
    Ok(report(skill, changes, notes))
}

/// Set the note of a changed file. The file must differ from a known
/// baseline. An empty note clears it.
pub fn set_note(
    store: &SkillStore,
    skill: &SkillRecord,
    path: &str,
    note: &str,
) -> Result<ChangesReport> {
    let note = normalize_note(note);
    if note.is_empty() {
        return clear_note(store, skill, path);
    }
    if note.chars().count() > MAX_NOTE_CHARS {
        bail!("a note is one line of at most {MAX_NOTE_CHARS} characters");
    }
    let path = normalize_path(path);
    let changes = baseline::local_changes(store, skill)?;
    match changes.state {
        LocalState::Changed | LocalState::Clean => {}
        LocalState::BaselineUnknown => bail!(
            "baseline unknown for {}: run `skills baseline rebuild {}` first, so its local changes can be told apart",
            skill.name,
            skill.name
        ),
        LocalState::BaselineSuspect => bail!(
            "baseline suspect for {}: a library sync conflict on it is pending; resolve it first",
            skill.name
        ),
        LocalState::Untracked => bail!(
            "{} is not a git or skills.sh skill: it has no baseline, so no local changes to note",
            skill.name
        ),
    }
    if !changes.files.iter().any(|f| f.path == path) {
        bail!(
            "{path} does not differ from the baseline of {}: only a changed file carries a note",
            skill.name
        );
    }
    {
        let _lock = RepoLock::acquire_foreground("write change note")?;
        let mut notes = load(&skill.id)?;
        notes.retain(|n| n.path != path);
        notes.push(ChangeNote {
            path,
            note,
            updated_at: now_ms(),
        });
        save_unlocked(&skill.id, notes)?;
    }
    changes_report(store, skill)
}

/// Clear the note of a file. Clearing a note that is not there is fine, and
/// works whatever the baseline state.
pub fn clear_note(store: &SkillStore, skill: &SkillRecord, path: &str) -> Result<ChangesReport> {
    let path = normalize_path(path);
    {
        let _lock = RepoLock::acquire_foreground("clear change note")?;
        let notes = load(&skill.id)?;
        if notes.iter().any(|n| n.path == path) {
            save_unlocked(
                &skill.id,
                notes.into_iter().filter(|n| n.path != path).collect(),
            )?;
        }
    }
    changes_report(store, skill)
}

// ── patch export ──

/// `skills changes export`: the patch and what it was made from.
#[derive(Debug, Clone, Serialize)]
pub struct PatchExport {
    pub skill_id: String,
    pub name: String,
    pub baseline_revision: String,
    pub base_source: BaseSource,
    /// The skill directory's path in the upstream repository, `/`-separated,
    /// empty at the root. Every path in the patch starts with it.
    pub path_prefix: String,
    pub file_count: usize,
    pub noted_file_count: usize,
    /// The patch bytes. Text files are written as they are, so this is not
    /// always UTF-8; [`PatchExport::text`] gives a lossy view.
    #[serde(skip)]
    pub patch: Vec<u8>,
}

impl PatchExport {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.patch).into_owned()
    }
}

/// A unified diff of the library copy of `skill` against its baseline
/// content (base store, else a by-SHA fetch). Fails with a clear message
/// when the skill has no local changes, no known baseline, or the baseline
/// content cannot be obtained.
pub fn export_patch(
    store: &SkillStore,
    skill: &SkillRecord,
    proxy_url: Option<&str>,
) -> Result<PatchExport> {
    export_patch_inner(store, skill, proxy_url, true)
}

/// `validate_url` is false only in tests, which serve upstreams over `file://`.
fn export_patch_inner(
    store: &SkillStore,
    skill: &SkillRecord,
    proxy_url: Option<&str>,
    validate_url: bool,
) -> Result<PatchExport> {
    let report = changes_report(store, skill)?;
    match report.state {
        LocalState::Changed => {}
        LocalState::Clean => bail!(
            "{} has no local changes against its baseline: there is nothing to export",
            skill.name
        ),
        LocalState::BaselineUnknown => bail!(
            "cannot export {}: its baseline is unknown, so there is nothing to diff against. Run `skills baseline rebuild {}` first",
            skill.name,
            skill.name
        ),
        LocalState::BaselineSuspect => bail!(
            "cannot export {}: a library sync conflict on it is pending, so its baseline may be wrong. Resolve it first",
            skill.name
        ),
        LocalState::Untracked => bail!(
            "cannot export {}: it is not a git or skills.sh skill, so it has no baseline",
            skill.name
        ),
    }
    let baseline = baseline::load(&skill.id)?
        .with_context(|| format!("the baseline of {} vanished while exporting", skill.name))?;
    let prefix = path_prefix(skill);
    let notes: BTreeMap<String, String> = report
        .files
        .iter()
        .filter_map(|f| f.note.clone().map(|n| (f.path.clone(), n)))
        .collect();
    let files: Vec<LocalFileChange> = report
        .files
        .iter()
        .map(|f| LocalFileChange {
            path: f.path.clone(),
            state: f.state,
        })
        .collect();
    let library = PathBuf::from(&skill.central_path);
    let (base_source, body) =
        with_baseline_content(skill, &baseline, proxy_url, validate_url, |base| {
            render_patch(base, &library, &files, &prefix, &notes)
        })
        .map_err(|err| {
            anyhow!(
                "cannot export {}: the baseline content at revision {} could not be obtained, from this device's base store or by fetching it ({err:#})",
                skill.name,
                baseline.revision
            )
        })?;

    let mut patch = Vec::new();
    let title = normalize_note(&skill.name);
    for line in [
        format!("# skills-manager: local changes to {title} ({})", skill.id),
        format!("# Baseline revision: {}", baseline.revision),
        if prefix.is_empty() {
            "# Apply at the root of a checkout of that revision: git apply <this file>".to_string()
        } else {
            format!(
                "# Apply at the root of a checkout of that revision (the skill is in {prefix}/): git apply <this file>"
            )
        },
        "# A \"# <path>: <note>\" line above a file's diff gives the reason for that change."
            .to_string(),
        String::new(),
    ] {
        patch.extend_from_slice(line.as_bytes());
        patch.push(b'\n');
    }
    patch.extend_from_slice(&body);
    Ok(PatchExport {
        skill_id: skill.id.clone(),
        name: skill.name.clone(),
        baseline_revision: baseline.revision,
        base_source,
        path_prefix: prefix,
        file_count: files.len(),
        noted_file_count: notes.len(),
        patch,
    })
}

/// Where the skill directory sits in its upstream repository.
fn path_prefix(skill: &SkillRecord) -> String {
    crate::commands::skills::git_source_from_skill(skill)
        .ok()
        .and_then(|source| source.subpath)
        .map(|subpath| normalize_path(&subpath))
        .unwrap_or_default()
}

fn repo_path(prefix: &str, path: &str) -> String {
    if prefix.is_empty() {
        path.to_string()
    } else {
        format!("{prefix}/{path}")
    }
}

/// The diff of `files` between `base` (baseline content) and `library`, as
/// git writes it, with each note above its file's header.
fn render_patch(
    base: &Path,
    library: &Path,
    files: &[LocalFileChange],
    prefix: &str,
    notes: &BTreeMap<String, String>,
) -> Result<Vec<u8>> {
    // Both sides go into an in-memory object database: nothing on disk.
    let odb = Odb::new()?;
    odb.add_new_mempack_backend(1)?;
    let repo = Repository::from_odb(odb)?;
    let mut old = Index::new()?;
    let mut new = Index::new()?;
    let mut by_repo_path: BTreeMap<String, String> = BTreeMap::new();

    for file in files {
        let path = repo_path(prefix, &file.path);
        by_repo_path.insert(path.clone(), file.path.clone());
        let base_file = base.join(&file.path);
        let base_content = match file.state {
            FileChangeState::Added => None,
            _ => Some((
                std::fs::read(&base_file)
                    .with_context(|| format!("the baseline content has no {}", file.path))?,
                file_mode(&base_file),
            )),
        };
        if let Some((content, mode)) = &base_content {
            add_entry(&repo, &mut old, &path, content, *mode)?;
        }
        if file.state != FileChangeState::Deleted {
            let lib_file = library.join(&file.path);
            let content = std::fs::read(&lib_file)
                .with_context(|| format!("failed to read {}", lib_file.display()))?;
            let (content, mode) = match &base_content {
                Some((base, mode)) => (match_line_endings(base, content), *mode),
                None => (content, file_mode(&lib_file)),
            };
            add_entry(&repo, &mut new, &path, &content, mode)?;
        }
    }

    let old_tree = repo.find_tree(old.write_tree_to(&repo)?)?;
    let new_tree = repo.find_tree(new.write_tree_to(&repo)?)?;
    let mut opts = DiffOptions::new();
    opts.show_binary(true).context_lines(3);
    let diff = repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), Some(&mut opts))?;

    let mut out: Vec<u8> = Vec::new();
    diff.print(DiffFormat::Patch, |delta, _hunk, line| {
        if line.origin() == 'F' {
            let path = delta
                .new_file()
                .path_bytes()
                .or_else(|| delta.old_file().path_bytes())
                .map(|p| String::from_utf8_lossy(p).into_owned());
            if let Some(rel) = path.as_ref().and_then(|p| by_repo_path.get(p)) {
                if let Some(note) = notes.get(rel) {
                    out.extend_from_slice(format!("# {rel}: {note}\n").as_bytes());
                }
            }
        }
        if matches!(line.origin(), '+' | '-' | ' ') {
            out.push(line.origin() as u8);
        }
        out.extend_from_slice(line.content());
        true
    })?;
    Ok(out)
}

fn add_entry(
    repo: &Repository,
    index: &mut Index,
    path: &str,
    content: &[u8],
    mode: u32,
) -> Result<()> {
    let id = repo.blob(content)?;
    let entry = IndexEntry {
        ctime: IndexTime::new(0, 0),
        mtime: IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode,
        uid: 0,
        gid: 0,
        file_size: content.len() as u32,
        id,
        flags: path.len().min(0xfff) as u16,
        flags_extended: 0,
        path: path.as_bytes().to_vec(),
    };
    index.add(&entry)?;
    Ok(())
}

fn file_mode(_path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(_path) {
            if meta.permissions().mode() & 0o111 != 0 {
                return 0o100755;
            }
        }
    }
    0o100644
}

/// Line endings are not a local change (detection folds CRLF), so give the
/// library side the baseline file's endings: otherwise every line would
/// differ and the patch would not apply. Mixed or binary files are left
/// as they are.
fn match_line_endings(base: &[u8], library: Vec<u8>) -> Vec<u8> {
    if base.contains(&0) || library.contains(&0) {
        return library;
    }
    let lf = base.iter().filter(|b| **b == b'\n').count();
    let crlf = base.windows(2).filter(|w| w == b"\r\n").count();
    let folded = fold_crlf(&library);
    if crlf == 0 {
        folded
    } else if crlf == lf {
        let mut out = Vec::with_capacity(folded.len() + folded.len() / 16);
        for byte in folded {
            if byte == b'\n' {
                out.push(b'\r');
            }
            out.push(byte);
        }
        out
    } else {
        library
    }
}

fn fold_crlf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests;
