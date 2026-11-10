import { invoke } from "@tauri-apps/api/core";

// Mirrors src-tauri/src/core/watched_upstreams.rs (sm-69q.7). Field names are
// snake_case, as serde writes them; `Option` fields are `| null`.

/** A git location a skill borrowed from. Checked for movement, never merged. */
export interface WatchedUpstream {
  id: string;
  git_url: string;
  subpath: string | null;
  branch: string | null;
  last_seen_revision: string | null;
  note: string | null;
  /** Unix milliseconds. */
  added_at: number;
  remote_revision: string | null;
  /** The upstream moved since it was last seen. */
  moved: boolean;
  /** Unix milliseconds. */
  checked_at: number | null;
  check_error: string | null;
}

export interface ForkResult {
  skill_id: string;
  name: string;
  central_path: string;
  forked_from: string;
  watched: WatchedUpstream | null;
}

/** As stored; no network. */
export const getWatchedUpstreams = (skillId: string) =>
  invoke<WatchedUpstream[]>("get_watched_upstreams", { skillId });

/** Resolves every watched upstream's remote revision and stores what moved. */
export const checkWatchedUpstreams = (skillId: string) =>
  invoke<WatchedUpstream[]>("check_watched_upstreams", { skillId });

export const markWatchedUpstreamSeen = (skillId: string, watchId: string) =>
  invoke<WatchedUpstream>("mark_watched_upstream_seen", { skillId, watchId });

/** What the "Borrowed from" form asks for (sm-69q.13). Without a revision,
 *  the remote's current head counts as seen, so adding needs it reachable. */
export interface NewWatchInput {
  gitUrl: string;
  subpath?: string | null;
  branch?: string | null;
  note?: string | null;
}

export const addWatchedUpstream = (skillId: string, input: NewWatchInput) =>
  invoke<WatchedUpstream>("add_watched_upstream", {
    skillId,
    gitUrl: input.gitUrl,
    subpath: input.subpath || null,
    branch: input.branch || null,
    note: input.note || null,
  });

export const removeWatchedUpstream = (skillId: string, watchId: string) =>
  invoke<WatchedUpstream>("remove_watched_upstream", { skillId, watchId });

/** A skill-card marker: a skill that watches something, and how much moved. */
export interface WatchedMarker {
  skill_id: string;
  watch_count: number;
  moved_count: number;
}

/** Every skill that watches something. No network. */
export const getWatchedUpstreamMarkers = () =>
  invoke<WatchedMarker[]>("get_watched_upstream_markers");

/** Dispatched on `window` after the app changes a skill's watches, so card
 *  markers reload without a full skill-list refresh. */
export const WATCHED_UPSTREAMS_CHANGED = "watched-upstreams-changed";
export const notifyWatchedUpstreamsChanged = () =>
  window.dispatchEvent(new Event(WATCHED_UPSTREAMS_CHANGED));

export const forkSkill = (skillId: string, newName: string) =>
  invoke<ForkResult>("fork_skill", { skillId, newName });

export const shortRevision = (revision: string | null | undefined) =>
  revision ? revision.slice(0, 7) : null;

/** Owner/repo of a GitHub clone URL (https or SCP form), else null. */
function githubRepo(gitUrl: string): string | null {
  const match = gitUrl
    .trim()
    .match(/^(?:https?:\/\/github\.com\/|git@github\.com:|ssh:\/\/git@github\.com\/)([^/\s]+\/[^/\s]+?)(?:\.git)?\/?$/i);
  return match ? match[1] : null;
}

/**
 * A page to look at the upstream in a browser: for GitHub, the directory at
 * the branch (or the compare view from the last seen revision when it moved);
 * for other http(s) hosts, the URL itself. Null for ssh, file:// and paths.
 */
export function upstreamWebUrl(watch: WatchedUpstream): string | null {
  const repo = githubRepo(watch.git_url);
  if (repo) {
    const base = `https://github.com/${repo}`;
    if (watch.moved && watch.last_seen_revision && watch.remote_revision) {
      return `${base}/compare/${watch.last_seen_revision}...${watch.remote_revision}`;
    }
    const ref = watch.branch ?? watch.remote_revision ?? watch.last_seen_revision;
    if (!ref) return base;
    const path = watch.subpath ? `/${watch.subpath.replace(/^\/+/, "")}` : "";
    return `${base}/tree/${ref}${path}`;
  }
  return /^https?:\/\//i.test(watch.git_url) ? watch.git_url : null;
}
