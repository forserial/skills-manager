import { useState } from "react";
import { Loader2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { NewWatchInput } from "../lib/watchedUpstreamsApi";

const inputClass =
  "w-full bg-background border border-border-subtle rounded-lg px-3 py-1.5 text-[13px] text-secondary focus:outline-none focus:border-border transition-all placeholder-faint";
const labelClass = "mb-1 block text-[12px] font-medium text-tertiary";

/** "Borrowed from" → Add (sm-69q.13): a git location to watch, with optional
 *  branch, subpath and note. The remote's current head counts as seen, so the
 *  remote has to be reachable. `onAdd` rejects to keep the form open; the
 *  caller reports the error. */
export function WatchedUpstreamAddForm({
  onAdd,
  onCancel,
}: {
  onAdd: (input: NewWatchInput) => Promise<void>;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const [gitUrl, setGitUrl] = useState("");
  const [branch, setBranch] = useState("");
  const [subpath, setSubpath] = useState("");
  const [note, setNote] = useState("");
  const [saving, setSaving] = useState(false);

  const canAdd = gitUrl.trim().length > 0 && !saving;

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!canAdd) return;
    setSaving(true);
    try {
      await onAdd({
        gitUrl: gitUrl.trim(),
        branch: branch.trim(),
        subpath: subpath.trim(),
        note: note.trim(),
      });
    } catch {
      // Reported by the caller; keep what was typed.
    } finally {
      setSaving(false);
    }
  };

  return (
    <form
      data-testid="borrowed-add-form"
      onSubmit={handleSubmit}
      className="space-y-2.5 border-t border-border-subtle px-4 py-3"
    >
      <div>
        <label htmlFor="borrowed-url" className={labelClass}>
          {t("borrowed.urlLabel")}
        </label>
        <input
          id="borrowed-url"
          type="text"
          value={gitUrl}
          onChange={(e) => setGitUrl(e.target.value)}
          placeholder="https://github.com/owner/repo"
          className={`${inputClass} font-mono`}
          autoFocus
        />
      </div>
      <div className="grid grid-cols-2 gap-2.5">
        <div>
          <label htmlFor="borrowed-branch" className={labelClass}>
            {t("borrowed.branchLabel")}
          </label>
          <input
            id="borrowed-branch"
            type="text"
            value={branch}
            onChange={(e) => setBranch(e.target.value)}
            placeholder={t("borrowed.optional")}
            className={`${inputClass} font-mono`}
          />
        </div>
        <div>
          <label htmlFor="borrowed-subpath" className={labelClass}>
            {t("borrowed.subpathLabel")}
          </label>
          <input
            id="borrowed-subpath"
            type="text"
            value={subpath}
            onChange={(e) => setSubpath(e.target.value)}
            placeholder={t("borrowed.optional")}
            className={`${inputClass} font-mono`}
          />
        </div>
      </div>
      <div>
        <label htmlFor="borrowed-note" className={labelClass}>
          {t("borrowed.noteLabel")}
        </label>
        <input
          id="borrowed-note"
          type="text"
          value={note}
          onChange={(e) => setNote(e.target.value)}
          placeholder={t("borrowed.optional")}
          className={inputClass}
        />
      </div>
      <p className="text-[12px] text-muted">{t("borrowed.addHint")}</p>
      <div className="flex justify-end gap-2">
        <button
          type="button"
          onClick={onCancel}
          className="rounded-lg px-3 py-1.5 text-[13px] font-medium text-tertiary transition-colors outline-none hover:bg-surface-hover hover:text-secondary"
        >
          {t("common.cancel")}
        </button>
        <button
          type="submit"
          disabled={!canAdd}
          className="inline-flex items-center gap-1.5 rounded-lg border border-accent-border bg-accent-dark px-3 py-1.5 text-[13px] font-medium text-white transition-colors outline-none hover:bg-accent disabled:cursor-not-allowed disabled:opacity-50"
        >
          {saving && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
          {t("borrowed.addConfirm")}
        </button>
      </div>
    </form>
  );
}
