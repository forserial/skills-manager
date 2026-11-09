import { useTranslation } from "react-i18next";
import { cn } from "../utils";
import type { EvaluationSummary } from "../lib/evaluationApi";
import { gradeBadgeProps, type GradeTone } from "../lib/evaluation";

const TONE_CLASS: Record<GradeTone, string> = {
  excellent: "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400",
  good: "bg-sky-500/10 text-sky-600 dark:text-sky-400",
  fair: "bg-amber-500/12 text-amber-600 dark:text-amber-400",
  poor: "bg-red-500/10 text-red-600 dark:text-red-400",
  neutral: "bg-surface-hover text-muted",
};

/** The latest evaluation grade of a skill, as a pill for skill cards. Renders
 *  nothing without a summary; dimmed once the skill changed since. */
export function EvaluationGradeBadge({ summary }: { summary: EvaluationSummary | undefined }) {
  const { t } = useTranslation();
  const badge = gradeBadgeProps(summary);
  if (!badge) return null;

  const title = [
    badge.grade && t("evaluation.badge.grade", { grade: badge.grade }),
    badge.score && t("evaluation.badge.score", { score: badge.score }),
    t("evaluation.badge.evaluatedAt", { date: new Date(badge.finishedAt).toLocaleString() }),
    badge.dimmed && t("evaluation.badge.stale"),
  ]
    .filter(Boolean)
    .join("\n");

  return (
    <span
      title={title}
      aria-label={title}
      data-stale={badge.dimmed || undefined}
      className={cn(
        "inline-flex shrink-0 items-center rounded-full px-2 py-0.5 text-[11px] font-semibold",
        TONE_CLASS[badge.tone],
        badge.dimmed && "opacity-50",
      )}
    >
      {badge.label}
    </span>
  );
}
