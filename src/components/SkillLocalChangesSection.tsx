import { useEffect, useState, type KeyboardEvent } from "react";
import {
  Download,
  FileDiff,
  HelpCircle,
  Loader2,
  MessageSquarePlus,
  MessageSquareText,
  PencilLine,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { save as dialogSave } from "@tauri-apps/plugin-dialog";
import type { ManagedSkill } from "../lib/tauri";
import { getErrorMessage } from "../lib/error";
import { cn } from "../utils";
import { getSkillLocalDiff, type FileChangeState, type LocalDiff } from "../lib/localChangesApi";
import {
  MAX_CHANGE_NOTE_CHARS,
  exportSkillLocalPatch,
  getSkillChangeNotes,
  setSkillChangeNote,
  type ChangesReport,
  type NotedChange,
} from "../lib/changeNotesApi";
import { SkillSourceDiffViewer } from "./SkillSourceDiffViewer";

const pillClass =
  "inline-flex shrink-0 items-center gap-1.5 rounded-full border border-border-subtle bg-bg-secondary px-2 py-1 text-[12px] text-muted";

const actionClass =
  "inline-flex shrink-0 items-center gap-1.5 rounded-lg px-2.5 py-1 text-[12px] font-medium text-muted transition-colors hover:bg-surface-hover hover:text-secondary disabled:opacity-50";

const STATE_TONE: Record<FileChangeState, string> = {
  modified: "bg-sky-500/10 text-sky-600 dark:text-sky-400",
  added: "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400",
  deleted: "bg-red-500/10 text-red-600 dark:text-red-400",
};

/** "Your changes" (sm-69q.3): the files of a git skill that differ from its
 *  baseline, with a diff on demand. Each file can carry a one-line note, and
 *  the whole set exports as a patch (sm-69q.8). Hidden for local skills and
 *  for skills with no local changes. */
export function SkillLocalChangesSection({ skill }: { skill: ManagedSkill }) {
  const { t } = useTranslation();
  const [changes, setChanges] = useState<ChangesReport | null>(null);
  const [diff, setDiff] = useState<LocalDiff | null>(null);
  const [diffOpen, setDiffOpen] = useState(false);
  const [loadingDiff, setLoadingDiff] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [savingNote, setSavingNote] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setDiff(null);
    setDiffOpen(false);
    setEditing(null);
    getSkillChangeNotes(skill.id)
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

  const handleExport = async () => {
    const path = await dialogSave({
      defaultPath: `${skill.name}-local-changes.patch`,
      filters: [{ name: t("localChanges.exportFilter"), extensions: ["patch", "diff"] }],
    });
    if (!path) return;
    setExporting(true);
    try {
      const result = await exportSkillLocalPatch(skill.id, path);
      toast.success(t("localChanges.exported", { count: result.file_count, path }));
    } catch (error) {
      toast.error(getErrorMessage(error, t("common.error")));
    } finally {
      setExporting(false);
    }
  };

  const startEditing = (file: NotedChange) => {
    setEditing(file.path);
    setDraft(file.note ?? "");
  };

  const saveNote = async (path: string) => {
    setSavingNote(true);
    try {
      const note = draft.trim();
      setChanges(await setSkillChangeNote(skill.id, path, note === "" ? null : note));
      setEditing(null);
    } catch (error) {
      toast.error(getErrorMessage(error, t("common.error")));
    } finally {
      setSavingNote(false);
    }
  };

  const handleNoteKey = (event: KeyboardEvent<HTMLInputElement>, path: string) => {
    if (event.key === "Enter") {
      event.preventDefault();
      void saveNote(path);
    } else if (event.key === "Escape") {
      event.preventDefault();
      setEditing(null);
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
        <span className="flex shrink-0 items-center gap-1">
          <button type="button" onClick={handleExport} disabled={exporting} className={actionClass}>
            {exporting ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <Download className="h-3.5 w-3.5" />
            )}
            {t("localChanges.exportPatch")}
          </button>
          <button
            type="button"
            onClick={handleToggleDiff}
            disabled={loadingDiff}
            className={actionClass}
          >
            {loadingDiff ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <FileDiff className="h-3.5 w-3.5" />
            )}
            {diffOpen ? t("localChanges.hideDiff") : t("localChanges.showDiff")}
          </button>
        </span>
      </div>
      <ul className="divide-y divide-border-subtle border-t border-border-subtle">
        {changes.files.map((file) => (
          <li key={file.path} className="px-4 py-2">
            <div className="flex min-w-0 items-center gap-2">
              <span
                className={cn(
                  "inline-flex shrink-0 items-center rounded-full px-2 py-0.5 text-[11px] font-medium",
                  STATE_TONE[file.state],
                )}
              >
                {t(`localChanges.state.${file.state}`)}
              </span>
              <span
                className="min-w-0 flex-1 truncate font-mono text-[13px] text-secondary"
                title={file.path}
              >
                {file.path}
              </span>
              {editing !== file.path && (
                <button
                  type="button"
                  data-testid="change-note-edit"
                  onClick={() => startEditing(file)}
                  aria-label={file.note ? t("localChanges.editNote") : t("localChanges.addNote")}
                  title={file.note ? t("localChanges.editNote") : t("localChanges.addNote")}
                  className="shrink-0 rounded-md p-1 text-muted transition-colors hover:bg-surface-hover hover:text-secondary"
                >
                  {file.note ? (
                    <PencilLine className="h-3.5 w-3.5" />
                  ) : (
                    <MessageSquarePlus className="h-3.5 w-3.5" />
                  )}
                </button>
              )}
            </div>
            {editing === file.path ? (
              <div className="mt-2">
                <input
                  type="text"
                  autoFocus
                  data-testid="change-note-input"
                  value={draft}
                  maxLength={MAX_CHANGE_NOTE_CHARS}
                  disabled={savingNote}
                  placeholder={t("localChanges.notePlaceholder")}
                  onChange={(event) => setDraft(event.target.value)}
                  onKeyDown={(event) => handleNoteKey(event, file.path)}
                  className="w-full rounded-lg border border-border-subtle bg-bg-secondary px-2.5 py-1.5 text-[13px] text-secondary outline-none focus:border-accent"
                />
                <div className="mt-1.5 flex items-center justify-between gap-2">
                  <span className="text-[11px] text-muted">{t("localChanges.noteHint")}</span>
                  <span className="flex shrink-0 items-center gap-1">
                    <button
                      type="button"
                      onClick={() => setEditing(null)}
                      disabled={savingNote}
                      className={actionClass}
                    >
                      {t("common.cancel")}
                    </button>
                    <button
                      type="button"
                      onClick={() => void saveNote(file.path)}
                      disabled={savingNote}
                      className={actionClass}
                    >
                      {savingNote && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
                      {t("common.save")}
                    </button>
                  </span>
                </div>
              </div>
            ) : (
              file.note && (
                <p
                  data-testid="change-note"
                  className="mt-1 flex min-w-0 items-start gap-1.5 text-[12px] text-muted"
                >
                  <MessageSquareText className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                  <span className="break-words">{file.note}</span>
                </p>
              )
            )}
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
