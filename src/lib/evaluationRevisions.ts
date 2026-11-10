// Pure helpers for evaluation revisions (sm-69q.10): which upstream revision
// a run judged, whether the skill has moved on since, and runs of an incoming
// update kept apart from runs of the library copy.
import type { ManagedSkill } from "./tauri";
import type { EvaluationRecord } from "./evaluationApi";

/** A commit id as the UI shows it. */
export const shortSha = (revision: string | null | undefined): string | null =>
  revision ? revision.slice(0, 7) : null;

export interface RevisionNote {
  /** Short sha of the revision the run judged, when known. */
  evaluatedAt: string | null;
  /** Short sha of where upstream is now, only when it differs. */
  upstreamNow: string | null;
}

/** "evaluated at <sha>" and, once the skill has moved on, "upstream is now
 *  at <sha>". A run without a source revision (older runs, skills cloned on
 *  another device) falls back to the baseline it was measured against. */
export function revisionNote(record: EvaluationRecord): RevisionNote {
  const judged = record.source_revision ?? record.baseline_revision ?? null;
  const upstream = record.upstream_revision ?? null;
  return {
    evaluatedAt: shortSha(judged),
    upstreamNow: judged && upstream && upstream !== judged ? shortSha(upstream) : null,
  };
}

export const isIncomingRun = (record: EvaluationRecord): boolean =>
  record.subject === "incoming";

export interface RunsBySubject {
  /** The newest successful run of the library copy. */
  library: EvaluationRecord | null;
  /** The newest successful run of an update not applied yet, when it is
   *  newer than `library`. Once its update is applied the library holds what
   *  it judged, and it is no longer "incoming" (`stale` turns false). */
  incoming: EvaluationRecord | null;
}

/** `records` newest first, as the backend lists them. */
export function runsBySubject(records: readonly EvaluationRecord[] | null): RunsBySubject {
  const succeeded = (records ?? []).filter((record) => record.status === "succeeded");
  const library =
    succeeded.find((record) => !isIncomingRun(record) || !record.stale) ?? null;
  const incoming =
    succeeded.find((record) => isIncomingRun(record) && record.stale) ?? null;
  const newer =
    incoming && (!library || incoming.started_at > library.started_at) ? incoming : null;
  return { library, incoming: newer };
}

/** Whether "Evaluate update" makes sense: the skill has an upstream and the
 *  last check found a newer version. */
export const canEvaluateIncoming = (skill: Pick<ManagedSkill, "source_type" | "update_status">) =>
  (skill.source_type === "git" || skill.source_type === "skillssh") &&
  skill.update_status === "update_available";
