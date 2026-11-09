// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useEvaluateCardAction } from "./useEvaluateCardAction";
import { CardActionMenu } from "../components/CardActionMenu";
import type { EvaluationRuns } from "../lib/evaluation";

const app = vi.hoisted(() => ({
  evaluationRuns: {} as EvaluationRuns,
  openSkillDetailById: vi.fn(),
  startEvaluation: vi.fn(),
}));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("../context/AppContext", () => ({ useApp: () => app }));

function Menu({ skillId }: { skillId: string }) {
  const evaluateAction = useEvaluateCardAction();
  return <CardActionMenu label="more" actions={[evaluateAction(skillId)]} />;
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.clearAllMocks();
  app.evaluationRuns = {};
  app.startEvaluation.mockResolvedValue(undefined);
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

async function openMenu(skillId = "skill-a") {
  await act(async () => root.render(<Menu skillId={skillId} />));
  const toggle = container.querySelector<HTMLButtonElement>('[aria-label="more"]');
  await act(async () => toggle!.click());
  const entries = [...container.querySelectorAll<HTMLButtonElement>("button")].slice(1);
  expect(entries).toHaveLength(1);
  return entries[0];
}

describe("useEvaluateCardAction", () => {
  it("opens the detail panel, then starts a run with the default settings", async () => {
    const entry = await openMenu();
    expect(entry.textContent).toBe("evaluation.menuEvaluate");
    expect(entry.disabled).toBe(false);
    await act(async () => entry.click());
    expect(app.openSkillDetailById).toHaveBeenCalledExactlyOnceWith("skill-a");
    expect(app.startEvaluation).toHaveBeenCalledExactlyOnceWith("skill-a");
    expect(app.openSkillDetailById.mock.invocationCallOrder[0]).toBeLessThan(
      app.startEvaluation.mock.invocationCallOrder[0],
    );
  });

  it("is disabled while a run of that skill is active", async () => {
    app.evaluationRuns = {
      "skill-a": {
        runId: "run-1",
        phase: "progress",
        lines: [],
        startedAt: 0,
        error: null,
        evaluationId: null,
      },
    };
    const entry = await openMenu();
    expect(entry.textContent).toBe("evaluation.menuEvaluating");
    expect(entry.disabled).toBe(true);
  });

  it("is enabled again once the skill's run has finished", async () => {
    app.evaluationRuns = {
      "skill-a": {
        runId: "run-1",
        phase: "finished",
        lines: [],
        startedAt: 0,
        error: null,
        evaluationId: "e1",
      },
    };
    const entry = await openMenu();
    expect(entry.disabled).toBe(false);
  });
});
