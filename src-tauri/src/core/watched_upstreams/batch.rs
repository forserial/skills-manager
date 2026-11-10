//! Watched upstreams in the app's own update checks (sm-69q.13).
//!
//! `check_all_skill_updates`, `check_skill_update` and the background
//! auto-updater each make one call into this module, so a skill whose
//! borrowed-from location moved is reported without opening its detail panel.
//!
//! - Every watch of every skill is resolved concurrently (bounded, like the
//!   skill checks' `resolve_remotes_concurrent`) and off the repo lock. The
//!   lock is then taken once for the whole batch, only to write the results.
//! - Nothing here returns an error to the caller. An unreachable upstream is
//!   stored on its entry (`check_error`), and a watch file that cannot be read
//!   or written is logged, so a watched upstream never fails the skill checks
//!   it rides along with.
//! - A skill with no watches costs one small file read and no network.
//! - Unforced checks (app start, the auto-updater) skip a skill whose watches
//!   were checked within the skill-check TTL (`update_check_ttl_minutes`,
//!   default 60). That memory is per process: `checked_at` is deliberately not
//!   written on a check that changes nothing (no backup commit per check), so
//!   the file can't carry it.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::{any_moved, apply_unlocked, list, resolve, Resolved, WatchedUpstream};
use crate::core::central_repo;
use crate::core::repo_lock::RepoLock;
use crate::core::skill_store::SkillStore;

/// Upper bound on concurrent remote lookups, as for the skill checks.
const MAX_CONCURRENCY: usize = 8;
const DEFAULT_TTL_MINUTES: u64 = 60;

/// When each skill's watches were last checked by this process, keyed by
/// library location and skill id.
static LAST_CHECKED: Mutex<BTreeMap<String, Instant>> = Mutex::new(BTreeMap::new());

/// What one batch did. For logs and tests; callers don't act on it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BatchSummary {
    /// Skills whose watches were resolved and stored.
    pub skills_checked: usize,
    pub watches_checked: usize,
    /// Watches whose remote could not be resolved (stored on the entry).
    pub unreachable: usize,
    /// Skills with at least one moved watch after this batch.
    pub moved_skills: Vec<String>,
    /// Skills whose watch file could not be read or written.
    pub failed_skills: Vec<String>,
}

/// A skill-card marker: how many locations a skill watches and how many moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatchedMarker {
    pub skill_id: String,
    pub watch_count: usize,
    pub moved_count: usize,
}

/// Check the watched upstreams of every skill in the library. `force` ignores
/// the TTL (a user's "Check all"). Never fails; see the module docs.
pub fn check_library(store: &SkillStore, force: bool, proxy_url: Option<&str>) -> BatchSummary {
    let ids: Vec<String> = match store.get_all_skills() {
        Ok(skills) => skills.into_iter().map(|s| s.id).collect(),
        Err(err) => {
            log::warn!("watched upstreams: could not list skills: {err}");
            return BatchSummary::default();
        }
    };
    let skip_within = (!force).then(|| ttl(store));
    let summary = check_skills_with(&ids, skip_within, |w| resolve(w, proxy_url));
    if summary.watches_checked > 0 || !summary.failed_skills.is_empty() {
        log::info!(
            "watched upstreams: checked {} watch(es) of {} skill(s); moved: {}; unreachable: {}; failed: {}",
            summary.watches_checked,
            summary.skills_checked,
            summary.moved_skills.len(),
            summary.unreachable,
            summary.failed_skills.len()
        );
    }
    summary
}

/// Check one skill's watched upstreams (the single-skill "Check for update").
/// Returns at once, with no network, for a skill that watches nothing. Must be
/// called without the repo lock held. Never fails; see the module docs.
pub fn check_skill(skill_id: &str, proxy_url: Option<&str>) {
    let summary = check_skills_with(&[skill_id.to_string()], None, |w| resolve(w, proxy_url));
    if !summary.failed_skills.is_empty() {
        log::warn!("watched upstreams: check of {skill_id} could not be stored");
    }
}

/// Card markers for every skill that watches something. No network.
pub fn markers(store: &SkillStore) -> Vec<WatchedMarker> {
    let Ok(skills) = store.get_all_skills() else {
        return Vec::new();
    };
    skills
        .into_iter()
        .filter_map(|skill| {
            let watches = list(&skill.id).ok()?;
            (!watches.is_empty()).then(|| WatchedMarker {
                watch_count: watches.len(),
                moved_count: watches.iter().filter(|w| w.moved).count(),
                skill_id: skill.id,
            })
        })
        .collect()
}

/// The batch, with the resolver injected so tests can watch what runs where.
/// `skip_within`: skip a skill checked by this process more recently than
/// that; `None` checks every skill.
pub(crate) fn check_skills_with<F>(
    skill_ids: &[String],
    skip_within: Option<Duration>,
    resolver: F,
) -> BatchSummary
where
    F: Fn(&WatchedUpstream) -> Resolved + Sync,
{
    let mut summary = BatchSummary::default();

    // Which watches to resolve, as (index into `skills`, watch).
    let mut skills: Vec<&String> = Vec::new();
    let mut jobs: Vec<(usize, WatchedUpstream)> = Vec::new();
    for skill_id in skill_ids {
        let watches = match list(skill_id) {
            Ok(watches) => watches,
            Err(err) => {
                log::warn!(
                    "watched upstreams: could not read {skill_id}'s: {}",
                    err.message
                );
                summary.failed_skills.push(skill_id.clone());
                continue;
            }
        };
        if watches.is_empty() || skip_within.is_some_and(|ttl| checked_within(skill_id, ttl)) {
            continue;
        }
        skills.push(skill_id);
        jobs.extend(watches.into_iter().map(|w| (skills.len() - 1, w)));
    }
    if jobs.is_empty() {
        return summary;
    }

    // Network, off the lock.
    let resolved = run_concurrent(&jobs, |(_, watch)| resolver(watch));
    let mut per_skill: Vec<Vec<Resolved>> = vec![Vec::new(); skills.len()];
    for ((index, _), result) in jobs.iter().zip(resolved) {
        per_skill[*index].push(result);
    }

    // One lock for the whole batch, only for the writes.
    let _lock = match RepoLock::acquire_foreground("check watched upstreams") {
        Ok(lock) => lock,
        Err(err) => {
            log::warn!("watched upstreams: results not stored, repository busy: {err}");
            summary
                .failed_skills
                .extend(skills.iter().map(|id| (*id).clone()));
            return summary;
        }
    };
    let now = chrono::Utc::now().timestamp_millis();
    for (skill_id, results) in skills.into_iter().zip(per_skill) {
        match apply_unlocked(skill_id, &results, now) {
            Ok(stored) => {
                summary.skills_checked += 1;
                summary.watches_checked += results.len();
                summary.unreachable += results.iter().filter(|(_, _, r)| r.is_err()).count();
                if any_moved(&stored) {
                    summary.moved_skills.push(skill_id.clone());
                }
                mark_checked(skill_id);
            }
            Err(err) => {
                log::warn!(
                    "watched upstreams: could not store {skill_id}'s check: {}",
                    err.message
                );
                summary.failed_skills.push(skill_id.clone());
            }
        }
    }
    summary
}

fn ttl(store: &SkillStore) -> Duration {
    let minutes = store
        .get_setting("update_check_ttl_minutes")
        .ok()
        .flatten()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TTL_MINUTES);
    Duration::from_secs(minutes * 60)
}

fn ttl_key(skill_id: &str) -> String {
    format!("{}\n{skill_id}", central_repo::base_dir().display())
}

fn checked_within(skill_id: &str, ttl: Duration) -> bool {
    let map = LAST_CHECKED.lock().unwrap_or_else(|e| e.into_inner());
    map.get(&ttl_key(skill_id))
        .is_some_and(|at| at.elapsed() < ttl)
}

fn mark_checked(skill_id: &str) {
    let mut map = LAST_CHECKED.lock().unwrap_or_else(|e| e.into_inner());
    map.insert(ttl_key(skill_id), Instant::now());
}

/// Run `f` over every item, at most `MAX_CONCURRENCY` at a time, and return
/// the results in item order. Never touches the repo lock.
fn run_concurrent<T, R, F>(items: &[T], f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<R>>> = items.iter().map(|_| Mutex::new(None)).collect();
    let workers = MAX_CONCURRENCY.min(items.len()).max(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(index) else { break };
                let result = f(item);
                *slots[index].lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
            });
        }
    });
    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .unwrap_or_else(|e| e.into_inner())
                .expect("every item is resolved before the scope ends")
        })
        .collect()
}

#[cfg(test)]
#[path = "batch_tests.rs"]
mod tests;
