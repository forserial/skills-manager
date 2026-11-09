//! Tables owned by this fork, kept out of the versioned migrations.
//!
//! `migrations.rs` tracks upstream's `PRAGMA user_version`. A fork-owned
//! version bump would collide with upstream's next migration and make it skip
//! on existing databases, so fork tables are created idempotently here, right
//! after the migrations run, and `user_version` is left alone.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use super::content_hash;
use super::evaluation::{EvaluationRecord, EvaluationResult, EvaluationStatus, EvaluationSummary};
use super::skill_store::SkillStore;

/// Create the fork's tables when missing. Safe to call on every open.
///
/// `skill_evaluations` keeps one row per run. Its rows follow their skill
/// through `upsert_skill` (an `ON CONFLICT DO UPDATE`, so the parent row is
/// never deleted) and go with it on `delete_skill`.
pub fn ensure_fork_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS skill_evaluations (
            id TEXT PRIMARY KEY,
            skill_id TEXT NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
            skill_content_hash TEXT,
            evaluator TEXT NOT NULL,
            evaluator_content_hash TEXT,
            agent TEXT NOT NULL,
            model TEXT,
            status TEXT NOT NULL,
            started_at INTEGER NOT NULL,
            finished_at INTEGER NOT NULL,
            duration_ms INTEGER NOT NULL,
            cost_usd REAL,
            grade TEXT,
            score_value REAL,
            score_max REAL,
            result_json TEXT,
            report_md TEXT,
            error TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_skill_evaluations_skill
            ON skill_evaluations(skill_id, started_at);",
    )?;
    Ok(())
}

/// The skill's content hash as its directory reads now.
///
/// `stale` compares against this rather than `skills.content_hash`, which only
/// in-app updates and the startup reindex refresh: an edit made on disk while
/// the app runs must mark the evaluation stale without a restart. It is the
/// same `hash_directory` the reindex stores in `skills.content_hash`, so an
/// unedited skill agrees with its DB hash. Falls back to `db_hash` when the
/// directory is missing, because `hash_directory` reads a missing directory as
/// an empty one. Never written back: the skills table stays upstream's.
pub(crate) fn live_content_hash(central_path: &str, db_hash: Option<String>) -> Option<String> {
    let dir = Path::new(central_path);
    if !dir.is_dir() {
        return db_hash;
    }
    match content_hash::hash_directory(dir) {
        Ok(hash) => Some(hash),
        Err(e) => {
            log::warn!("could not hash {}: {e}", dir.display());
            db_hash
        }
    }
}

/// Where a skill lives and the hash the DB holds for it, read under the lock
/// so the directory can be hashed after it is released.
struct SkillLocation {
    central_path: String,
    db_hash: Option<String>,
}

/// [`live_content_hash`] once per skill. Runs without the DB mutex held.
fn live_hashes(skills: HashMap<String, SkillLocation>) -> HashMap<String, Option<String>> {
    skills
        .into_iter()
        .map(|(id, loc)| (id, live_content_hash(&loc.central_path, loc.db_hash)))
        .collect()
}

fn is_stale(recorded: &Option<String>, current: Option<&Option<String>>) -> bool {
    // `None` would mean the skill row vanished between query and hash; the
    // JOIN found it, so this is unreachable in practice.
    current.map_or(true, |current| recorded != current)
}

const RECORD_COLUMNS: &str = "e.id, e.skill_id, e.skill_content_hash, e.evaluator,
    e.evaluator_content_hash, e.agent, e.model, e.status, e.started_at, e.finished_at,
    e.duration_ms, e.cost_usd, e.result_json, e.report_md, e.error,
    s.central_path, s.content_hash";

/// A record with `stale` still unset, plus its skill's location.
fn map_located(row: &rusqlite::Row<'_>) -> rusqlite::Result<(EvaluationRecord, SkillLocation)> {
    Ok((
        map_record(row)?,
        SkillLocation {
            central_path: row.get(15)?,
            db_hash: row.get(16)?,
        },
    ))
}

/// Hash each skill in `rows` once, then set every record's `stale`.
fn with_stale(rows: Vec<(EvaluationRecord, SkillLocation)>) -> Vec<EvaluationRecord> {
    let mut skills = HashMap::new();
    let mut records = Vec::with_capacity(rows.len());
    for (record, location) in rows {
        skills.entry(record.skill_id.clone()).or_insert(location);
        records.push(record);
    }
    let current = live_hashes(skills);
    for record in &mut records {
        record.stale = is_stale(&record.skill_content_hash, current.get(&record.skill_id));
    }
    records
}

fn map_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<EvaluationRecord> {
    let id: String = row.get(0)?;
    let result_json: Option<String> = row.get(12)?;
    let result = result_json.and_then(|json| {
        serde_json::from_str::<EvaluationResult>(&json)
            .map_err(|e| log::warn!("evaluation {id}: stored result is unreadable: {e}"))
            .ok()
    });
    Ok(EvaluationRecord {
        skill_id: row.get(1)?,
        skill_content_hash: row.get(2)?,
        evaluator: row.get(3)?,
        evaluator_content_hash: row.get(4)?,
        agent: row.get(5)?,
        model: row.get(6)?,
        status: EvaluationStatus::parse(&row.get::<_, String>(7)?),
        started_at: row.get(8)?,
        finished_at: row.get(9)?,
        duration_ms: row.get(10)?,
        cost_usd: row.get(11)?,
        result,
        report_md: row.get(13)?,
        error: row.get(14)?,
        stale: false,
        id,
    })
}

impl SkillStore {
    pub fn insert_evaluation(&self, record: &EvaluationRecord) -> Result<()> {
        let result_json = record
            .result
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let score = record.result.as_ref().and_then(|r| r.score.as_ref());
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO skill_evaluations (
                id, skill_id, skill_content_hash, evaluator, evaluator_content_hash, agent,
                model, status, started_at, finished_at, duration_ms, cost_usd, grade,
                score_value, score_max, result_json, report_md, error
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                record.id,
                record.skill_id,
                record.skill_content_hash,
                record.evaluator,
                record.evaluator_content_hash,
                record.agent,
                record.model,
                record.status.as_str(),
                record.started_at,
                record.finished_at,
                record.duration_ms,
                record.cost_usd,
                record.result.as_ref().and_then(|r| r.grade.clone()),
                score.map(|s| s.value),
                score.map(|s| s.max),
                result_json,
                record.report_md,
                record.error,
            ],
        )?;
        Ok(())
    }

    /// Every stored run for `skill_id`, newest first, each with its stale flag.
    pub fn list_evaluations(&self, skill_id: &str) -> Result<Vec<EvaluationRecord>> {
        let rows = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare(&format!(
                "SELECT {RECORD_COLUMNS}
                 FROM skill_evaluations e JOIN skills s ON s.id = e.skill_id
                 WHERE e.skill_id = ?1
                 ORDER BY e.started_at DESC, e.id DESC"
            ))?;
            let rows = stmt.query_map(params![skill_id], map_located)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok(with_stale(rows))
    }

    /// One stored run by id.
    pub fn get_evaluation(&self, id: &str) -> Result<Option<EvaluationRecord>> {
        let row = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                &format!(
                    "SELECT {RECORD_COLUMNS}
                     FROM skill_evaluations e JOIN skills s ON s.id = e.skill_id
                     WHERE e.id = ?1"
                ),
                params![id],
                map_located,
            )
            .optional()?
        };
        Ok(row.and_then(|row| with_stale(vec![row]).pop()))
    }

    /// Per skill, the latest successful run: grade, score, stale flag.
    /// Skills never evaluated successfully are absent.
    ///
    /// Runs on every skill-list reload. The stale flag hashes each listed
    /// skill's directory, so the cost is O(evaluated skills), not O(library):
    /// skills without a successful evaluation are never read from disk.
    pub fn latest_evaluation_summaries(&self) -> Result<Vec<EvaluationSummary>> {
        let rows = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT e.skill_id, e.id, e.grade, e.score_value, e.score_max,
                        e.skill_content_hash, e.finished_at, s.central_path, s.content_hash
                 FROM skill_evaluations e JOIN skills s ON s.id = e.skill_id
                 WHERE e.id = (
                     SELECT latest.id FROM skill_evaluations latest
                     WHERE latest.skill_id = e.skill_id AND latest.status = 'succeeded'
                     ORDER BY latest.started_at DESC, latest.id DESC
                     LIMIT 1
                 )
                 ORDER BY e.skill_id",
            )?;
            let rows = stmt.query_map([], |row| {
                let recorded: Option<String> = row.get(5)?;
                let summary = EvaluationSummary {
                    skill_id: row.get(0)?,
                    evaluation_id: row.get(1)?,
                    grade: row.get(2)?,
                    score_value: row.get(3)?,
                    score_max: row.get(4)?,
                    stale: false,
                    finished_at: row.get(6)?,
                };
                let location = SkillLocation {
                    central_path: row.get(7)?,
                    db_hash: row.get(8)?,
                };
                Ok((summary, recorded, location))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut skills = HashMap::new();
        let mut summaries = Vec::with_capacity(rows.len());
        for (summary, recorded, location) in rows {
            skills.insert(summary.skill_id.clone(), location);
            summaries.push((summary, recorded));
        }
        let current = live_hashes(skills);
        Ok(summaries
            .into_iter()
            .map(|(mut summary, recorded)| {
                summary.stale = is_stale(&recorded, current.get(&summary.skill_id));
                summary
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::evaluation::{EvaluationResult, Score};
    use crate::core::skill_store::SkillRecord;
    use serde_json::Map;

    fn skill(id: &str, hash: &str) -> SkillRecord {
        SkillRecord {
            id: id.into(),
            name: id.into(),
            description: None,
            source_type: "local".into(),
            source_ref: None,
            source_ref_resolved: None,
            source_subpath: None,
            source_branch: None,
            source_revision: None,
            remote_revision: None,
            central_path: format!("/library/{id}"),
            content_hash: Some(hash.into()),
            enabled: true,
            created_at: 0,
            updated_at: 0,
            status: "ok".into(),
            update_status: "local_only".into(),
            last_checked_at: None,
            last_check_error: None,
        }
    }

    fn evaluation(
        id: &str,
        skill_id: &str,
        hash: &str,
        started_at: i64,
        status: EvaluationStatus,
        grade: &str,
    ) -> EvaluationRecord {
        let result = (status == EvaluationStatus::Succeeded).then(|| EvaluationResult {
            schema_version: 1,
            verdict: "ok".into(),
            score: Some(Score {
                value: 90.0,
                max: 120.0,
            }),
            grade: Some(grade.into()),
            dimensions: vec![],
            findings: vec![],
            report_markdown: "# r".into(),
            extra: Map::new(),
        });
        EvaluationRecord {
            id: id.into(),
            skill_id: skill_id.into(),
            skill_content_hash: Some(hash.into()),
            evaluator: "skill-judge".into(),
            evaluator_content_hash: Some("judge".into()),
            agent: "claude_code".into(),
            model: None,
            status,
            started_at,
            finished_at: started_at + 10,
            duration_ms: 10,
            cost_usd: Some(1.0),
            report_md: result.as_ref().map(|r| r.report_markdown.clone()),
            result,
            error: (status != EvaluationStatus::Succeeded).then(|| "boom".to_string()),
            stale: false,
        }
    }

    fn user_version(conn: &Connection) -> u32 {
        conn.pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn schema_creation_is_idempotent_and_leaves_user_version_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("test.db");
        let store = SkillStore::new(&db).unwrap();
        store.insert_skill(&skill("a", "h1")).unwrap();
        store
            .insert_evaluation(&evaluation(
                "e1",
                "a",
                "h1",
                1,
                EvaluationStatus::Succeeded,
                "A",
            ))
            .unwrap();
        drop(store);

        let conn = Connection::open(&db).unwrap();
        let version = user_version(&conn);
        assert_eq!(
            version, 9,
            "the fork must not bump upstream's schema version"
        );
        ensure_fork_schema(&conn).unwrap();
        ensure_fork_schema(&conn).unwrap();
        assert_eq!(user_version(&conn), version);
        drop(conn);

        // Reopening runs it again; the stored run is still there.
        let store = SkillStore::new(&db).unwrap();
        assert_eq!(store.list_evaluations("a").unwrap().len(), 1);
    }

    #[test]
    fn evaluations_survive_upsert_and_turn_stale_when_the_content_hash_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("test.db")).unwrap();
        store.insert_skill(&skill("a", "h1")).unwrap();
        store
            .insert_evaluation(&evaluation(
                "e1",
                "a",
                "h1",
                1,
                EvaluationStatus::Succeeded,
                "A",
            ))
            .unwrap();
        assert!(!store.list_evaluations("a").unwrap()[0].stale);

        // The per-launch reindex upserts every skill.
        store.upsert_skill(&skill("a", "h1")).unwrap();
        let rows = store.list_evaluations("a").unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].stale);

        // An edit changes the hash the reindex writes.
        store.upsert_skill(&skill("a", "h2")).unwrap();
        let rows = store.list_evaluations("a").unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].stale);
        assert!(store.latest_evaluation_summaries().unwrap()[0].stale);
        assert!(store.get_evaluation("e1").unwrap().unwrap().stale);
    }

    /// A skill whose directory exists, with the DB hash the reindex would
    /// write for it.
    fn skill_on_disk(root: &Path, id: &str) -> SkillRecord {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), format!("---\nname: {id}\n---\n")).unwrap();
        let hash = content_hash::hash_directory(&dir).unwrap();
        SkillRecord {
            central_path: dir.to_string_lossy().to_string(),
            ..skill(id, &hash)
        }
    }

    fn files_read() -> usize {
        content_hash::FILES_READ.with(|count| count.get())
    }

    #[test]
    fn an_on_disk_edit_turns_evaluations_stale_without_a_db_hash_update() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("test.db")).unwrap();
        let record = skill_on_disk(tmp.path(), "a");
        let db_hash = record.content_hash.clone().unwrap();
        store.insert_skill(&record).unwrap();
        store
            .insert_evaluation(&evaluation(
                "e1",
                "a",
                &db_hash,
                1,
                EvaluationStatus::Succeeded,
                "A",
            ))
            .unwrap();

        // Unedited: the live hash agrees with the one stored with the run.
        assert!(!store.list_evaluations("a").unwrap()[0].stale);
        assert!(!store.latest_evaluation_summaries().unwrap()[0].stale);
        assert!(!store.get_evaluation("e1").unwrap().unwrap().stale);

        std::fs::write(Path::new(&record.central_path).join("SKILL.md"), "edited").unwrap();

        assert!(store.list_evaluations("a").unwrap()[0].stale);
        assert!(store.latest_evaluation_summaries().unwrap()[0].stale);
        assert!(store.get_evaluation("e1").unwrap().unwrap().stale);
        // Read-only: the DB hash is still the one from before the edit.
        let row = store.get_skill_by_id("a").unwrap().unwrap();
        assert_eq!(row.content_hash.as_deref(), Some(db_hash.as_str()));
    }

    #[test]
    fn a_missing_skill_directory_falls_back_to_the_db_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("test.db")).unwrap();
        for (id, db_hash) in [("same", "h1"), ("changed", "h2")] {
            store
                .insert_skill(&SkillRecord {
                    central_path: tmp.path().join(id).to_string_lossy().to_string(),
                    ..skill(id, db_hash)
                })
                .unwrap();
        }
        store
            .insert_evaluation(&evaluation(
                "e1",
                "same",
                "h1",
                1,
                EvaluationStatus::Succeeded,
                "A",
            ))
            .unwrap();
        store
            .insert_evaluation(&evaluation(
                "e2",
                "changed",
                "h1",
                2,
                EvaluationStatus::Succeeded,
                "A",
            ))
            .unwrap();

        // A missing directory would hash as an empty one; the DB hash is used.
        assert!(!store.list_evaluations("same").unwrap()[0].stale);
        assert!(store.list_evaluations("changed").unwrap()[0].stale);
        let summaries = store.latest_evaluation_summaries().unwrap();
        let stale: Vec<_> = summaries
            .iter()
            .map(|s| (s.skill_id.as_str(), s.stale))
            .collect();
        assert_eq!(stale, [("changed", true), ("same", false)]);
    }

    #[test]
    fn each_evaluated_skill_is_hashed_once_per_call_and_others_not_at_all() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("test.db")).unwrap();
        let record = skill_on_disk(tmp.path(), "a");
        let hash = record.content_hash.clone().unwrap();
        store.insert_skill(&record).unwrap();
        store
            .insert_skill(&skill_on_disk(tmp.path(), "never"))
            .unwrap();
        for (id, started_at) in [("e1", 1), ("e2", 2), ("e3", 3)] {
            store
                .insert_evaluation(&evaluation(
                    id,
                    "a",
                    &hash,
                    started_at,
                    EvaluationStatus::Succeeded,
                    "A",
                ))
                .unwrap();
        }

        // Each skill directory here holds one file.
        let before = files_read();
        assert_eq!(store.list_evaluations("a").unwrap().len(), 3);
        assert_eq!(files_read() - before, 1);

        let before = files_read();
        assert_eq!(store.latest_evaluation_summaries().unwrap().len(), 1);
        assert_eq!(files_read() - before, 1, "only evaluated skills are hashed");
    }

    #[test]
    fn deleting_a_skill_deletes_its_evaluations() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("test.db")).unwrap();
        store.insert_skill(&skill("a", "h1")).unwrap();
        store
            .insert_evaluation(&evaluation(
                "e1",
                "a",
                "h1",
                1,
                EvaluationStatus::Succeeded,
                "A",
            ))
            .unwrap();
        store.delete_skill("a").unwrap();
        assert!(store.get_evaluation("e1").unwrap().is_none());
    }

    #[test]
    fn history_is_newest_first_and_summaries_take_the_latest_success() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("test.db")).unwrap();
        store.insert_skill(&skill("a", "h1")).unwrap();
        store.insert_skill(&skill("b", "h1")).unwrap();
        store.insert_skill(&skill("never", "h1")).unwrap();
        for record in [
            evaluation("e1", "a", "h1", 1, EvaluationStatus::Succeeded, "C"),
            evaluation("e3", "a", "h1", 3, EvaluationStatus::Failed, ""),
            evaluation("e2", "a", "h1", 2, EvaluationStatus::Succeeded, "B"),
            evaluation("e4", "b", "h0", 4, EvaluationStatus::Succeeded, "A"),
            evaluation("e5", "never", "h1", 5, EvaluationStatus::Cancelled, ""),
        ] {
            store.insert_evaluation(&record).unwrap();
        }

        let ids: Vec<_> = store
            .list_evaluations("a")
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, ["e3", "e2", "e1"]);
        let failed = store.get_evaluation("e3").unwrap().unwrap();
        assert_eq!(failed.status, EvaluationStatus::Failed);
        assert_eq!(failed.error.as_deref(), Some("boom"));

        let summaries = store.latest_evaluation_summaries().unwrap();
        assert_eq!(summaries.len(), 2, "{summaries:?}");
        assert_eq!(summaries[0].skill_id, "a");
        assert_eq!(summaries[0].evaluation_id, "e2");
        assert_eq!(summaries[0].grade.as_deref(), Some("B"));
        assert_eq!(summaries[0].score_value, Some(90.0));
        assert_eq!(summaries[0].score_max, Some(120.0));
        assert!(!summaries[0].stale);
        assert_eq!(summaries[1].skill_id, "b");
        assert!(summaries[1].stale);
    }
}
