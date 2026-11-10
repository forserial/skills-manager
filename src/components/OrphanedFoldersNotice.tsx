import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, FolderOpen, Loader2, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import type { ManagedSkill } from "../lib/tauri";
import { getErrorMessage } from "../lib/error";
import {
  adoptOrphanedFolder,
  getOrphanedFolders,
  type OrphanedFolder,
} from "../lib/orphanedFoldersApi";

interface Props {
  /** The skill list; the folders are looked up again whenever it changes. */
  skills: readonly ManagedSkill[];
  /** Called after a folder was adopted, to reload the skill list. */
  onAdopted: () => Promise<void> | void;
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * Library folders no skill record points at (sm-69q.16): a notice above the
 * skills list, and a list that adopts one back in place, saying what cannot be
 * recovered. Renders nothing while there are none.
 */
export function OrphanedFoldersNotice({ skills, onAdopted }: Props) {
  const { t } = useTranslation();
  const [folders, setFolders] = useState<OrphanedFolder[]>([]);
  const [open, setOpen] = useState(false);
  const [adopting, setAdopting] = useState<string | null>(null);

  const load = useCallback(() => {
    getOrphanedFolders()
      .then(setFolders)
      .catch((error) => console.error("Failed to list unmanaged library folders:", error));
  }, []);

  useEffect(() => {
    load();
    window.addEventListener("focus", load);
    return () => window.removeEventListener("focus", load);
  }, [load, skills]);

  useEffect(() => {
    if (!open || adopting) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, adopting]);

  if (folders.length === 0) return null;

  const adopt = async (folder: OrphanedFolder) => {
    setAdopting(folder.dir_name);
    try {
      const adopted = await adoptOrphanedFolder(folder.dir_name);
      toast.success(t("orphanedFolders.adopted", { name: adopted.name }));
      const remaining = folders.filter((f) => f.dir_name !== folder.dir_name);
      setFolders(remaining);
      if (remaining.length === 0) setOpen(false);
      await onAdopted();
    } catch (error) {
      toast.error(getErrorMessage(error, t("orphanedFolders.adoptFailed")));
      load();
    } finally {
      setAdopting(null);
    }
  };

  return (
    <>
      <div
        data-testid="orphaned-folders-notice"
        className="mx-1 mb-2 flex items-center gap-2 rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-[13px] text-amber-700 dark:text-amber-300"
      >
        <AlertTriangle className="h-4 w-4 shrink-0" />
        <span className="flex-1">{t("orphanedFolders.notice", { count: folders.length })}</span>
        <button
          type="button"
          onClick={() => setOpen(true)}
          className="rounded-full bg-amber-500/15 px-2.5 py-0.5 text-[12px] font-medium outline-none transition-colors hover:bg-amber-500/25"
        >
          {t("orphanedFolders.review")}
        </button>
      </div>

      {open ? (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div
            className="absolute inset-0 bg-black/70 backdrop-blur-sm"
            onClick={() => !adopting && setOpen(false)}
          />
          <div
            role="dialog"
            aria-label={t("orphanedFolders.title")}
            className="relative bg-surface border border-border rounded-xl w-full max-w-lg p-5 shadow-2xl flex flex-col max-h-[calc(85vh/var(--app-scale))]"
          >
            <div className="flex items-center justify-between mb-3">
              <h2 className="text-[13px] font-semibold text-primary flex items-center gap-2">
                <FolderOpen className="w-4 h-4 text-amber-600 dark:text-amber-400" />
                {t("orphanedFolders.title")}
              </h2>
              <button
                onClick={() => setOpen(false)}
                disabled={!!adopting}
                aria-label={t("common.cancel")}
                className="text-muted hover:text-secondary p-1 rounded-md transition-colors outline-none disabled:opacity-50"
              >
                <X className="w-4 h-4" />
              </button>
            </div>
            <p className="text-[13px] text-tertiary mb-3">{t("orphanedFolders.intro")}</p>
            <div
              data-testid="orphaned-folders-warning"
              className="mb-3 rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-[12px] text-amber-700 dark:text-amber-300 space-y-1"
            >
              <p className="font-medium">{t("orphanedFolders.warningTitle")}</p>
              <p>{t("orphanedFolders.warningLost")}</p>
              <p>{t("orphanedFolders.warningSource")}</p>
            </div>
            <ul className="min-h-0 overflow-y-auto divide-y divide-border-subtle">
              {folders.map((folder) => (
                <li
                  key={folder.dir_name}
                  data-testid="orphaned-folder-row"
                  className="flex items-center gap-3 py-2"
                >
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-[13px] font-medium text-primary" title={folder.path}>
                      {folder.dir_name}
                      {folder.name && folder.name !== folder.dir_name ? (
                        <span className="ml-1.5 font-normal text-muted">({folder.name})</span>
                      ) : null}
                    </p>
                    <p className="text-[12px] text-muted">
                      {formatSize(folder.size)}
                      {folder.modified
                        ? ` · ${new Date(folder.modified).toLocaleString()}`
                        : ""}
                      {folder.kind === "unmanaged" && !folder.has_skill_md
                        ? ` · ${t("orphanedFolders.noSkillMd")}`
                        : ""}
                    </p>
                    {folder.kind !== "unmanaged" ? (
                      <p data-testid="orphaned-folder-leftover" className="text-[12px] text-tertiary">
                        {t(`orphanedFolders.${folder.kind}`)}
                      </p>
                    ) : null}
                  </div>
                  <button
                    type="button"
                    onClick={() => void adopt(folder)}
                    disabled={folder.kind !== "unmanaged" || !folder.has_skill_md || !!adopting}
                    title={
                      folder.kind !== "unmanaged"
                        ? (folder.note ?? undefined)
                        : folder.has_skill_md
                          ? undefined
                          : t("orphanedFolders.noSkillMdHint")
                    }
                    className="inline-flex items-center gap-1 px-3 py-1.5 rounded-lg bg-accent-dark hover:bg-accent text-white text-[13px] font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed border border-accent-border outline-none"
                  >
                    {adopting === folder.dir_name ? (
                      <Loader2 className="h-3.5 w-3.5 animate-spin" />
                    ) : null}
                    {t("orphanedFolders.adopt")}
                  </button>
                </li>
              ))}
            </ul>
          </div>
        </div>
      ) : null}
    </>
  );
}
