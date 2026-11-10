import { useEffect, useState } from "react";
import { AlertTriangle, Check, ExternalLink, GitFork, Loader2, Plus, RefreshCw, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { ManagedSkill } from "../lib/tauri";
import { getErrorMessage } from "../lib/error";
import {
  addWatchedUpstream,
  checkWatchedUpstreams,
  getWatchedUpstreams,
  markWatchedUpstreamSeen,
  notifyWatchedUpstreamsChanged,
  removeWatchedUpstream,
  shortRevision,
  upstreamWebUrl,
  type NewWatchInput,
  type WatchedUpstream,
} from "../lib/watchedUpstreamsApi";
import { ConfirmDialog } from "./ConfirmDialog";
import { WatchedUpstreamAddForm } from "./WatchedUpstreamAddForm";

const pillClass =
  "inline-flex shrink-0 items-center gap-1.5 rounded-full border border-border-subtle bg-bg-secondary px-2 py-1 text-[12px] text-muted";

/** "Borrowed from": the git locations a skill watches (sm-69q.7). Shows the
 *  stored state at once; "Check" resolves the remotes. "Add" and each row's
 *  "Remove" change the list (sm-69q.13); with nothing watched, only the title,
 *  a hint and "Add" show. */
export function SkillBorrowedFromSection({ skill }: { skill: ManagedSkill }) {
  const { t } = useTranslation();
  const [watches, setWatches] = useState<WatchedUpstream[] | null>(null);
  const [checking, setChecking] = useState(false);
  const [markingId, setMarkingId] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<WatchedUpstream | null>(null);

  useEffect(() => {
    let cancelled = false;
    getWatchedUpstreams(skill.id)
      .then((list) => {
        if (!cancelled) setWatches(list);
      })
      .catch((error) => {
        if (cancelled) return;
        console.error("Failed to load watched upstreams:", error);
        setWatches([]);
      });
    return () => {
      cancelled = true;
    };
    // The record is replaced on every skill-list refresh, e.g. after "Check
    // all", which also checks watched upstreams (sm-69q.13).
  }, [skill]);

  if (!watches) return null;

  const handleCheck = async () => {
    setChecking(true);
    try {
      setWatches(await checkWatchedUpstreams(skill.id));
      notifyWatchedUpstreamsChanged();
    } catch (error) {
      toast.error(getErrorMessage(error, t("common.error")));
    } finally {
      setChecking(false);
    }
  };

  const handleSeen = async (watch: WatchedUpstream) => {
    setMarkingId(watch.id);
    try {
      const updated = await markWatchedUpstreamSeen(skill.id, watch.id);
      setWatches((prev) => (prev ?? []).map((w) => (w.id === updated.id ? updated : w)));
      notifyWatchedUpstreamsChanged();
      toast.success(t("borrowed.markedSeen"));
    } catch (error) {
      toast.error(getErrorMessage(error, t("common.error")));
    } finally {
      setMarkingId(null);
    }
  };

  const handleAdd = async (input: NewWatchInput) => {
    try {
      const added = await addWatchedUpstream(skill.id, input);
      setWatches((prev) => [...(prev ?? []), added]);
      setAdding(false);
      notifyWatchedUpstreamsChanged();
      toast.success(t("borrowed.added"));
    } catch (error) {
      toast.error(getErrorMessage(error, t("common.error")));
      throw error;
    }
  };

  const handleRemove = async () => {
    if (!removing) return;
    try {
      await removeWatchedUpstream(skill.id, removing.id);
      setWatches((prev) => (prev ?? []).filter((w) => w.id !== removing.id));
      notifyWatchedUpstreamsChanged();
      toast.success(t("borrowed.removed"));
    } catch (error) {
      toast.error(getErrorMessage(error, t("common.error")));
    }
  };

  const movedCount = watches.filter((w) => w.moved).length;
  const headerButtonClass =
    "inline-flex shrink-0 items-center gap-1.5 rounded-lg px-2.5 py-1 text-[12px] font-medium text-muted transition-colors hover:bg-surface-hover hover:text-secondary disabled:opacity-50";

  return (
    <section className="mb-4 rounded-xl border border-border-subtle bg-surface/70">
      <div className="flex items-center justify-between gap-3 px-4 py-3">
        <span className="flex min-w-0 items-center gap-2">
          <span className={pillClass}>
            <GitFork className="h-3.5 w-3.5" />
            {watches.length}
          </span>
          <span className="truncate text-[13px] font-medium text-secondary">
            {t("borrowed.title")}
          </span>
          {movedCount > 0 && (
            <span className="inline-flex shrink-0 items-center gap-1 rounded-full bg-amber-500/10 px-2 py-0.5 text-[11px] font-medium text-amber-600 dark:text-amber-400">
              <AlertTriangle className="h-3 w-3" />
              {t("borrowed.movedCount", { count: movedCount })}
            </span>
          )}
        </span>
        <span className="flex shrink-0 items-center gap-1">
          {!adding && (
            <button type="button" onClick={() => setAdding(true)} className={headerButtonClass}>
              <Plus className="h-3.5 w-3.5" />
              {t("borrowed.add")}
            </button>
          )}
          {watches.length > 0 && (
            <button
              type="button"
              onClick={handleCheck}
              disabled={checking}
              className={headerButtonClass}
            >
              {checking ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <RefreshCw className="h-3.5 w-3.5" />
              )}
              {t("borrowed.check")}
            </button>
          )}
        </span>
      </div>
      {watches.length === 0 && !adding && (
        <p className="border-t border-border-subtle px-4 py-2.5 text-[12px] text-muted">
          {t("borrowed.emptyHint")}
        </p>
      )}
      {adding && (
        <WatchedUpstreamAddForm onAdd={handleAdd} onCancel={() => setAdding(false)} />
      )}
      {watches.length > 0 && (
        <ul className="divide-y divide-border-subtle border-t border-border-subtle">
          {watches.map((watch) => (
            <BorrowedRow
              key={watch.id}
              watch={watch}
              marking={markingId === watch.id}
              onSeen={() => handleSeen(watch)}
              onRemove={() => setRemoving(watch)}
            />
          ))}
        </ul>
      )}
      <ConfirmDialog
        open={removing !== null}
        title={t("borrowed.removeTitle")}
        message={t("borrowed.removeMessage", { url: removing?.git_url ?? "" })}
        confirmLabel={t("borrowed.remove")}
        onClose={() => setRemoving(null)}
        onConfirm={handleRemove}
      />
    </section>
  );
}

function BorrowedRow({
  watch,
  marking,
  onSeen,
  onRemove,
}: {
  watch: WatchedUpstream;
  marking: boolean;
  onSeen: () => void;
  onRemove: () => void;
}) {
  const { t } = useTranslation();
  const webUrl = upstreamWebUrl(watch);
  const location = [watch.branch, watch.subpath].filter(Boolean).join(" · ");
  const seen = shortRevision(watch.last_seen_revision);
  const remote = shortRevision(watch.remote_revision);

  return (
    <li className="px-4 py-3">
      <div className="flex min-w-0 items-center gap-2">
        <span className="truncate font-mono text-[13px] text-secondary" title={watch.git_url}>
          {watch.git_url}
        </span>
        {watch.moved ? (
          <span
            data-testid="borrowed-moved"
            className="inline-flex shrink-0 items-center gap-1 rounded-full bg-amber-500/10 px-2 py-0.5 text-[11px] font-medium text-amber-600 dark:text-amber-400"
          >
            <AlertTriangle className="h-3 w-3" />
            {t("borrowed.moved")}
          </span>
        ) : watch.remote_revision ? (
          <span className="inline-flex shrink-0 items-center gap-1 rounded-full bg-surface-hover px-2 py-0.5 text-[11px] font-medium text-muted">
            <Check className="h-3 w-3" />
            {t("borrowed.unchanged")}
          </span>
        ) : null}
      </div>
      {location && <div className="mt-0.5 truncate font-mono text-[12px] text-muted">{location}</div>}
      {watch.note && <div className="mt-1 text-[12px] text-tertiary">{watch.note}</div>}
      <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-[12px] text-muted">
        <span>
          {t("borrowed.lastSeen")}: <span className="font-mono">{seen ?? t("borrowed.unknown")}</span>
          {watch.moved && remote && (
            <>
              {" → "}
              <span className="font-mono">{remote}</span>
            </>
          )}
        </span>
        {watch.check_error && (
          <span className="truncate text-amber-600 dark:text-amber-400" title={watch.check_error}>
            {t("borrowed.checkFailed")}
          </span>
        )}
        <span className="ml-auto flex items-center gap-1">
          {webUrl && (
            <button
              type="button"
              onClick={() => void openUrl(webUrl).catch(() => {})}
              className="inline-flex items-center gap-1 rounded-md px-2 py-0.5 text-[12px] font-medium text-muted transition-colors hover:bg-surface-hover hover:text-secondary"
            >
              <ExternalLink className="h-3 w-3" />
              {t("borrowed.open")}
            </button>
          )}
          {watch.moved && (
            <button
              type="button"
              onClick={onSeen}
              disabled={marking}
              className="inline-flex items-center gap-1 rounded-md px-2 py-0.5 text-[12px] font-medium text-accent transition-colors hover:bg-surface-hover disabled:opacity-50"
            >
              {marking ? <Loader2 className="h-3 w-3 animate-spin" /> : <Check className="h-3 w-3" />}
              {t("borrowed.markSeen")}
            </button>
          )}
          <button
            type="button"
            onClick={onRemove}
            aria-label={t("borrowed.remove")}
            title={t("borrowed.remove")}
            className="inline-flex items-center rounded-md p-1 text-muted transition-colors hover:bg-surface-hover hover:text-secondary"
          >
            <Trash2 className="h-3 w-3" />
          </button>
        </span>
      </div>
    </li>
  );
}
