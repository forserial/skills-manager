// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { EvaluationGradeBadge } from "./EvaluationGradeBadge";
import type { EvaluationSummary } from "../lib/evaluationApi";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, options?: Record<string, string>) =>
      options ? `${key} ${Object.values(options).join(" ")}` : key,
  }),
}));

const summary = (over: Partial<EvaluationSummary> = {}): EvaluationSummary => ({
  skill_id: "skill-a",
  evaluation_id: "e1",
  grade: "B",
  score_value: 77,
  score_max: 120,
  stale: false,
  finished_at: 1_700_000_000_000,
  ...over,
});

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
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

async function render(value: EvaluationSummary | undefined) {
  await act(async () => root.render(<EvaluationGradeBadge summary={value} />));
  return container.querySelector("span");
}

describe("EvaluationGradeBadge", () => {
  it("renders nothing without a summary", async () => {
    expect(await render(undefined)).toBeNull();
    expect(container.innerHTML).toBe("");
  });

  it("shows the grade, with score and date in the tooltip", async () => {
    const pill = await render(summary());
    expect(pill?.textContent).toBe("B");
    expect(pill?.title).toContain("evaluation.badge.score 77/120");
    expect(pill?.title).toContain("evaluation.badge.evaluatedAt");
    expect(pill?.title).not.toContain("evaluation.badge.stale");
    expect(pill?.className).not.toContain("opacity-50");
    expect(pill?.className).toContain("text-sky-600");
  });

  it("shows the score when there is no grade", async () => {
    const pill = await render(summary({ grade: null }));
    expect(pill?.textContent).toBe("77/120");
  });

  it("is dimmed and says why once the skill changed", async () => {
    const pill = await render(summary({ stale: true }));
    expect(pill?.className).toContain("opacity-50");
    expect(pill?.dataset.stale).toBe("true");
    expect(pill?.title).toContain("evaluation.badge.stale");
  });
});
