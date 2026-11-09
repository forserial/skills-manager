// Evaluation run state for AppContext. It lives above the detail panel
// because SkillDetailPanel remounts whenever the skill's updated_at changes,
// and closing the panel must not lose a running evaluation.
import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import i18n from "../i18n";
import * as api from "../lib/tauri";
import { getErrorMessage } from "../lib/error";
import {
  evaluationRunsReducer,
  type EvaluationRuns,
} from "../lib/evaluation";
import {
  EVALUATION_PROGRESS_EVENT,
  type EvaluateOptions,
  type EvaluationProgress,
  type EvaluationSummary,
} from "../lib/evaluationApi";

export interface EvaluationContextState {
  /** Run state keyed by skill id. */
  evaluationRuns: EvaluationRuns;
  /** Latest successful evaluation keyed by skill id. */
  evaluationSummaries: Record<string, EvaluationSummary>;
  refreshEvaluationSummaries: () => Promise<void>;
  /** Starts a run; failures to start are toasted, never thrown. */
  startEvaluation: (skillId: string, options?: EvaluateOptions) => Promise<void>;
  cancelEvaluation: (skillId: string) => Promise<void>;
}

const loadSummaries = (): Promise<Record<string, EvaluationSummary>> =>
  api
    .getEvaluationSummaries()
    .then((list) => Object.fromEntries(list.map((s) => [s.skill_id, s])));

export function useEvaluationRuns(): EvaluationContextState {
  const [evaluationRuns, dispatch] = useReducer(evaluationRunsReducer, {});
  const [evaluationSummaries, setEvaluationSummaries] = useState<
    Record<string, EvaluationSummary>
  >({});
  const runsRef = useRef(evaluationRuns);
  useEffect(() => {
    runsRef.current = evaluationRuns;
  }, [evaluationRuns]);

  const refreshEvaluationSummaries = useCallback(async () => {
    try {
      setEvaluationSummaries(await loadSummaries());
    } catch (error) {
      console.error("Failed to load evaluation summaries:", error);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    loadSummaries()
      .then((summaries) => {
        if (!cancelled) setEvaluationSummaries(summaries);
      })
      .catch((error) => console.error("Failed to load evaluation summaries:", error));
    return () => {
      cancelled = true;
    };
  }, []);

  // One listener for every run, registered once.
  useEffect(() => {
    const unlistenPromise = listen<EvaluationProgress>(EVALUATION_PROGRESS_EVENT, (event) => {
      const progress = event.payload;
      dispatch({ type: "progress", event: progress, at: Date.now() });
      switch (progress.phase) {
        case "finished":
          toast.success(i18n.t("evaluation.toast.finished"));
          refreshEvaluationSummaries();
          break;
        case "failed":
          toast.error(i18n.t("evaluation.toast.failed"), {
            description: progress.error ?? undefined,
            duration: 10000,
          });
          refreshEvaluationSummaries();
          break;
        case "cancelled":
          toast.info(i18n.t("evaluation.toast.cancelled"));
          break;
      }
    });
    return () => {
      unlistenPromise
        .then((unlisten) => unlisten())
        .catch((error) => {
          console.error(`Failed to unlisten ${EVALUATION_PROGRESS_EVENT}:`, error);
        });
    };
  }, [refreshEvaluationSummaries]);

  const startEvaluation = useCallback(async (skillId: string, options?: EvaluateOptions) => {
    dispatch({ type: "requested", skillId, at: Date.now() });
    try {
      const runId = await api.startSkillEvaluation(skillId, options);
      dispatch({ type: "started", skillId, runId });
    } catch (error) {
      const message = getErrorMessage(error, i18n.t("evaluation.toast.startFailed"));
      dispatch({ type: "startFailed", skillId, error: message });
      toast.error(i18n.t("evaluation.toast.startFailed"), { description: message });
    }
  }, []);

  const cancelEvaluation = useCallback(async (skillId: string) => {
    const runId = runsRef.current[skillId]?.runId;
    if (!runId) return;
    try {
      await api.cancelSkillEvaluation(runId);
    } catch (error) {
      toast.error(getErrorMessage(error, i18n.t("common.error")));
    }
  }, []);

  return {
    evaluationRuns,
    evaluationSummaries,
    refreshEvaluationSummaries,
    startEvaluation,
    cancelEvaluation,
  };
}
