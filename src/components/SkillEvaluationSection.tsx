import { useEffect, useState } from "react";
import {
  AlertTriangle,
  ChevronDown,
  ChevronUp,
  Download,
  Gauge,
  Loader2,
  Play,
  Square,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { cn } from "../utils";
import { useApp } from "../context/AppContext";
import * as api from "../lib/tauri";
import type { ManagedSkill } from "../lib/tauri";
import { getErrorMessage } from "../lib/error";
import {
  EVAL_SETTINGS,
  SKILL_JUDGE_SKILLSSH,
  type AgentRunnerInfo,
  type EvaluationRecord,
} from "../lib/evaluationApi";
import {
  evaluatorCandidates,
  formatCost,
  formatDuration,
  formatScore,
  groupFindingsBySeverity,
  hasSkillJudge,
  isRunActive,
  pickAgent,
  pickEvaluator,
  scorePercent,
  type SeverityGroupKey,
} from "../lib/evaluation";
import { SkillMarkdown } from "./SkillMarkdown";

const SEVERITY_CLASS: Record<SeverityGroupKey, string> = {
  critical: "bg-red-500/10 text-red-600 dark:text-red-400",
  major: "bg-amber-500/10 text-amber-600 dark:text-amber-400",
  minor: "bg-sky-500/10 text-sky-600 dark:text-sky-400",
  info: "bg-surface-hover text-muted",
  other: "bg-surface-hover text-muted",
};

/** Skills whose evaluation card the user left open. Outlives the panel's
 *  remounts (it remounts whenever the skill's updated_at changes). */
const expandedSkills = new Set<string>();

const pillClass =
  "inline-flex shrink-0 items-center gap-1.5 rounded-full border border-border-subtle bg-bg-secondary px-2 py-1 text-[12px] text-muted";

function StalePill({ title }: { title: string }) {
  const { t } = useTranslation();
  return (
    <span
      title={title}
      className="inline-flex shrink-0 items-center gap-1 rounded-full bg-amber-500/10 px-2 py-0.5 text-[11px] font-medium text-amber-600 dark:text-amber-400"
    >
      <AlertTriangle className="h-3 w-3" />
      {t("evaluation.stale")}
    </span>
  );
}

/** The evaluation card in the skill detail panel. Run state comes from
 *  AppContext, so closing the panel or a remount keeps a running evaluation. */
export function SkillEvaluationSection({ skill }: { skill: ManagedSkill }) {
  const { t } = useTranslation();
  const {
    evaluationRuns,
    evaluationSummaries,
    refreshEvaluationSummaries,
    startEvaluation,
    cancelEvaluation,
    managedSkills,
    refreshManagedSkills,
    refreshPresets,
  } = useApp();
  const run = evaluationRuns[skill.id];
  const active = isRunActive(run);
  const summary = evaluationSummaries[skill.id];

  const [expanded, setExpandedState] = useState(() => active || expandedSkills.has(skill.id));
  const setExpanded = (next: boolean) => {
    if (next) expandedSkills.add(skill.id);
    else expandedSkills.delete(skill.id);
    setExpandedState(next);
  };

  // The panel remounts when the skill changes on disk; the summaries may
  // have gone stale with it.
  useEffect(() => {
    refreshEvaluationSummaries();
  }, [refreshEvaluationSummaries]);
  const [runners, setRunners] = useState<AgentRunnerInfo[] | null>(null);
  const [configuredAgent, setConfiguredAgent] = useState<string | null>(null);
  const [configuredEvaluator, setConfiguredEvaluator] = useState<string | null>(null);
  const [agentChoice, setAgentChoice] = useState<string | null>(null);
  const [evaluatorChoice, setEvaluatorChoice] = useState<string | null>(null);
  const [records, setRecords] = useState<EvaluationRecord[] | null>(null);
  const [historyFailed, setHistoryFailed] = useState(false);
  const [showReport, setShowReport] = useState(false);
  const [installing, setInstalling] = useState(false);

  // Pickers: loaded once, the first time the card is opened.
  const runnersLoaded = runners !== null;
  useEffect(() => {
    if (!expanded || runnersLoaded) return;
    let cancelled = false;
    Promise.all([
      api.listAgentRunners(),
      api.getSettings(EVAL_SETTINGS.agent).catch(() => null),
      api.getSettings(EVAL_SETTINGS.evaluator).catch(() => null),
    ])
      .then(([list, agentSetting, evaluatorSetting]) => {
        if (cancelled) return;
        setRunners(list);
        setConfiguredAgent(agentSetting);
        setConfiguredEvaluator(evaluatorSetting);
      })
      .catch((error) => {
        if (cancelled) return;
        setRunners([]);
        toast.error(getErrorMessage(error, t("common.error")));
      });
    return () => {
      cancelled = true;
    };
  }, [expanded, runnersLoaded, t]);

  // History: reloaded whenever a run of this skill reaches a new phase, so a
  // finished run shows up without reopening the panel.
  const runPhase = run?.phase;
  const runEvaluationId = run?.evaluationId;
  useEffect(() => {
    if (!expanded) return;
    let cancelled = false;
    api
      .getSkillEvaluations(skill.id)
      .then((list) => {
        if (cancelled) return;
        setRecords(list);
        setHistoryFailed(false);
      })
      .catch((error) => {
        if (cancelled) return;
        console.error("Failed to load evaluations:", error);
        setHistoryFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [expanded, skill.id, runPhase, runEvaluationId]);

  const agentKeys = (runners ?? []).map((runner) => runner.key);
  const agent = agentChoice ?? pickAgent(agentKeys, configuredAgent);
  const selectedRunner = runners?.find((runner) => runner.key === agent) ?? null;
  const candidates = evaluatorCandidates(managedSkills);
  const evaluator = evaluatorChoice ?? pickEvaluator(candidates, configuredEvaluator);
  const judgePresent = hasSkillJudge(managedSkills);

  const latest = records?.[0] ?? null;
  const latestSucceeded = records?.find((record) => record.status === "succeeded") ?? null;
  const lastRunFailed = !active && latest && latest.status !== "succeeded" ? latest : null;

  const handleEvaluate = () => {
    setExpanded(true);
    startEvaluation(skill.id, {
      agent: agent || null,
      evaluator: evaluator || null,
    });
  };

  const handleInstallJudge = async () => {
    setInstalling(true);
    try {
      await api.installFromSkillssh(SKILL_JUDGE_SKILLSSH.source, SKILL_JUDGE_SKILLSSH.skillId);
      await Promise.all([refreshPresets(), refreshManagedSkills()]);
      toast.success(t("evaluation.installed"));
    } catch (error) {
      toast.error(t("evaluation.installFailed"), {
        description: getErrorMessage(error, t("common.error")),
      });
    } finally {
      setInstalling(false);
    }
  };

  const headerBadge = active ? (
    <span className={pillClass}>
      <Loader2 className="h-3.5 w-3.5 animate-spin" />
      {t(`evaluation.phase.${run!.phase}`)}
    </span>
  ) : summary ? (
    <span className={pillClass}>
      <Gauge className="h-3.5 w-3.5" />
      {[summary.grade, formatScore(summary.score_value, summary.score_max)]
        .filter(Boolean)
        .join(" · ") || t("evaluation.status.succeeded")}
    </span>
  ) : (
    <span className={pillClass}>
      <Gauge className="h-3.5 w-3.5" />
      {t("evaluation.notEvaluated")}
    </span>
  );

  return (
    <div className="mb-4 rounded-xl border border-border-subtle bg-surface/70">
      <button
        type="button"
        onClick={() => setExpanded(!expanded)}
        aria-expanded={expanded}
        aria-controls="skill-evaluation"
        className="flex w-full items-center justify-between gap-3 px-4 py-3 text-left"
      >
        <span className="flex min-w-0 items-center gap-2">
          {headerBadge}
          <span className="truncate text-[13px] font-medium text-secondary">
            {t("evaluation.title")}
          </span>
          {!active && summary?.stale && <StalePill title={t("evaluation.staleHint")} />}
        </span>
        <span className="inline-flex shrink-0 items-center gap-1 text-[12px] text-muted">
          <span>{expanded ? t("evaluation.collapse") : t("evaluation.expand")}</span>
          {expanded ? <ChevronUp className="h-3.5 w-3.5" /> : <ChevronDown className="h-3.5 w-3.5" />}
        </span>
      </button>

      {expanded && (
        <div id="skill-evaluation" className="space-y-4 border-t border-border-subtle px-4 py-3">
          {!judgePresent && (
            <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-border-subtle bg-bg-secondary px-3 py-2">
              <span className="text-[12px] text-muted">{t("evaluation.installPrompt")}</span>
              <button
                type="button"
                onClick={handleInstallJudge}
                disabled={installing}
                className="app-button-secondary gap-1.5 px-3 py-1.5 text-[12px]"
              >
                {installing ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Download className="h-3.5 w-3.5" />
                )}
                {installing ? t("evaluation.installing") : t("evaluation.installJudge")}
              </button>
            </div>
          )}

          {/* Pickers and actions */}
          <div className="flex flex-wrap items-end gap-2">
            <label className="flex min-w-0 flex-1 flex-col gap-1">
              <span className="text-[11px] font-medium uppercase tracking-[0.08em] text-faint">
                {t("evaluation.agent")}
              </span>
              <select
                value={agent}
                onChange={(e) => setAgentChoice(e.target.value)}
                disabled={active || runners === null}
                className="app-input h-[34px] cursor-pointer bg-background dark:[color-scheme:dark]"
              >
                {(runners ?? []).map((runner) => (
                  <option key={runner.key} value={runner.key}>
                    {runner.display_name}
                    {runner.experimental ? ` (${t("evaluation.experimental")})` : ""}
                    {runner.binary ? "" : ` — ${t("evaluation.agentNotFound")}`}
                  </option>
                ))}
              </select>
            </label>
            <label className="flex min-w-0 flex-1 flex-col gap-1">
              <span className="text-[11px] font-medium uppercase tracking-[0.08em] text-faint">
                {t("evaluation.evaluator")}
              </span>
              <select
                value={evaluator}
                onChange={(e) => setEvaluatorChoice(e.target.value)}
                disabled={active || candidates.length === 0}
                className="app-input h-[34px] cursor-pointer bg-background dark:[color-scheme:dark]"
              >
                {candidates.map((name) => (
                  <option key={name} value={name}>
                    {name}
                  </option>
                ))}
              </select>
            </label>
            {active ? (
              <button
                type="button"
                onClick={() => cancelEvaluation(skill.id)}
                disabled={!run?.runId}
                className="app-button-secondary h-[34px] gap-1.5 px-3 py-0"
              >
                <Square className="h-3.5 w-3.5" />
                {t("evaluation.cancel")}
              </button>
            ) : (
              <button
                type="button"
                onClick={handleEvaluate}
                disabled={!evaluator || !agent}
                className="app-button-primary h-[34px] gap-1.5 px-3 py-0"
              >
                <Play className="h-3.5 w-3.5" />
                {t("evaluation.evaluate")}
              </button>
            )}
          </div>
          {selectedRunner && !selectedRunner.binary && (
            <p className="flex items-start gap-1.5 text-[12px] text-amber-600 dark:text-amber-400">
              <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
              <span className="min-w-0 break-words">
                {t("evaluation.agentUnavailable", {
                  agent: selectedRunner.display_name,
                  error: selectedRunner.binary_error ?? "",
                })}
              </span>
            </p>
          )}
          {candidates.length === 0 && (
            <p className="text-[12px] text-muted">{t("evaluation.noEvaluators")}</p>
          )}
          <p className="text-[12px] text-muted">{t("evaluation.costNote")}</p>

          {/* Progress of the current or last run started from this app */}
          {run && run.phase !== "finished" && (
            <div className="rounded-lg border border-border-subtle bg-bg-secondary px-3 py-2">
              <div className="flex items-center gap-2 text-[12px] font-medium text-secondary">
                {active && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
                {t(`evaluation.phase.${run.phase}`)}
              </div>
              {run.lines.length > 0 && active && (
                <div className="mt-2 max-h-40 overflow-y-auto font-mono text-[12px] leading-5 text-muted">
                  {run.lines.map((line, index) => (
                    <div key={index} className="break-words">
                      {line}
                    </div>
                  ))}
                </div>
              )}
              {!active && run.error && (
                <p className="mt-1 break-words text-[12px] text-red-600 dark:text-red-400">
                  {run.error}
                </p>
              )}
            </div>
          )}

          {lastRunFailed && !(run && !active && run.error) && (
            <p className="break-words text-[12px] text-red-600 dark:text-red-400">
              {t("evaluation.lastRunFailed", {
                status: t(`evaluation.status.${lastRunFailed.status}`),
                error: lastRunFailed.error ?? "",
              })}
            </p>
          )}

          {latestSucceeded?.result && (
            <EvaluationResultView
              record={latestSucceeded}
              showReport={showReport}
              onToggleReport={() => setShowReport((prev) => !prev)}
            />
          )}

          <EvaluationHistory records={records} failed={historyFailed} />
        </div>
      )}
    </div>
  );
}

function EvaluationResultView({
  record,
  showReport,
  onToggleReport,
}: {
  record: EvaluationRecord;
  showReport: boolean;
  onToggleReport: () => void;
}) {
  const { t } = useTranslation();
  const result = record.result!;
  const score = formatScore(result.score?.value, result.score?.max);
  const groups = groupFindingsBySeverity(result.findings);
  const report = record.report_md ?? result.report_markdown;

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-[11px] font-medium uppercase tracking-[0.08em] text-faint">
          {t("evaluation.latestResult")}
        </span>
        {result.grade && (
          <span className="text-[17px] font-semibold text-primary" title={t("evaluation.grade")}>
            {result.grade}
          </span>
        )}
        {score && (
          <span className="text-[13px] text-secondary" title={t("evaluation.score")}>
            {score}
          </span>
        )}
        {record.stale && <StalePill title={t("evaluation.staleHint")} />}
        <span className="text-[12px] text-muted">
          {new Date(record.finished_at).toLocaleString()}
        </span>
      </div>
      {record.stale && <p className="text-[12px] text-muted">{t("evaluation.staleHint")}</p>}
      <p className="text-[13px] leading-6 text-secondary">{result.verdict}</p>

      {result.dimensions && result.dimensions.length > 0 && (
        <div>
          <div className="mb-1.5 text-[11px] font-medium uppercase tracking-[0.08em] text-faint">
            {t("evaluation.dimensions")}
          </div>
          <div className="space-y-2">
            {result.dimensions.map((dimension, index) => (
              <div key={dimension.id ?? `${dimension.name}-${index}`} className="min-w-0">
                <div className="flex items-baseline justify-between gap-2 text-[12px]">
                  <span className="truncate text-secondary">{dimension.name}</span>
                  <span className="shrink-0 font-mono text-muted">
                    {formatScore(dimension.score, dimension.max) ?? "—"}
                  </span>
                </div>
                <div className="mt-1 h-1.5 overflow-hidden rounded-full bg-surface-hover">
                  <div
                    className="h-full rounded-full bg-accent"
                    style={{ width: `${scorePercent(dimension.score, dimension.max)}%` }}
                  />
                </div>
                {dimension.note && (
                  <p className="mt-1 text-[12px] text-muted">{dimension.note}</p>
                )}
              </div>
            ))}
          </div>
        </div>
      )}

      <div>
        <div className="mb-1.5 text-[11px] font-medium uppercase tracking-[0.08em] text-faint">
          {t("evaluation.findings")}
        </div>
        {groups.length === 0 ? (
          <p className="text-[12px] text-muted">{t("evaluation.noFindings")}</p>
        ) : (
          <div className="space-y-3">
            {groups.map((group) => (
              <div key={group.severity}>
                <span
                  className={cn(
                    "inline-flex items-center rounded-full px-2 py-0.5 text-[11px] font-medium",
                    SEVERITY_CLASS[group.severity],
                  )}
                >
                  {t(`evaluation.severity.${group.severity}`)} · {group.findings.length}
                </span>
                <ul className="mt-1.5 space-y-2">
                  {group.findings.map((finding, index) => (
                    <li key={index} className="min-w-0 text-[12px]">
                      <div className="text-[13px] font-medium text-secondary">{finding.title}</div>
                      {finding.location && (
                        <div className="truncate font-mono text-muted" title={finding.location}>
                          {finding.location}
                        </div>
                      )}
                      {finding.detail && <p className="mt-0.5 text-muted">{finding.detail}</p>}
                      {finding.suggestion && (
                        <p className="mt-0.5 text-muted">
                          <span className="font-medium text-secondary">
                            {t("evaluation.suggestion")}:
                          </span>{" "}
                          {finding.suggestion}
                        </p>
                      )}
                    </li>
                  ))}
                </ul>
              </div>
            ))}
          </div>
        )}
      </div>

      {report && (
        <div>
          <button
            type="button"
            onClick={onToggleReport}
            aria-expanded={showReport}
            className="inline-flex items-center gap-1 text-[12px] font-medium text-accent hover:underline"
          >
            {showReport ? <ChevronUp className="h-3.5 w-3.5" /> : <ChevronDown className="h-3.5 w-3.5" />}
            {showReport ? t("evaluation.hideReport") : t("evaluation.showReport")}
          </button>
          {showReport && (
            <div className="mt-2 rounded-lg border border-border-subtle bg-background px-4 py-3">
              <SkillMarkdown content={report} />
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function EvaluationHistory({
  records,
  failed,
}: {
  records: EvaluationRecord[] | null;
  failed: boolean;
}) {
  const { t } = useTranslation();
  return (
    <div>
      <div className="mb-1.5 text-[11px] font-medium uppercase tracking-[0.08em] text-faint">
        {t("evaluation.history")}
      </div>
      {failed ? (
        <p className="text-[12px] text-muted">{t("evaluation.historyLoadFailed")}</p>
      ) : records === null ? (
        <p className="text-[12px] text-muted">{t("common.loading")}</p>
      ) : records.length === 0 ? (
        <p className="text-[12px] text-muted">{t("evaluation.historyEmpty")}</p>
      ) : (
        <ul className="divide-y divide-border-faint rounded-lg border border-border-subtle">
          {records.map((record) => {
            const score = formatScore(record.result?.score?.value, record.result?.score?.max);
            const succeeded = record.status === "succeeded";
            return (
              <li
                key={record.id}
                className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2 text-[12px]"
                title={record.error ?? undefined}
              >
                <span className="text-muted">{new Date(record.started_at).toLocaleString()}</span>
                <span
                  className={cn(
                    "font-medium",
                    succeeded ? "text-secondary" : "text-red-600 dark:text-red-400",
                  )}
                >
                  {t(`evaluation.status.${record.status}`)}
                </span>
                {(record.result?.grade || score) && (
                  <span className="text-secondary">
                    {[record.result?.grade, score].filter(Boolean).join(" · ")}
                  </span>
                )}
                {succeeded && record.stale && <StalePill title={t("evaluation.staleHint")} />}
                <span className="ml-auto flex items-center gap-3 text-muted">
                  <span>
                    {record.agent}
                    {record.model ? ` · ${record.model}` : ""}
                  </span>
                  <span title={t("evaluation.cost")}>{formatCost(record.cost_usd)}</span>
                  <span title={t("evaluation.duration")}>{formatDuration(record.duration_ms)}</span>
                </span>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
