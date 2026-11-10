import { GitFork } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { WatchedMarker } from "../lib/watchedUpstreamsApi";

/** A skill card's "Upstream moved" marker (sm-69q.13): shown when a location
 *  the skill watches has moved since it was last seen. Renders nothing for a
 *  skill that watches nothing, or whose watches are all unchanged. */
export function WatchedUpstreamMarker({ marker }: { marker: WatchedMarker | undefined }) {
  const { t } = useTranslation();
  if (!marker || marker.moved_count === 0) return null;

  const title = t("borrowed.markerTitle", { count: marker.moved_count });
  return (
    <span
      data-testid="watched-upstream-marker"
      title={title}
      aria-label={title}
      className="inline-flex shrink-0 items-center gap-1 rounded-full bg-amber-500/10 px-2 py-0.5 text-[11px] font-medium text-amber-600 dark:text-amber-400"
    >
      <GitFork className="h-3 w-3" />
      {t("borrowed.marker")}
    </span>
  );
}
