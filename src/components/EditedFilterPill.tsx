import { PencilLine } from "lucide-react";
import { useTranslation } from "react-i18next";
import { cn } from "../utils";

/** The Library's "Edited" filter pill (sm-69q.24), styled like the card's
 *  Edited marker. Hidden while no skill is edited, except when it is on: a
 *  filter must never hide the control that turns it off. */
export function EditedFilterPill({
  count,
  active,
  onToggle,
}: {
  count: number;
  active: boolean;
  onToggle: () => void;
}) {
  const { t } = useTranslation();
  if (count === 0 && !active) return null;

  return (
    <>
      <span aria-hidden="true" className="mx-0.5 h-3 w-px bg-border-subtle" />
      <button
        type="button"
        data-testid="edited-filter-pill"
        onClick={onToggle}
        aria-pressed={active}
        title={t("mySkills.editedFilter.title")}
        className={cn(
          "inline-flex items-center gap-1 rounded-full px-2.5 py-0.5 text-[12px] font-medium transition-colors",
          active
            ? "bg-violet-600 text-white dark:bg-violet-500 dark:text-white"
            : "bg-violet-500/10 text-violet-600 hover:bg-violet-500/20 dark:text-violet-400",
        )}
      >
        <PencilLine className="h-3 w-3" />
        {t("mySkills.editedFilter.label")}
        <span className={cn("text-[11px]", active ? "text-white/80" : "opacity-70")}>{count}</span>
      </button>
    </>
  );
}
