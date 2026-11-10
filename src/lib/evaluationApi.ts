import { invoke } from "@tauri-apps/api/core";

// Mirrors src-tauri/src/core/evaluation.rs and commands/evaluation.rs. None of
// those types rename fields, so everything is snake_case. Optional fields the
// Rust side skips when empty are optional here; `Option` fields it always
// writes are `| null`.

export type EvaluationStatus =
  | "succeeded"
  | "failed"
  | "invalid_output"
  | "cancelled"
  | "timeout"
  | "budget_exceeded";

export type FindingSeverity = "critical" | "major" | "minor" | "info";

export interface EvaluationScore {
  value: number;
  max: number;
}

export interface EvaluationDimension {
  id?: string;
  name: string;
  score?: number;
  max?: number;
  note?: string;
}

export interface EvaluationFinding {
  /** One of FindingSeverity in practice; kept as text like the backend does. */
  severity: string;
  title: string;
  detail?: string;
  location?: string;
  suggestion?: string;
}

/** The v1 result contract, as produced by the agent. */
export interface EvaluationResult {
  schema_version: number;
  verdict: string;
  score?: EvaluationScore;
  grade?: string;
  dimensions?: EvaluationDimension[];
  findings?: EvaluationFinding[];
  report_markdown: string;
  [extra: string]: unknown;
}

export interface EvaluationRecord {
  id: string;
  skill_id: string;
  skill_content_hash: string | null;
  evaluator: string;
  evaluator_content_hash: string | null;
  agent: string;
  model: string | null;
  status: EvaluationStatus;
  /** Unix milliseconds. */
  started_at: number;
  finished_at: number;
  duration_ms: number;
  cost_usd: number | null;
  result: EvaluationResult | null;
  report_md: string | null;
  error: string | null;
  /** The skill's content changed since this run. An incoming run is stale
   *  until its update is applied. */
  stale: boolean;
  /** The upstream commit the judged files came from (sm-69q.10); null for
   *  older runs and skills without an upstream. */
  source_revision: string | null;
  /** The skill's baseline revision when the run started (sm-69q.10). */
  baseline_revision: string | null;
  /** What the run judged: the library copy, or the version an update would
   *  install (sm-69q.10). */
  subject: EvaluationSubject;
  /** The newest upstream revision known for the skill now (sm-69q.10). */
  upstream_revision: string | null;
}

export type EvaluationSubject = "library" | "incoming";

/** The latest successful evaluation of one skill. */
export interface EvaluationSummary {
  skill_id: string;
  evaluation_id: string;
  grade: string | null;
  score_value: number | null;
  score_max: number | null;
  stale: boolean;
  finished_at: number;
}

/** Unset fields fall back to the eval_* settings, then the defaults. */
export interface EvaluateOptions {
  agent?: string | null;
  evaluator?: string | null;
  model?: string | null;
  timeout_secs?: number | null;
  budget_usd?: number | null;
  /** Judge the version an update would install, staged outside the library
   *  (sm-69q.10). */
  incoming?: boolean;
}

export interface AgentRunnerInfo {
  key: string;
  display_name: string;
  experimental: boolean;
  /** The binary a run would use, after the agent_binary_<key> setting. */
  binary: string | null;
  /** Why no binary was found. */
  binary_error: string | null;
}

export type EvaluationPhase = "started" | "progress" | "finished" | "failed" | "cancelled";

/** Payload of the `evaluation-progress` event. */
export interface EvaluationProgress {
  run_id: string;
  skill_id: string;
  phase: EvaluationPhase;
  detail: string | null;
  evaluation_id: string | null;
  error: string | null;
}

export const EVALUATION_PROGRESS_EVENT = "evaluation-progress";

/** Settings keys read by the evaluation service (core/evaluation.rs). */
export const EVAL_SETTINGS = {
  agent: "eval_agent",
  evaluator: "eval_evaluator",
  model: "eval_model",
  timeoutSecs: "eval_timeout_secs",
  budgetUsd: "eval_budget_usd",
} as const;

export const DEFAULT_EVAL_AGENT = "claude_code";
export const DEFAULT_EVALUATOR = "skill-judge";
export const DEFAULT_EVAL_TIMEOUT_SECS = 600;
export const DEFAULT_EVAL_BUDGET_USD = 2;
/** Where skill-judge is installed from, as installFromSkillssh(source, skillId) wants it. */
export const SKILL_JUDGE_SKILLSSH = { source: "softaworks/agent-toolkit", skillId: "skill-judge" };

/** The settings key overriding where an agent's binary is found. */
export const agentBinarySettingKey = (agentKey: string) => `agent_binary_${agentKey}`;

/** Starts a run and returns its id at once; progress arrives as events. */
export const startSkillEvaluation = (skillId: string, options?: EvaluateOptions) =>
  invoke<string>("start_skill_evaluation", { skillId, options: options ?? null });

/** `false` when no such run is active. */
export const cancelSkillEvaluation = (runId: string) =>
  invoke<boolean>("cancel_skill_evaluation", { runId });

/** Every stored run of the skill, newest first. */
export const getSkillEvaluations = (skillId: string) =>
  invoke<EvaluationRecord[]>("get_skill_evaluations", { skillId });

export const getEvaluationSummaries = () =>
  invoke<EvaluationSummary[]>("get_evaluation_summaries");

export const listAgentRunners = () => invoke<AgentRunnerInfo[]>("list_agent_runners");
