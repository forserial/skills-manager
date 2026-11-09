import { useCallback, useEffect, useState } from "react";
import { Loader2, Save } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { useApp } from "../context/AppContext";
import * as api from "../lib/tauri";
import { getErrorMessage } from "../lib/error";
import {
  DEFAULT_EVAL_BUDGET_USD,
  DEFAULT_EVAL_TIMEOUT_SECS,
  DEFAULT_EVALUATOR,
  EVAL_SETTINGS,
  agentBinarySettingKey,
  type AgentRunnerInfo,
} from "../lib/evaluationApi";
import { checkPositiveNumberSetting, evaluatorCandidates, pickAgent } from "../lib/evaluation";

const fieldClass = "app-input bg-background";
const selectClass =
  "app-input h-[34px] max-w-full cursor-pointer bg-background dark:[color-scheme:dark]";

/** Settings → Evaluation. Self-contained: it reads and writes its own keys
 *  through the generic get/set settings commands. The section is titled
 *  "Evaluation" because runner messages point users at "Settings → Evaluation". */
export function EvaluationSettingsSection() {
  const { t } = useTranslation();
  const { managedSkills } = useApp();
  const [runners, setRunners] = useState<AgentRunnerInfo[]>([]);
  const [agent, setAgent] = useState("");
  const [evaluator, setEvaluator] = useState("");
  const [model, setModel] = useState("");
  const [timeoutSecs, setTimeoutSecs] = useState("");
  const [budgetUsd, setBudgetUsd] = useState("");
  const [binaries, setBinaries] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);

  const loadRunners = useCallback(async () => {
    try {
      const list = await api.listAgentRunners();
      setRunners(list);
      return list;
    } catch (error) {
      console.error("Failed to list agent runners:", error);
      return [];
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    const read = (key: string) => api.getSettings(key).catch(() => null);
    (async () => {
      const list = await loadRunners();
      const [agentValue, evaluatorValue, modelValue, timeoutValue, budgetValue, ...binaryValues] =
        await Promise.all([
          read(EVAL_SETTINGS.agent),
          read(EVAL_SETTINGS.evaluator),
          read(EVAL_SETTINGS.model),
          read(EVAL_SETTINGS.timeoutSecs),
          read(EVAL_SETTINGS.budgetUsd),
          ...list.map((runner) => read(agentBinarySettingKey(runner.key))),
        ]);
      if (cancelled) return;
      setAgent(pickAgent(list.map((runner) => runner.key), agentValue));
      // Unset and an explicit skill-judge mean the same thing.
      const configuredEvaluator = evaluatorValue?.trim() ?? "";
      setEvaluator(configuredEvaluator === DEFAULT_EVALUATOR ? "" : configuredEvaluator);
      setModel(modelValue ?? "");
      setTimeoutSecs(timeoutValue ?? "");
      setBudgetUsd(budgetValue ?? "");
      setBinaries(
        Object.fromEntries(list.map((runner, index) => [runner.key, binaryValues[index] ?? ""])),
      );
    })();
    return () => {
      cancelled = true;
    };
  }, [loadRunners]);

  const saveImmediately = async (key: string, value: string) => {
    try {
      await api.setSettings(key, value);
      toast.success(t("evaluation.settings.saved"));
    } catch (error) {
      toast.error(t("evaluation.settings.saveFailed"), {
        description: getErrorMessage(error, t("common.error")),
      });
    }
  };

  const handleSaveFields = async () => {
    const timeout = checkPositiveNumberSetting(timeoutSecs, true);
    if (!timeout.ok) {
      toast.error(t("evaluation.settings.invalidTimeout"));
      return;
    }
    const budget = checkPositiveNumberSetting(budgetUsd, false);
    if (!budget.ok) {
      toast.error(t("evaluation.settings.invalidBudget"));
      return;
    }
    setSaving(true);
    try {
      await Promise.all([
        api.setSettings(EVAL_SETTINGS.model, model.trim()),
        api.setSettings(EVAL_SETTINGS.timeoutSecs, timeout.value),
        api.setSettings(EVAL_SETTINGS.budgetUsd, budget.value),
        ...runners.map((runner) =>
          api.setSettings(agentBinarySettingKey(runner.key), (binaries[runner.key] ?? "").trim()),
        ),
      ]);
      await loadRunners();
      toast.success(t("evaluation.settings.saved"));
    } catch (error) {
      toast.error(t("evaluation.settings.saveFailed"), {
        description: getErrorMessage(error, t("common.error")),
      });
    } finally {
      setSaving(false);
    }
  };

  // Offer every evaluator in the library, and keep a configured one that is
  // not (or no longer) there so the select still shows the truth.
  const candidates = evaluatorCandidates(managedSkills).filter((name) => name !== DEFAULT_EVALUATOR);
  const extraEvaluator =
    evaluator && evaluator !== DEFAULT_EVALUATOR && !candidates.includes(evaluator) ? evaluator : null;

  const row = "px-4 py-3";
  const label = "text-[14px] font-semibold text-primary";
  const desc = "mt-0.5 mb-2 text-[12px] text-muted";

  return (
    <section>
      <h2 className="app-section-title mb-3">{t("evaluation.settings.title")}</h2>
      <div className="app-panel overflow-hidden divide-y divide-border-faint">
        <div className="flex flex-wrap items-center justify-between gap-3 px-4 py-2.5">
          <div className="min-w-0 flex-1">
            <h3 className={label}>{t("evaluation.settings.agentLabel")}</h3>
            <p className="text-[12px] text-muted">{t("evaluation.settings.agentDesc")}</p>
          </div>
          <select
            value={agent}
            aria-label={t("evaluation.settings.agentLabel")}
            onChange={(e) => {
              setAgent(e.target.value);
              saveImmediately(EVAL_SETTINGS.agent, e.target.value);
            }}
            className={selectClass}
          >
            {runners.map((runner) => (
              <option key={runner.key} value={runner.key}>
                {runner.display_name}
                {runner.experimental ? ` (${t("evaluation.experimental")})` : ""}
                {runner.binary ? "" : ` — ${t("evaluation.agentNotFound")}`}
              </option>
            ))}
          </select>
        </div>

        <div className="flex flex-wrap items-center justify-between gap-3 px-4 py-2.5">
          <div className="min-w-0 flex-1">
            <h3 className={label}>{t("evaluation.settings.evaluatorLabel")}</h3>
            <p className="text-[12px] text-muted">{t("evaluation.settings.evaluatorDesc")}</p>
          </div>
          <select
            value={evaluator}
            aria-label={t("evaluation.settings.evaluatorLabel")}
            onChange={(e) => {
              setEvaluator(e.target.value);
              saveImmediately(EVAL_SETTINGS.evaluator, e.target.value);
            }}
            className={selectClass}
          >
            <option value="">{t("evaluation.settings.evaluatorDefault")}</option>
            {extraEvaluator && <option value={extraEvaluator}>{extraEvaluator}</option>}
            {candidates.map((name) => (
              <option key={name} value={name}>
                {name}
              </option>
            ))}
          </select>
        </div>

        <div className={row}>
          <h3 className={label}>{t("evaluation.settings.modelLabel")}</h3>
          <p className={desc}>{t("evaluation.settings.modelDesc")}</p>
          <input
            type="text"
            value={model}
            onChange={(e) => setModel(e.target.value)}
            placeholder={t("evaluation.settings.modelPlaceholder")}
            className={`${fieldClass} w-full font-mono`}
          />
        </div>

        <div className="grid gap-0 divide-y divide-border-faint md:grid-cols-2 md:divide-x md:divide-y-0">
          <div className={row}>
            <h3 className={label}>{t("evaluation.settings.timeoutLabel")}</h3>
            <p className={desc}>{t("evaluation.settings.timeoutDesc")}</p>
            <input
              type="text"
              inputMode="numeric"
              value={timeoutSecs}
              onChange={(e) => setTimeoutSecs(e.target.value)}
              placeholder={String(DEFAULT_EVAL_TIMEOUT_SECS)}
              className={`${fieldClass} w-full font-mono`}
            />
          </div>
          <div className={row}>
            <h3 className={label}>{t("evaluation.settings.budgetLabel")}</h3>
            <p className={desc}>{t("evaluation.settings.budgetDesc")}</p>
            <input
              type="text"
              inputMode="decimal"
              value={budgetUsd}
              onChange={(e) => setBudgetUsd(e.target.value)}
              placeholder={String(DEFAULT_EVAL_BUDGET_USD)}
              className={`${fieldClass} w-full font-mono`}
            />
          </div>
        </div>

        {runners.map((runner) => (
          <div key={runner.key} className={row}>
            <h3 className={label}>
              {t("evaluation.settings.binaryLabel", { agent: runner.display_name })}
            </h3>
            <p className={desc}>{t("evaluation.settings.binaryDesc")}</p>
            <input
              type="text"
              value={binaries[runner.key] ?? ""}
              onChange={(e) =>
                setBinaries((prev) => ({ ...prev, [runner.key]: e.target.value }))
              }
              placeholder={t("evaluation.settings.binaryPlaceholder")}
              className={`${fieldClass} w-full font-mono`}
            />
            {runner.binary ? (
              <p className="mt-1 break-all font-mono text-[12px] text-muted">
                {t("evaluation.settings.binaryFound", { path: runner.binary })}
              </p>
            ) : (
              runner.binary_error && (
                <p className="mt-1 break-words text-[12px] text-amber-600 dark:text-amber-400">
                  {runner.binary_error}
                </p>
              )
            )}
          </div>
        ))}

        <div className="flex justify-end px-4 py-2.5">
          <button
            type="button"
            onClick={handleSaveFields}
            disabled={saving}
            className="app-button-secondary gap-1.5"
          >
            {saving ? <Loader2 className="h-3 w-3 animate-spin" /> : <Save className="h-3 w-3" />}
            {t("common.save")}
          </button>
        </div>
      </div>
    </section>
  );
}
