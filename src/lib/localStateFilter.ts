import type { LocalChangeMarker } from "./localChangesApi";

// The Library's "Edited" filter (sm-69q.24): the GUI side of
// `skills list --modified`. A skill counts as edited when its files differ
// from what upstream shipped. A skill with no baseline (or a suspect one)
// does not: nothing is known about its edits.

/** True when the marker says the skill's files differ from its baseline. */
export function isEdited(marker: LocalChangeMarker | undefined): boolean {
  return marker?.state === "changed";
}

/** How many of `skills` are edited. Counts only skills still in the list, so
 *  a marker that outlives its skill (deleted since the last reload) is not
 *  counted. */
export function countEdited(
  skills: readonly { id: string }[],
  markers: Record<string, LocalChangeMarker>,
): number {
  return skills.reduce((count, skill) => count + (isEdited(markers[skill.id]) ? 1 : 0), 0);
}
