import { invoke } from "@tauri-apps/api/core";

// Mirrors src-tauri/src/core/orphaned_folders.rs (sm-69q.16). Field names are
// snake_case, as serde writes them; `Option` fields are `| null`.

/**
 * `unmanaged`: no record points at it, and it can be adopted. `staged` /
 * `backup`: a hidden copy an interrupted install or update left, which startup
 * neither put back nor removed (sm-69q.23); never adopted.
 */
export type OrphanedFolderKind = "unmanaged" | "staged" | "backup";

/** A top-level library folder that no skill record points at. */
export interface OrphanedFolder {
  /** The folder's name at the library root; what adopting takes. */
  dir_name: string;
  path: string;
  /** Only a folder with a SKILL.md can be adopted. */
  has_skill_md: boolean;
  /** The name it would get (SKILL.md's `name:`, else the folder name). */
  name: string | null;
  /** Bytes of the files under it. */
  size: number;
  /** Newest modification time under it, Unix milliseconds. */
  modified: number | null;
  kind: OrphanedFolderKind;
  /** For a staged or backup folder: what it is and what to do with it (English). */
  note: string | null;
}

export interface AdoptedFolder {
  skill_id: string;
  name: string;
  dir_name: string;
  central_path: string;
}

/** Read-only: walks the library root. */
export const getOrphanedFolders = () => invoke<OrphanedFolder[]>("get_orphaned_folders");

/** Gives the folder a record in place, as a new local skill. Its files are not changed. */
export const adoptOrphanedFolder = (dirName: string) =>
  invoke<AdoptedFolder>("adopt_orphaned_folder", { dirName });
