import { useEffect, useState } from "react";
import type { ManagedSkill } from "../lib/tauri";
import {
  getWatchedUpstreamMarkers,
  WATCHED_UPSTREAMS_CHANGED,
  type WatchedMarker,
} from "../lib/watchedUpstreamsApi";

/** Skill-card markers for watched upstreams (sm-69q.13), keyed by skill id.
 *  Reloads whenever the skill list does (a check-all refreshes it), when the
 *  window regains focus, and when the detail panel changes a skill's watches. */
export function useWatchedUpstreamMarkers(
  skills: readonly ManagedSkill[],
): Record<string, WatchedMarker> {
  const [markers, setMarkers] = useState<Record<string, WatchedMarker>>({});

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      getWatchedUpstreamMarkers()
        .then((list) => {
          if (!cancelled) setMarkers(Object.fromEntries(list.map((m) => [m.skill_id, m])));
        })
        .catch((error) => console.error("Failed to load watched upstream markers:", error));
    };
    load();
    window.addEventListener("focus", load);
    window.addEventListener(WATCHED_UPSTREAMS_CHANGED, load);
    return () => {
      cancelled = true;
      window.removeEventListener("focus", load);
      window.removeEventListener(WATCHED_UPSTREAMS_CHANGED, load);
    };
  }, [skills]);

  return markers;
}
