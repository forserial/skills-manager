import { useEffect, useState } from "react";
import { X, GitMerge, FolderOpen } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { MergeReport } from "../lib/tauri";

/**
 * An update of a locally changed skill that merged with conflicts
 * (sm-69q.6). Nothing in the library has changed: the dialog says how many
 * files merged automatically and how many need a decision, and each
 * conflict offers keep mine, take theirs, or open both versions. Applying
 * records the choices and updates again; cancelling drops the pending merge.
 */
interface Props {
  open: boolean;
  skillName: string;
  report: MergeReport | null;
  onClose: () => void;
  onApply: (choices: Record<string, "local" | "upstream">) => Promise<void>;
  onOpenVersions: (path: string) => void;
}

export function MergeConflictDialog({
  open,
  skillName,
  report,
  onClose,
  onApply,
  onOpenVersions,
}: Props) {
  const { t } = useTranslation();
  const [choices, setChoices] = useState<Record<string, "local" | "upstream">>({});
  const [applying, setApplying] = useState(false);

  // A new hold asks afresh.
  useEffect(() => {
    setChoices({});
  }, [report]);

  useEffect(() => {
    if (!open || applying) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, applying, onClose]);

  if (!open || !report) return null;

  const conflicts = report.conflicts;
  const automatic = report.merged.length + report.resolved.length;
  const decided = conflicts.every((c) => choices[c.path]);

  const choose = (path: string, take: "local" | "upstream") =>
    setChoices((prev) => ({ ...prev, [path]: take }));

  const apply = async () => {
    setApplying(true);
    try {
      await onApply(choices);
    } finally {
      setApplying(false);
    }
  };

  const choiceButton = (path: string, take: "local" | "upstream", label: string) => {
    const selected = choices[path] === take;
    return (
      <button
        type="button"
        aria-pressed={selected}
        onClick={() => choose(path, take)}
        className={
          selected
            ? "px-2.5 py-1 rounded-lg text-[12px] font-medium border border-accent-border bg-accent-dark text-white outline-none"
            : "px-2.5 py-1 rounded-lg text-[12px] font-medium border border-border-subtle text-secondary hover:bg-surface-hover outline-none"
        }
      >
        {label}
      </button>
    );
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div
        className="absolute inset-0 bg-black/70 backdrop-blur-sm"
        onClick={() => !applying && onClose()}
      />
      <div
        role="dialog"
        aria-label={t("mergeUpdate.title")}
        className="relative bg-surface border border-border rounded-xl w-full max-w-md p-5 shadow-2xl flex flex-col max-h-[calc(85vh/var(--app-scale))]"
      >
        <div className="flex items-center justify-between mb-4">
          <h2 className="text-[13px] font-semibold text-primary flex items-center gap-2">
            <GitMerge className="w-4 h-4 text-amber-600 dark:text-amber-400" />
            {t("mergeUpdate.title")}
          </h2>
          <button
            onClick={onClose}
            disabled={applying}
            aria-label={t("common.cancel")}
            className="text-muted hover:text-secondary p-1 rounded-md transition-colors outline-none disabled:opacity-50"
          >
            <X className="w-4 h-4" />
          </button>
        </div>
        <p className="text-[13px] text-tertiary mb-4">
          {t("mergeUpdate.summary", {
            name: skillName,
            merged: automatic,
            count: conflicts.length,
          })}
        </p>
        {report.base_message ? (
          <p className="text-[12px] text-tertiary mb-4">{t("mergeUpdate.baseMissing")}</p>
        ) : null}
        <ul className="mb-5 flex flex-col gap-3 overflow-y-auto min-h-0" aria-label={t("mergeUpdate.conflictsGroup")}>
          {conflicts.map((conflict) => (
            <li
              key={conflict.path}
              data-path={conflict.path}
              className="rounded-lg border border-border-subtle bg-bg-secondary p-3"
            >
              <p className="text-[13px] font-medium text-secondary break-all">{conflict.path}</p>
              <p className="text-[12px] text-tertiary mb-2">
                {t(`mergeUpdate.kind.${conflict.kind}`)}
              </p>
              <div className="flex flex-wrap gap-2">
                {choiceButton(conflict.path, "local", t("mergeUpdate.keepMine"))}
                {choiceButton(conflict.path, "upstream", t("mergeUpdate.takeTheirs"))}
                <button
                  type="button"
                  onClick={() => onOpenVersions(conflict.path)}
                  className="px-2.5 py-1 rounded-lg text-[12px] font-medium text-tertiary hover:text-secondary hover:bg-surface-hover outline-none flex items-center gap-1"
                >
                  <FolderOpen className="w-3.5 h-3.5" />
                  {t("mergeUpdate.openBoth")}
                </button>
              </div>
            </li>
          ))}
        </ul>

        <div className="flex justify-end gap-2">
          <button
            onClick={onClose}
            disabled={applying}
            className="px-3 py-1.5 rounded-lg text-[13px] font-medium text-tertiary hover:text-secondary hover:bg-surface-hover transition-colors outline-none disabled:opacity-50"
          >
            {t("common.cancel")}
          </button>
          <button
            onClick={apply}
            disabled={!decided || applying}
            className="px-3 py-1.5 rounded-lg bg-accent-dark hover:bg-accent text-white text-[13px] font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed border border-accent-border outline-none"
          >
            {applying ? t("common.loading") : t("mergeUpdate.apply")}
          </button>
        </div>
      </div>
    </div>
  );
}
