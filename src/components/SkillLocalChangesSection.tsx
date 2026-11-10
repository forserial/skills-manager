import { useEffect, useState } from "react";
import { FileDiff, HelpCircle, Loader2, PencilLine } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import type { ManagedSkill } from "../lib/tauri";
import { getErrorMessage } from "../lib/error";
import { cn } from "../utils";
import {
  getSkillLocalChanges,
  getSkillLocalDiff,
  type FileChangeState,
  type LocalChanges,
  type LocalDiff,
} from "../lib/localChangesApi";
import { SkillSourceDiffViewer } from "./SkillSourceDiffViewer";

const pillClass =
  "inline-flex shrink-0 items-center gap-1.5 rounded-full border border-border-subtle bg-bg-secondary px-2 py-1 text-[12px] text-muted";

const STATE_TONE: Record<FileChangeState, string> = {
  modified: "bg-sky-500/10 text-sky-600 dark:text-sky-400",
  added: "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400",
  deleted: "bg-red-500/10 text-red-600 dark:text-red-400",
};

/** "Your changes" (sm-69q.3): the files of a git skill that differ from its
 *  baseline, with a diff on demand. Hidden for local skills and for skills
 *  with no local changes. */
export function SkillLocalChangesSection({ skill }: { skill: ManagedSkill }) {
  const { t } = useTranslation();
  const [changes, setChanges] = useState<LocalChanges | null>(null);
  const [diff, setDiff] = useState<LocalDiff | null>(null);
  const [diffOpen, setDiffOpen] = useState(false);
  const [loadingDiff, setLoadingDiff] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setDiff(null);
    setDiffOpen(false);
    getSkillLocalChanges(skill.id)
      .then((result) => {
        if (!cancelled) setChanges(result);
      })
      .catch((error) => {
        if (cancelled) return;
        console.error("Failed to load local changes:", error);
        setChanges(null);
      });
    return () => {
      cancelled = true;
    };
  }, [skill.id, skill.updated_at]);

  if (!changes || changes.state === "clean" || changes.state === "untracked") return null;

  if (changes.state === "baseline_unknown" || changes.state === "baseline_suspect") {
    return (
      <section
        data-testid="local-changes-unknown"
        className="mb-4 flex items-center gap-2 rounded-xl border border-border-subtle bg-surface/70 px-4 py-3 text-[12px] text-muted"
      >
        <HelpCircle className="h-3.5 w-3.5 shrink-0" />
        <span>
          {changes.state === "baseline_suspect"
            ? t("localChanges.suspectHint")
            : t("localChanges.unknownHint")}
        </span>
      </section>
    );
  }

  const handleToggleDiff = async () => {
    if (diffOpen) {
      setDiffOpen(false);
      return;
    }
    setDiffOpen(true);
    if (diff) return;
    setLoadingDiff(true);
    try {
      setDiff(await getSkillLocalDiff(skill.id));
    } catch (error) {
      toast.error(getErrorMessage(error, t("common.error")));
      setDiffOpen(false);
    } finally {
      setLoadingDiff(false);
    }
  };

  return (
    <section className="mb-4 rounded-xl border border-border-subtle bg-surface/70">
      <div className="flex items-center justify-between gap-3 px-4 py-3">
        <span className="flex min-w-0 items-center gap-2">
          <span className={pillClass}>
            <PencilLine className="h-3.5 w-3.5" />
            {changes.files.length}
          </span>
          <span className="truncate text-[13px] font-medium text-secondary">
            {t("localChanges.title")}
          </span>
        </span>
        <button
          type="button"
          onClick={handleToggleDiff}
          disabled={loadingDiff}
          className="inline-flex shrink-0 items-center gap-1.5 rounded-lg px-2.5 py-1 text-[12px] font-medium text-muted transition-colors hover:bg-surface-hover hover:text-secondary disabled:opacity-50"
        >
          {loadingDiff ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
          ) : (
            <FileDiff className="h-3.5 w-3.5" />
          )}
          {diffOpen ? t("localChanges.hideDiff") : t("localChanges.showDiff")}
        </button>
      </div>
      <ul className="divide-y divide-border-subtle border-t border-border-subtle">
        {changes.files.map((file) => (
          <li key={file.path} className="flex min-w-0 items-center gap-2 px-4 py-2">
            <span
              className={cn(
                "inline-flex shrink-0 items-center rounded-full px-2 py-0.5 text-[11px] font-medium",
                STATE_TONE[file.state],
              )}
            >
              {t(`localChanges.state.${file.state}`)}
            </span>
            <span className="truncate font-mono text-[13px] text-secondary" title={file.path}>
              {file.path}
            </span>
          </li>
        ))}
      </ul>
      {diffOpen && diff && (
        <div className="border-t border-border-subtle px-4 py-3">
          {diff.entries ? (
            <SkillSourceDiffViewer entries={diff.entries} />
          ) : (
            <p className="text-[12px] text-muted">{t("localChanges.diffUnavailable")}</p>
          )}
        </div>
      )}
    </section>
  );
}
