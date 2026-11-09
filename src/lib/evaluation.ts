// Pure helpers for the evaluation UI: the run-state reducer kept in
// AppContext, evaluator discovery, grouping and formatting.
import type { ManagedSkill } from "./tauri";
import {
  DEFAULT_EVAL_AGENT,
  DEFAULT_EVALUATOR,
  type EvaluationFinding,
  type EvaluationPhase,
  type EvaluationProgress,
  type EvaluationSummary,
} from "./evaluationApi";

// ── Run state ──

/** How many progress lines a run keeps for display. */
export const MAX_PROGRESS_LINES = 20;

/** `starting`: asked the backend, no run id or event yet. */
export type RunPhase = "starting" | EvaluationPhase;

export interface EvaluationRunState {
  /** Null until the backend has returned or reported the run id. */
  runId: string | null;
  phase: RunPhase;
  /** The last MAX_PROGRESS_LINES progress lines, oldest first. */
  lines: string[];
  /** Unix ms, when the user asked for the run. */
  startedAt: number;
  error: string | null;
  /** The stored record, once a final event names it. */
  evaluationId: string | null;
}

/** Run state per skill id. */
export type EvaluationRuns = Record<string, EvaluationRunState>;

export type EvaluationRunAction =
  | { type: "requested"; skillId: string; at: number }
  | { type: "started"; skillId: string; runId: string }
  | { type: "startFailed"; skillId: string; error: string }
  | { type: "progress"; event: EvaluationProgress; at: number };

const TERMINAL: ReadonlySet<RunPhase> = new Set(["finished", "failed", "cancelled"]);

export const isTerminalPhase = (phase: RunPhase) => TERMINAL.has(phase);

export const isRunActive = (run: EvaluationRunState | undefined | null): boolean =>
  !!run && !isTerminalPhase(run.phase);

function freshRun(at: number, runId: string | null): EvaluationRunState {
  return { runId, phase: "starting", lines: [], startedAt: at, error: null, evaluationId: null };
}

export function evaluationRunsReducer(
  state: EvaluationRuns,
  action: EvaluationRunAction,
): EvaluationRuns {
  switch (action.type) {
    case "requested": {
      if (isRunActive(state[action.skillId])) return state;
      return { ...state, [action.skillId]: freshRun(action.at, null) };
    }
    case "started": {
      const run = state[action.skillId];
      // Events may arrive before the invoke resolves and already carry the id.
      if (!run || run.runId !== null) return state;
      return { ...state, [action.skillId]: { ...run, runId: action.runId } };
    }
    case "startFailed": {
      const run = state[action.skillId];
      if (!run || run.runId !== null) return state;
      return { ...state, [action.skillId]: { ...run, phase: "failed", error: action.error } };
    }
    case "progress": {
      const { event } = action;
      const current = state[event.skill_id];
      let run: EvaluationRunState;
      if (!current || (current.runId !== null && current.runId !== event.run_id)) {
        // A run we did not start here (e.g. after a webview reload), or a
        // newer run than the one we knew about. An older run's late events
        // are dropped: a newer run would have a different id and be active.
        if (current && current.runId !== event.run_id && isRunActive(current)) return state;
        run = freshRun(action.at, event.run_id);
      } else {
        if (current.runId === event.run_id && isTerminalPhase(current.phase)) return state;
        run = { ...current, runId: event.run_id };
      }
      switch (event.phase) {
        case "started":
          run = { ...run, phase: "started" };
          break;
        case "progress":
          run = {
            ...run,
            phase: "progress",
            lines: event.detail
              ? [...run.lines, event.detail].slice(-MAX_PROGRESS_LINES)
              : run.lines,
          };
          break;
        case "finished":
        case "failed":
        case "cancelled":
          run = {
            ...run,
            phase: event.phase,
            error: event.error,
            evaluationId: event.evaluation_id,
          };
          break;
      }
      return { ...state, [event.skill_id]: run };
    }
  }
}

// ── Evaluators ──

const dirName = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() ?? "";

/** Whether `skill` answers to `name`, the way the backend's find_evaluator matches. */
export const skillAnswersTo = (skill: ManagedSkill, name: string) =>
  skill.name === name || dirName(skill.central_path) === name;

export const hasSkillJudge = (skills: ManagedSkill[]) =>
  skills.some((skill) => skillAnswersTo(skill, DEFAULT_EVALUATOR));

/** The tag that marks a library skill as an evaluator. */
export const EVALUATOR_TAG = "evaluator";

/** Evaluator names to offer: skill-judge when present first, then skills
 *  tagged `evaluator`, by name. */
export function evaluatorCandidates(skills: ManagedSkill[]): string[] {
  const tagged = skills
    .filter((skill) => skill.tags.some((tag) => tag.toLowerCase() === EVALUATOR_TAG))
    .map((skill) => skill.name)
    .sort((a, b) => a.localeCompare(b));
  const names = hasSkillJudge(skills) ? [DEFAULT_EVALUATOR, ...tagged] : tagged;
  return [...new Set(names)];
}

/** The evaluator to preselect: the configured one when it is offered,
 *  else skill-judge, else the first candidate, else none. */
export function pickEvaluator(candidates: string[], configured: string | null | undefined): string {
  const wanted = configured?.trim();
  if (wanted && candidates.includes(wanted)) return wanted;
  if (candidates.includes(DEFAULT_EVALUATOR)) return DEFAULT_EVALUATOR;
  return candidates[0] ?? "";
}

/** The agent to preselect: the configured one when registered, else Claude
 *  Code, else the first registered. */
export function pickAgent(agentKeys: string[], configured: string | null | undefined): string {
  const wanted = configured?.trim();
  if (wanted && agentKeys.includes(wanted)) return wanted;
  if (agentKeys.includes(DEFAULT_EVAL_AGENT)) return DEFAULT_EVAL_AGENT;
  return agentKeys[0] ?? "";
}

// ── Findings ──

export const SEVERITY_ORDER = ["critical", "major", "minor", "info"] as const;
export type SeverityGroupKey = (typeof SEVERITY_ORDER)[number] | "other";

export interface SeverityGroup {
  severity: SeverityGroupKey;
  findings: EvaluationFinding[];
}

/** Findings by severity, most severe first, empty groups left out. An unknown
 *  severity goes in `other`, last, so it is still shown. */
export function groupFindingsBySeverity(findings: EvaluationFinding[] | undefined): SeverityGroup[] {
  const groups = new Map<SeverityGroupKey, EvaluationFinding[]>();
  for (const finding of findings ?? []) {
    const key = finding.severity.trim().toLowerCase();
    const severity: SeverityGroupKey = (SEVERITY_ORDER as readonly string[]).includes(key)
      ? (key as SeverityGroupKey)
      : "other";
    groups.set(severity, [...(groups.get(severity) ?? []), finding]);
  }
  return [...SEVERITY_ORDER, "other" as const]
    .filter((severity) => groups.has(severity))
    .map((severity) => ({ severity, findings: groups.get(severity)! }));
}

// ── Formatting ──

const trimNumber = (value: number) =>
  Number.isInteger(value) ? String(value) : String(Math.round(value * 10) / 10);

export function formatScore(value: number | null | undefined, max: number | null | undefined): string | null {
  if (value === null || value === undefined || !Number.isFinite(value)) return null;
  if (max === null || max === undefined || !Number.isFinite(max)) return trimNumber(value);
  return `${trimNumber(value)}/${trimNumber(max)}`;
}

/** A bar width in percent, clamped to 0–100; 0 when the max is unusable. */
export function scorePercent(value: number | null | undefined, max: number | null | undefined): number {
  if (value == null || max == null || !Number.isFinite(value) || !Number.isFinite(max) || max <= 0) {
    return 0;
  }
  return Math.min(100, Math.max(0, (value / max) * 100));
}

export function formatDuration(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return "—";
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  return rest === 0 ? `${minutes}m` : `${minutes}m ${rest}s`;
}

export function formatCost(usd: number | null | undefined): string {
  if (usd == null || !Number.isFinite(usd)) return "—";
  return `$${usd.toFixed(2)}`;
}

// ── Grade badge (skill cards) ──

export type GradeTone = "excellent" | "good" | "fair" | "poor" | "neutral";

/** A letter grade, optionally with + or -, and S for "above A". */
const LETTER_GRADE = /^([SA-F])[+-]?$/i;

/** The colour band of a letter grade. Anything that is not a letter grade
 *  (a word, a number) is neutral rather than guessed at. */
export function gradeTone(grade: string | null | undefined): GradeTone {
  const letter = grade?.trim().match(LETTER_GRADE)?.[1]?.toUpperCase();
  switch (letter) {
    case "S":
    case "A":
      return "excellent";
    case "B":
      return "good";
    case "C":
      return "fair";
    case "D":
    case "E":
    case "F":
      return "poor";
    default:
      return "neutral";
  }
}

export interface GradeBadgeProps {
  /** The grade, or the score when there is no grade. */
  label: string;
  tone: GradeTone;
  /** The skill changed since the evaluation. */
  dimmed: boolean;
  /** The formatted score, for the tooltip; null when there is none. */
  score: string | null;
  /** The grade, for the tooltip; null when there is none. */
  grade: string | null;
  /** Unix ms. */
  finishedAt: number;
}

/** What the card badge shows for a skill's latest successful evaluation;
 *  null when there is no evaluation, or it has neither grade nor score. */
export function gradeBadgeProps(summary: EvaluationSummary | null | undefined): GradeBadgeProps | null {
  if (!summary) return null;
  const grade = summary.grade?.trim() || null;
  const score = formatScore(summary.score_value, summary.score_max);
  const label = grade ?? score;
  if (!label) return null;
  return {
    label,
    tone: grade ? gradeTone(grade) : "neutral",
    dimmed: summary.stale,
    score,
    grade,
    finishedAt: summary.finished_at,
  };
}

// ── Settings ──

export type NumberSettingCheck =
  | { ok: true; value: string }
  | { ok: false };

/** Validate a timeout or budget field: empty clears the setting; otherwise
 *  a positive number, whole when `integer`. Returns the value to store. */
export function checkPositiveNumberSetting(raw: string, integer: boolean): NumberSettingCheck {
  const text = raw.trim();
  if (text === "") return { ok: true, value: "" };
  const pattern = integer ? /^\d+$/ : /^\d+(\.\d+)?$/;
  if (!pattern.test(text)) return { ok: false };
  const value = Number(text);
  if (!Number.isFinite(value) || value <= 0) return { ok: false };
  return { ok: true, value: text };
}
