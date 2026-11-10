import { useEffect, useState } from "react";
import type { ManagedSkill } from "../lib/tauri";
import { getLocalChangeMarkers, type LocalChangeMarker } from "../lib/localChangesApi";

/** Skill-card markers for local changes (sm-69q.3), keyed by skill id.
 *  Reloads whenever the skill list does, and when the window regains focus,
 *  so an edit made on disk shows (and a reverted one clears) without a
 *  restart. */
export function useLocalChangeMarkers(
  skills: readonly ManagedSkill[],
): Record<string, LocalChangeMarker> {
  const [markers, setMarkers] = useState<Record<string, LocalChangeMarker>>({});

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      getLocalChangeMarkers()
        .then((list) => {
          if (!cancelled) setMarkers(Object.fromEntries(list.map((m) => [m.skill_id, m])));
        })
        .catch((error) => console.error("Failed to load local change markers:", error));
    };
    load();
    window.addEventListener("focus", load);
    return () => {
      cancelled = true;
      window.removeEventListener("focus", load);
    };
  }, [skills]);

  return markers;
}
