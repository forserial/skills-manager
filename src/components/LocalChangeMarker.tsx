import { HelpCircle, PencilLine } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { LocalChangeMarker as Marker } from "../lib/localChangesApi";

/** A skill card's local-change marker (sm-69q.3): "Edited" when files differ
 *  from the baseline, a quieter "Baseline unknown" for a git skill that has
 *  none (or whose baseline is suspect). Renders nothing otherwise. */
export function LocalChangeMarker({ marker }: { marker: Marker | undefined }) {
  const { t } = useTranslation();
  if (!marker) return null;

  if (marker.state === "changed") {
    const title = t("localChanges.markerChangedTitle", { count: marker.change_count ?? 0 });
    return (
      <span
        data-testid="local-change-marker"
        title={title}
        aria-label={title}
        className="inline-flex shrink-0 items-center gap-1 rounded-full bg-violet-500/10 px-2 py-0.5 text-[11px] font-medium text-violet-600 dark:text-violet-400"
      >
        <PencilLine className="h-3 w-3" />
        {t("localChanges.markerChanged")}
      </span>
    );
  }

  if (marker.state === "baseline_unknown" || marker.state === "baseline_suspect") {
    const title =
      marker.state === "baseline_suspect"
        ? t("localChanges.suspectHint")
        : t("localChanges.unknownHint");
    return (
      <span
        data-testid="baseline-unknown-marker"
        title={title}
        aria-label={title}
        className="inline-flex shrink-0 items-center gap-1 rounded-full bg-surface-hover px-2 py-0.5 text-[11px] font-medium text-muted"
      >
        <HelpCircle className="h-3 w-3" />
        {t("localChanges.markerUnknown")}
      </span>
    );
  }

  return null;
}
