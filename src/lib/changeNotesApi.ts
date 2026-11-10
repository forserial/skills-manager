import { invoke } from "@tauri-apps/api/core";
import type { FileChangeState, LocalState } from "./localChangesApi";

// Mirrors src-tauri/src/core/change_notes.rs (sm-69q.8). Field names are
// snake_case, as serde writes them; `Option` fields are `| null`.

/** Longest note the backend keeps, in characters. */
export const MAX_CHANGE_NOTE_CHARS = 500;

export interface ChangeNote {
  path: string;
  note: string;
  /** Unix milliseconds. */
  updated_at: number;
}

/** A changed file with its one-line reason, if it has one. */
export interface NotedChange {
  path: string;
  state: FileChangeState;
  note: string | null;
  note_updated_at: number | null;
}

/** The local changes of a skill with their notes. */
export interface ChangesReport {
  skill_id: string;
  name: string;
  state: LocalState;
  baseline_revision: string | null;
  /** Sorted by path; empty unless `state` is "changed". */
  files: NotedChange[];
  /** Notes kept while the baseline is unknown or suspect. */
  held_notes: ChangeNote[];
}

export interface PatchExport {
  skill_id: string;
  name: string;
  baseline_revision: string;
  base_source: "base_store" | "fetch";
  /** Where the skill sits in its upstream repository; patch paths start with it. */
  path_prefix: string;
  file_count: number;
  noted_file_count: number;
}

export const getSkillChangeNotes = (skillId: string) =>
  invoke<ChangesReport>("get_skill_change_notes", { skillId });

/** Set a changed file's note; an empty or null note clears it. */
export const setSkillChangeNote = (skillId: string, path: string, note: string | null) =>
  invoke<ChangesReport>("set_skill_change_note", { skillId, path, note });

/** Write the skill's local changes as a patch file at `outputPath`. */
export const exportSkillLocalPatch = (skillId: string, outputPath: string) =>
  invoke<PatchExport>("export_skill_local_patch", { skillId, outputPath });
