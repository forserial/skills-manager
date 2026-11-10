import { invoke } from "@tauri-apps/api/core";

// Mirrors src-tauri/src/core/pinning.rs (sm-69q.5). Field names are
// snake_case, as serde writes them; `Option` fields are `| null`.

/** A pinned skill is checked but never updated until it is unpinned. */
export interface PinState {
  pinned: boolean;
  /** The revision the skill was pinned at. */
  revision: string | null;
  /** Unix milliseconds of the last pin or unpin. */
  pinned_at: number;
  note: string | null;
}

export interface UpstreamCommit {
  sha: string;
  /** Committer date, RFC 3339. */
  date: string;
  author: string;
  subject: string;
}

/** Upstream commits since the baseline that touch the skill's directory. */
export interface UpstreamCommits {
  skill_id: string;
  base_revision: string;
  /** "baseline", or "source_revision" when the skill has no baseline. */
  base_from: string;
  remote_revision: string;
  subpath: string | null;
  /** False when the base revision cannot be found upstream; see `message`. */
  reachable: boolean;
  message: string | null;
  /** Newest first. */
  commits: UpstreamCommit[];
  truncated: boolean;
}

/** The pin while the skill is pinned, else null. No network. */
export const getSkillPin = (skillId: string) =>
  invoke<PinState | null>("get_skill_pin", { skillId });

export const pinSkill = (skillId: string, note?: string | null) =>
  invoke<PinState>("pin_skill", { skillId, note: note ?? null });

export const unpinSkill = (skillId: string) =>
  invoke<PinState>("unpin_skill", { skillId });

/** Fetches upstream history off the library lock. */
export const getSkillUpstreamCommits = (skillId: string) =>
  invoke<UpstreamCommits>("get_skill_upstream_commits", { skillId });

export const isPinnable = (sourceType: string) =>
  sourceType === "git" || sourceType === "skillssh";
