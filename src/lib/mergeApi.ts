import { invoke } from "@tauri-apps/api/core";

// Mirrors src-tauri/src/core/skill_update_merge (sm-69q.6). Field names are
// snake_case, as serde writes them; `Option` fields are `| null`.

/** Why a file of a merged update needs a person. */
export type MergeConflictKind =
  | "text"
  | "binary"
  | "deleted_upstream"
  | "deleted_locally"
  | "added_both"
  | "base_gone"
  | "base_unavailable";

/** Which content a conflicted file takes: keep mine, or take theirs. */
export type MergeTake = "local" | "upstream" | "file";

export interface MergeConflict {
  path: string;
  kind: MergeConflictKind;
  /** SHA-256 of the library's copy, null when the library lacks it. */
  local_sha256: string | null;
  /** SHA-256 of upstream's copy, null when upstream lacks it. */
  upstream_sha256: string | null;
}

/** What merging local changes did, in `UpdateSkillResult.merge`. */
export interface MergeReport {
  target_revision: string;
  baseline_revision: string;
  /** Changed on both sides and merged automatically. */
  merged: string[];
  /** Changed or added locally only, carried over as they are. */
  kept_local: string[];
  /** Conflicts settled by a recorded choice. */
  resolved: string[];
  /** Non-empty means the update was held and nothing changed. */
  conflicts: MergeConflict[];
  base_source: "base_store" | "fetch" | null;
  base_message: string | null;
}

export interface MergeConflictStatus {
  path: string;
  kind: MergeConflictKind;
  description: string;
  resolution: MergeTake | null;
  versions_dir: string;
}

/** A held merge, as `skills merge status` shows it. */
export interface MergeStatus {
  skill_id: string;
  name: string;
  pending: boolean;
  target_revision: string | null;
  baseline_revision: string | null;
  stale: boolean;
  merged: string[];
  conflicts: MergeConflictStatus[];
  unresolved: string[];
}

export interface MergeAbortReport {
  skill_id: string;
  name: string;
  aborted: boolean;
}

/** The pending merge of a skill, if its last update was held over conflicts. */
export const getSkillMerge = (skillId: string) =>
  invoke<MergeStatus>("get_skill_merge", { skillId });

/** Record keep mine / take theirs for one conflicted file. Updating the
 *  skill again applies the merge once every conflict has a choice. */
export const resolveSkillMerge = (skillId: string, path: string, take: "local" | "upstream") =>
  invoke<MergeStatus>("resolve_skill_merge", { skillId, path, take });

/** Drop the pending merge. The library was never touched by it. */
export const abortSkillMerge = (skillId: string) =>
  invoke<MergeAbortReport>("abort_skill_merge", { skillId });

/** Open the folder holding both versions of a conflicted file. */
export const revealSkillMergeVersions = (skillId: string, path: string) =>
  invoke<void>("reveal_skill_merge_versions", { skillId, path });
