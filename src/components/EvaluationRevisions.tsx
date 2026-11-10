// Evaluation revisions in the skill detail panel (sm-69q.10): which upstream
// revision a run judged, and an incoming update's result beside the current one.
import { ArrowUpCircle, GitCommitHorizontal } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { EvaluationRecord } from "../lib/evaluationApi";
import { formatScore } from "../lib/evaluation";
import { revisionNote, shortSha } from "../lib/evaluationRevisions";

/** "Evaluated at abc1234 · upstream is now at def5678". Renders nothing when
 *  the run's revision is unknown. */
export function EvaluationRevisionLine({ record }: { record: EvaluationRecord }) {
  const { t } = useTranslation();
  const note = revisionNote(record);
  if (!note.evaluatedAt) return null;
  return (
    <span
      className="inline-flex min-w-0 items-center gap-1 text-[12px] text-muted"
      data-testid="evaluation-revision"
    >
      <GitCommitHorizontal className="h-3.5 w-3.5 shrink-0" />
      <span className="truncate">
        {t("evaluationRevisions.evaluatedAt", { sha: note.evaluatedAt })}
        {note.upstreamNow && (
          <>
            {" · "}
            <span className="text-amber-600 dark:text-amber-400">
              {t("evaluationRevisions.upstreamNow", { sha: note.upstreamNow })}
            </span>
          </>
        )}
      </span>
    </span>
  );
}

/** Starts a run of the version an update would install. */
export function EvaluateUpdateButton({
  onClick,
  disabled,
}: {
  onClick: () => void;
  disabled: boolean;
}) {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      title={t("evaluationRevisions.evaluateUpdateHint")}
      className="app-button-secondary h-[34px] gap-1.5 px-3 py-0"
    >
      <ArrowUpCircle className="h-3.5 w-3.5" />
      {t("evaluationRevisions.evaluateUpdate")}
    </button>
  );
}

function ResultColumn({
  label,
  record,
  emptyText,
}: {
  label: string;
  record: EvaluationRecord | null;
  emptyText: string;
}) {
  const result = record?.result ?? null;
  const score = formatScore(result?.score?.value, result?.score?.max);
  return (
    <div className="min-w-0 flex-1 rounded-lg border border-border-subtle bg-bg-secondary px-3 py-2">
      <div className="mb-1 truncate text-[11px] font-medium uppercase tracking-[0.08em] text-faint">
        {label}
      </div>
      {result ? (
        <>
          <div className="flex flex-wrap items-baseline gap-2">
            {result.grade && (
              <span className="text-[17px] font-semibold text-primary">{result.grade}</span>
            )}
            {score && <span className="text-[13px] text-secondary">{score}</span>}
          </div>
          <p className="mt-1 text-[12px] leading-5 text-secondary">{result.verdict}</p>
        </>
      ) : (
        <p className="text-[12px] text-muted">{emptyText}</p>
      )}
    </div>
  );
}

/** The latest result of the library copy beside the incoming update's. */
export function IncomingEvaluationComparison({
  library,
  incoming,
}: {
  library: EvaluationRecord | null;
  incoming: EvaluationRecord;
}) {
  const { t } = useTranslation();
  const librarySha = shortSha(library?.source_revision ?? library?.baseline_revision);
  const incomingSha = shortSha(incoming.source_revision);
  return (
    <div className="space-y-2" data-testid="incoming-comparison">
      <div className="text-[11px] font-medium uppercase tracking-[0.08em] text-faint">
        {t("evaluationRevisions.compareTitle")}
      </div>
      <div className="flex flex-col gap-2 sm:flex-row">
        <ResultColumn
          label={
            librarySha
              ? t("evaluationRevisions.currentAt", { sha: librarySha })
              : t("evaluationRevisions.current")
          }
          record={library}
          emptyText={t("evaluationRevisions.noCurrentResult")}
        />
        <ResultColumn
          label={
            incomingSha
              ? t("evaluationRevisions.incomingAt", { sha: incomingSha })
              : t("evaluationRevisions.incoming")
          }
          record={incoming}
          emptyText=""
        />
      </div>
      <p className="text-[12px] text-muted">{t("evaluationRevisions.notApplied")}</p>
    </div>
  );
}
