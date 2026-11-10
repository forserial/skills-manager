//! Pins (sm-69q.5): hold a git-sourced skill at the revision its owner
//! reviewed, and list what upstream has done since.
//!
//! A pin is per-skill state in `.skills-manager/fork-pin.<skill_id>.json`,
//! written through [`fork_files`] (epic design §11.1: flat in the metadata
//! root, so it syncs with the library; newest commit wins per file).
//!
//! What a pin does:
//! - `check` is untouched: it still resolves and stores `remote_revision`, so
//!   a pinned skill can read "update available (pinned)".
//! - Updating that one skill by hand is refused with a message naming
//!   `skills unpin` ([`refuse_update_if_pinned`], called at the top of
//!   `update_git_skill_internal`, so every update path is covered).
//! - Batch updates, `skills update --all` and the auto-updater skip the skill
//!   before trying, and report it as pinned ([`holds`]).
//!
//! Unpinning writes `pinned: false` rather than deleting the file. A
//! whole-file three-way merge resolves modify/modify by newest commit, which
//! is predictable; a delete against a modify is not.
//!
//! An unreadable pin file holds the skill: unattended code must never move a
//! skill whose owner may have pinned it. `skills unpin` overwrites the file.

use git2::{Oid, Repository, Sort};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::error::AppError;
use super::fork_files::{self, ForkConcern};
use super::skill_store::{SkillRecord, SkillStore};
use super::{baseline, git_fetcher};

/// The record stored in `fork-pin.<skill_id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinState {
    pub pinned: bool,
    /// The revision the skill was pinned at: its source revision, else its
    /// baseline's. `None` when neither was known, and after an unpin.
    #[serde(default)]
    pub revision: Option<String>,
    /// Unix milliseconds of the last pin or unpin.
    pub pinned_at: i64,
    #[serde(default)]
    pub note: Option<String>,
}

/// The pin, as stored. A missing file is `Ok(None)`.
pub fn load(skill_id: &str) -> anyhow::Result<Option<PinState>> {
    fork_files::read(ForkConcern::Pin, skill_id)
}

/// For reports and the GUI: the pin when the skill is pinned, else `None`.
/// An unreadable file is logged and reported as a pin with no revision, so
/// the owner sees why the skill is held.
pub fn active(skill_id: &str) -> Option<PinState> {
    match load(skill_id) {
        Ok(state) => state.filter(|s| s.pinned),
        Err(err) => {
            log::warn!("unreadable pin for {skill_id}: {err:#}");
            Some(PinState {
                pinned: true,
                revision: None,
                pinned_at: 0,
                note: Some(format!("unreadable pin file: {err}")),
            })
        }
    }
}

/// Whether update paths must leave this skill alone.
pub fn holds(skill_id: &str) -> bool {
    active(skill_id).is_some()
}

/// Called first thing by `update_git_skill_internal`: a pinned skill is never
/// updated, whoever asks.
pub fn refuse_update_if_pinned(skill: &SkillRecord) -> Result<(), AppError> {
    let Some(pin) = active(&skill.id) else {
        return Ok(());
    };
    let at = pin
        .revision
        .as_deref()
        .map(|rev| format!(" at {}", short(rev)))
        .unwrap_or_default();
    Err(AppError::invalid_input(format!(
        "{} is pinned{at}; run `skills-manager-cli skills unpin {}` (or unpin it in the app) before updating",
        skill.name, skill.name
    )))
}

fn short(rev: &str) -> &str {
    rev.get(..7).unwrap_or(rev)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn skill_or_not_found(store: &SkillStore, skill_id: &str) -> Result<SkillRecord, AppError> {
    store
        .get_skill_by_id(skill_id)
        .map_err(AppError::db)?
        .ok_or_else(|| AppError::not_found("Skill not found"))
}

fn is_git_source(skill: &SkillRecord) -> bool {
    matches!(skill.source_type.as_str(), "git" | "skillssh")
}

/// Pin a git or skills.sh skill at its current revision. Pinning a skill that
/// is already pinned keeps its revision and time, and replaces the note when
/// one is given.
pub fn pin(store: &SkillStore, skill_id: &str, note: Option<&str>) -> Result<PinState, AppError> {
    let skill = skill_or_not_found(store, skill_id)?;
    if !is_git_source(&skill) {
        return Err(AppError::invalid_input(
            "Only git and skills.sh skills can be pinned: other skills have no upstream revision",
        ));
    }
    let note = note.map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
    let current = load(skill_id).ok().flatten().filter(|s| s.pinned);
    let state = match current {
        Some(existing) => PinState {
            note: note.or(existing.note),
            ..existing
        },
        None => PinState {
            pinned: true,
            revision: skill.source_revision.clone().or_else(|| {
                baseline::load(skill_id).ok().flatten().map(|b| b.revision)
            }),
            pinned_at: now_ms(),
            note,
        },
    };
    fork_files::write(ForkConcern::Pin, skill_id, &state).map_err(AppError::io)?;
    Ok(state)
}

/// Unpin a skill. Unpinning one that is not pinned is a no-op that still
/// reports the (unpinned) state.
pub fn unpin(store: &SkillStore, skill_id: &str) -> Result<PinState, AppError> {
    skill_or_not_found(store, skill_id)?;
    let state = PinState {
        pinned: false,
        revision: None,
        pinned_at: now_ms(),
        note: None,
    };
    // Nothing to write when it was never pinned or is already unpinned. An
    // unreadable file is overwritten: this is how the owner clears it.
    if matches!(load(skill_id), Ok(None)) || matches!(load(skill_id), Ok(Some(s)) if !s.pinned) {
        return Ok(state);
    }
    fork_files::write(ForkConcern::Pin, skill_id, &state).map_err(AppError::io)?;
    Ok(state)
}

// ── upstream commits since the baseline ──

/// Most commits listed; more are reported as `truncated`.
const MAX_COMMITS: usize = 500;

/// How far before the base commit's date to cut history. Committer dates are
/// not strictly ordered, so a margin keeps the base's recent ancestors in the
/// fetched window.
const SHALLOW_MARGIN_SECS: i64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpstreamCommit {
    pub sha: String,
    /// Committer date, RFC 3339.
    pub date: String,
    pub author: String,
    pub subject: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpstreamCommits {
    pub skill_id: String,
    /// The revision the list starts from (exclusive).
    pub base_revision: String,
    /// `"baseline"`, or `"source_revision"` when the skill has no baseline.
    pub base_from: String,
    /// The upstream head the list ends at (inclusive).
    pub remote_revision: String,
    /// The directory the commits were filtered on; `None` for the repo root.
    pub subpath: Option<String>,
    /// False when the base revision is gone upstream or is not in the head's
    /// history (a force push). `commits` is then empty and `message` says why.
    pub reachable: bool,
    pub message: Option<String>,
    /// Newest first. Only commits that change the skill's directory.
    pub commits: Vec<UpstreamCommit>,
    pub truncated: bool,
}

/// The upstream commits between a skill's baseline revision and the current
/// upstream head that touch the skill's directory. Network work only: no repo
/// lock is taken, and nothing in the library is written.
pub fn upstream_commits(
    store: &SkillStore,
    skill_id: &str,
    proxy_url: Option<&str>,
) -> Result<UpstreamCommits, AppError> {
    let skill = skill_or_not_found(store, skill_id)?;
    upstream_commits_for(&skill, proxy_url, true)
}

/// `validate_url` is false only in tests, which serve upstreams over `file://`.
fn upstream_commits_for(
    skill: &SkillRecord,
    proxy_url: Option<&str>,
    validate_url: bool,
) -> Result<UpstreamCommits, AppError> {
    if !is_git_source(skill) {
        return Err(AppError::invalid_input(
            "Only git and skills.sh skills have upstream commits",
        ));
    }
    let source = crate::commands::skills::git_source_from_skill(skill)?;
    if validate_url {
        git_fetcher::validate_git_url(&source.clone_url).map_err(AppError::git)?;
    }
    let (base_revision, base_from) = match baseline::load(&skill.id) {
        Ok(Some(b)) => (b.revision, "baseline"),
        _ => match skill.source_revision.clone() {
            Some(rev) => (rev, "source_revision"),
            None => {
                return Err(AppError::invalid_input(
                    "No baseline or source revision is recorded for this skill, so there is nothing to list commits from",
                ))
            }
        },
    };
    if !is_object_id(&base_revision) {
        return Err(AppError::invalid_input(format!(
            "The recorded revision {base_revision:?} is not a full commit id"
        )));
    }
    let remote_revision = git_fetcher::resolve_remote_revision(
        &source.clone_url,
        source.branch.as_deref(),
        proxy_url,
    )
    .map_err(AppError::git)?;
    let subpath = source
        .subpath
        .as_deref()
        .map(|s| s.replace('\\', "/").trim_matches('/').to_string())
        .filter(|s| !s.is_empty());

    let mut result = UpstreamCommits {
        skill_id: skill.id.clone(),
        base_revision: base_revision.clone(),
        base_from: base_from.to_string(),
        remote_revision: remote_revision.clone(),
        subpath: subpath.clone(),
        reachable: true,
        message: None,
        commits: Vec::new(),
        truncated: false,
    };
    if base_revision == remote_revision {
        return Ok(result);
    }

    match fetch_history(&source.clone_url, &base_revision, &remote_revision, proxy_url)? {
        History::Gone(msg) => {
            result.reachable = false;
            result.message = Some(format!(
                "The baseline revision {} is no longer available upstream ({msg}), so the commits since it cannot be listed",
                short(&base_revision)
            ));
            Ok(result)
        }
        History::Fetched(dir) => {
            let listed = list_commits(&dir, &base_revision, &remote_revision, subpath.as_deref());
            let _ = std::fs::remove_dir_all(&dir);
            match listed? {
                Some((commits, truncated)) => {
                    result.commits = commits;
                    result.truncated = truncated;
                }
                None => {
                    result.reachable = false;
                    result.message = Some(format!(
                        "The baseline revision {} is not in the history of upstream {} (was upstream history rewritten?), so the commits since it cannot be listed",
                        short(&base_revision),
                        short(&remote_revision)
                    ));
                }
            }
            Ok(result)
        }
    }
}

fn is_object_id(rev: &str) -> bool {
    matches!(rev.len(), 40 | 64) && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

enum History {
    /// A bare repository holding the commits and trees between the two.
    Fetched(PathBuf),
    /// The server does not have the base revision ("not our ref").
    Gone(String),
}

fn temp_repo(label: &str) -> Result<PathBuf, AppError> {
    let dir = std::env::temp_dir().join(format!("skills-manager-{label}-{}", uuid::Uuid::new_v4()));
    Repository::init_bare(&dir).map_err(AppError::git)?;
    Ok(dir)
}

/// Fetch enough history to walk from `remote` back to `base`, blobless, into
/// throwaway bare repositories. The app's clones are depth 1 (design §11.1,
/// Q4), so they cannot answer this.
///
/// First the base commit alone, to learn its date and whether the server still
/// has it. Then both revisions with history cut a margin before that date, so
/// a long-lived repository is not fetched whole. If the server refuses the
/// date cut, the full blobless history is fetched instead.
fn fetch_history(
    url: &str,
    base: &str,
    remote: &str,
    proxy_url: Option<&str>,
) -> Result<History, AppError> {
    let probe = temp_repo("pin-probe")?;
    let base_time = (|| -> Result<Result<i64, String>, AppError> {
        if let Err(err) = git_fetcher::run_git_watched_at(
            &probe,
            &["fetch", "--no-tags", "--depth=1", "--filter=blob:none", url, base],
            proxy_url,
            None,
            &None,
        ) {
            let msg = err.to_string();
            if msg.contains("not our ref") {
                return Ok(Err(msg));
            }
            return Err(AppError::git(msg));
        }
        let repo = Repository::open_bare(&probe).map_err(AppError::git)?;
        let oid = Oid::from_str(base).map_err(AppError::git)?;
        let commit = repo.find_commit(oid).map_err(AppError::git)?;
        Ok(Ok(commit.time().seconds()))
    })();
    let _ = std::fs::remove_dir_all(&probe);
    let base_time = match base_time? {
        Ok(time) => time,
        Err(msg) => return Ok(History::Gone(msg)),
    };

    let dir = temp_repo("pin-history")?;
    let since = format!("--shallow-since={}", base_time - SHALLOW_MARGIN_SECS);
    let windowed = git_fetcher::run_git_watched_at(
        &dir,
        &["fetch", "--no-tags", "--filter=blob:none", &since, url, base, remote],
        proxy_url,
        None,
        &None,
    );
    if windowed.is_ok() {
        return Ok(History::Fetched(dir));
    }
    let _ = std::fs::remove_dir_all(&dir);
    let dir = temp_repo("pin-history")?;
    match git_fetcher::run_git_watched_at(
        &dir,
        &["fetch", "--no-tags", "--filter=blob:none", url, base, remote],
        proxy_url,
        None,
        &None,
    ) {
        Ok(()) => Ok(History::Fetched(dir)),
        Err(err) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(AppError::git(err))
        }
    }
}

/// The commits reachable from `remote` and not from `base` that change
/// `subpath`, newest first. `None` when `base` is not an ancestor of `remote`.
///
/// A commit "changes" the directory when its tree there differs from every
/// parent's, as `git log -- <path>` decides by default: a merge that only
/// brings in a side branch's change is left out, and the side branch's own
/// commits are listed.
fn list_commits(
    dir: &Path,
    base: &str,
    remote: &str,
    subpath: Option<&str>,
) -> Result<Option<(Vec<UpstreamCommit>, bool)>, AppError> {
    let repo = Repository::open_bare(dir).map_err(AppError::git)?;
    let base = Oid::from_str(base).map_err(AppError::git)?;
    let remote = Oid::from_str(remote).map_err(AppError::git)?;
    if !repo.graph_descendant_of(remote, base).unwrap_or(false) {
        return Ok(None);
    }

    let entry_at = |commit: &git2::Commit| -> Option<Oid> {
        let tree = commit.tree().ok()?;
        match subpath {
            None => Some(tree.id()),
            Some(path) => tree.get_path(Path::new(path)).ok().map(|e| e.id()),
        }
    };

    let mut walk = repo.revwalk().map_err(AppError::git)?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)
        .map_err(AppError::git)?;
    walk.push(remote).map_err(AppError::git)?;
    walk.hide(base).map_err(AppError::git)?;

    let mut commits = Vec::new();
    let mut truncated = false;
    let mut seen = BTreeSet::new();
    for oid in walk {
        let Ok(oid) = oid else { break };
        if !seen.insert(oid) {
            continue;
        }
        let Ok(commit) = repo.find_commit(oid) else { continue };
        let here = entry_at(&commit);
        // Parents cut off by the shallow fetch cannot be compared; a commit
        // with no comparable parent counts when the directory exists in it.
        let parents: Vec<Option<Oid>> = commit
            .parent_ids()
            .filter_map(|pid| repo.find_commit(pid).ok())
            .map(|parent| entry_at(&parent))
            .collect();
        let changes = if parents.is_empty() {
            here.is_some()
        } else {
            parents.iter().all(|p| *p != here)
        };
        if !changes {
            continue;
        }
        if commits.len() == MAX_COMMITS {
            truncated = true;
            break;
        }
        let date = chrono::DateTime::from_timestamp(commit.time().seconds(), 0)
            .map(|d| d.to_rfc3339())
            .unwrap_or_default();
        commits.push(UpstreamCommit {
            sha: oid.to_string(),
            date,
            author: commit.author().name().unwrap_or_default().to_string(),
            subject: commit.summary().unwrap_or_default().to_string(),
        });
    }
    Ok(Some((commits, truncated)))
}

#[cfg(test)]
mod tests;
