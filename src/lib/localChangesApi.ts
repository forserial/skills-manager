import { invoke } from "@tauri-apps/api/core";
import type { SkillSourceDiffEntry } from "./tauri";

// Mirrors src-tauri/src/core/baseline/local_changes.rs (sm-69q.3). Field
// names are snake_case, as serde writes them; `Option` fields are `| null`.

/** Where a skill stands against its baseline (what upstream shipped). */
export type LocalState =
  | "clean"
  | "changed"
  /** A git skill with no baseline yet. */
  | "baseline_unknown"
  /** A git skill pending in a library sync conflict; treat like unknown. */
  | "baseline_suspect"
  /** Not a git or skills.sh skill: no upstream, no baseline. */
  | "untracked";

export type FileChangeState = "modified" | "added" | "deleted";

export interface LocalFileChange {
  path: string;
  state: FileChangeState;
}

export interface LocalChanges {
  skill_id: string;
  state: LocalState;
  baseline_revision: string | null;
  /** Sorted by path; empty unless `state` is "changed". */
  files: LocalFileChange[];
}

/** One skill card's marker. Clean and local skills have none. */
export interface LocalChangeMarker {
  skill_id: string;
  state: LocalState;
  change_count: number | null;
}

export interface LocalDiff extends LocalChanges {
  base_source: "base_store" | "fetch" | null;
  /** Baseline as `original_text`, library copy as `updated_text`. Null when
   *  the baseline content could not be had; `files` still lists the paths. */
  entries: SkillSourceDiffEntry[] | null;
  message: string | null;
}

export const getSkillLocalChanges = (skillId: string) =>
  invoke<LocalChanges>("get_skill_local_changes", { skillId });

export const getLocalChangeMarkers = () =>
  invoke<LocalChangeMarker[]>("get_local_change_markers");

export const getSkillLocalDiff = (skillId: string) =>
  invoke<LocalDiff>("get_skill_local_diff", { skillId });
