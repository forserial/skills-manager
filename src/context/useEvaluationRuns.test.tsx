// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ManagedSkill } from "../lib/tauri";
import type { EvaluationSummary } from "../lib/evaluationApi";
import { useEvaluationRuns } from "./useEvaluationRuns";

const api = vi.hoisted(() => ({ getEvaluationSummaries: vi.fn() }));
vi.mock("../lib/tauri", () => api);
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));
vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() } }));
vi.mock("../i18n", () => ({ default: { t: (key: string) => key } }));

const summary = (stale: boolean): EvaluationSummary => ({
  skill_id: "skill-a",
  evaluation_id: "e1",
  grade: "B",
  score_value: null,
  score_max: null,
  stale,
  finished_at: 0,
});

function Probe({ skills }: { skills: ManagedSkill[] }) {
  const { evaluationSummaries } = useEvaluationRuns(skills);
  return <>{String(evaluationSummaries["skill-a"]?.stale)}</>;
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.clearAllMocks();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});

describe("useEvaluationRuns summaries", () => {
  it("loads on mount and reloads when the skill list reloads", async () => {
    api.getEvaluationSummaries.mockResolvedValueOnce([summary(false)]);
    await act(async () => root.render(<Probe skills={[]} />));
    expect(api.getEvaluationSummaries).toHaveBeenCalledTimes(1);
    expect(container.textContent).toBe("false");

    // The skill's content changed: the list reloads, and the summary is stale now.
    api.getEvaluationSummaries.mockResolvedValueOnce([summary(true)]);
    await act(async () => root.render(<Probe skills={[]} />));
    expect(api.getEvaluationSummaries).toHaveBeenCalledTimes(2);
    expect(container.textContent).toBe("true");
  });
});
