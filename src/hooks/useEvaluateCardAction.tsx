import { Gauge, Loader2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useApp } from "../context/AppContext";
import type { CardAction } from "../components/CardActionMenu";
import { isRunActive } from "../lib/evaluation";

/** The "Evaluate" entry of a skill card's action menu. Selecting it opens the
 *  detail panel and starts a run with the default agent and evaluator; the
 *  panel's evaluation section opens expanded because the run is active. */
export function useEvaluateCardAction(): (skillId: string) => CardAction {
  const { t } = useTranslation();
  const { evaluationRuns, openSkillDetailById, startEvaluation } = useApp();

  return (skillId) => {
    const running = isRunActive(evaluationRuns[skillId]);
    return {
      key: "evaluate",
      label: running ? t("evaluation.menuEvaluating") : t("evaluation.menuEvaluate"),
      icon: running ? (
        <Loader2 className="h-3.5 w-3.5 animate-spin" />
      ) : (
        <Gauge className="h-3.5 w-3.5" />
      ),
      disabled: running,
      onSelect: () => {
        openSkillDetailById(skillId);
        // Failures to start are toasted by startEvaluation.
        void startEvaluation(skillId);
      },
    };
  };
}
