//! Tables owned by this fork, kept out of the versioned migrations.
//!
//! `migrations.rs` tracks upstream's `PRAGMA user_version`. A fork-owned
//! version bump would collide with upstream's next migration and make it skip
//! on existing databases, so fork tables are created idempotently here, right
//! after the migrations run, and `user_version` is left alone.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

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

const RECORD_COLUMNS: &str = "e.id, e.skill_id, e.skill_content_hash, e.evaluator,
    e.evaluator_content_hash, e.agent, e.model, e.status, e.started_at, e.finished_at,
    e.duration_ms, e.cost_usd, e.result_json, e.report_md, e.error,
    (e.skill_content_hash IS NOT s.content_hash) AS stale";

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
        stale: row.get(15)?,
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
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RECORD_COLUMNS}
             FROM skill_evaluations e JOIN skills s ON s.id = e.skill_id
             WHERE e.skill_id = ?1
             ORDER BY e.started_at DESC, e.id DESC"
        ))?;
        let rows = stmt.query_map(params![skill_id], map_record)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// One stored run by id.
    pub fn get_evaluation(&self, id: &str) -> Result<Option<EvaluationRecord>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {RECORD_COLUMNS}
                     FROM skill_evaluations e JOIN skills s ON s.id = e.skill_id
                     WHERE e.id = ?1"
                ),
                params![id],
                map_record,
            )
            .optional()?)
    }

    /// Per skill, the latest successful run: grade, score, stale flag.
    /// Skills never evaluated successfully are absent.
    pub fn latest_evaluation_summaries(&self) -> Result<Vec<EvaluationSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT e.skill_id, e.id, e.grade, e.score_value, e.score_max,
                    (e.skill_content_hash IS NOT s.content_hash) AS stale, e.finished_at
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
            Ok(EvaluationSummary {
                skill_id: row.get(0)?,
                evaluation_id: row.get(1)?,
                grade: row.get(2)?,
                score_value: row.get(3)?,
                score_max: row.get(4)?,
                stale: row.get(5)?,
                finished_at: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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
            .insert_evaluation(&evaluation("e1", "a", "h1", 1, EvaluationStatus::Succeeded, "A"))
            .unwrap();
        drop(store);

        let conn = Connection::open(&db).unwrap();
        let version = user_version(&conn);
        assert_eq!(version, 9, "the fork must not bump upstream's schema version");
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
            .insert_evaluation(&evaluation("e1", "a", "h1", 1, EvaluationStatus::Succeeded, "A"))
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

    #[test]
    fn deleting_a_skill_deletes_its_evaluations() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("test.db")).unwrap();
        store.insert_skill(&skill("a", "h1")).unwrap();
        store
            .insert_evaluation(&evaluation("e1", "a", "h1", 1, EvaluationStatus::Succeeded, "A"))
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
