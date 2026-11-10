// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  EvaluateUpdateButton,
  EvaluationRevisionLine,
  IncomingEvaluationComparison,
} from "./EvaluationRevisions";
import type { EvaluationRecord } from "../lib/evaluationApi";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, options?: Record<string, string>) =>
      options ? `${key} ${Object.values(options).join(" ")}` : key,
  }),
}));

const record = (over: Partial<EvaluationRecord> = {}): EvaluationRecord => ({
  id: "e1",
  skill_id: "s",
  skill_content_hash: "h",
  evaluator: "skill-judge",
  evaluator_content_hash: null,
  agent: "claude_code",
  model: null,
  status: "succeeded",
  started_at: 1,
  finished_at: 2,
  duration_ms: 1,
  cost_usd: null,
  result: {
    schema_version: 1,
    verdict: "Solid.",
    grade: "B",
    score: { value: 90, max: 120 },
    report_markdown: "# r",
  },
  report_md: "# r",
  error: null,
  stale: false,
  source_revision: "1111111aaaa",
  baseline_revision: "1111111aaaa",
  subject: "library",
  upstream_revision: "1111111aaaa",
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

async function render(node: ReactNode) {
  await act(async () => root.render(node));
}

describe("EvaluationRevisionLine", () => {
  it("says which revision was evaluated", async () => {
    await render(<EvaluationRevisionLine record={record()} />);
    const line = container.querySelector('[data-testid="evaluation-revision"]');
    expect(line?.textContent).toBe("evaluationRevisions.evaluatedAt 1111111");
  });

  it("adds where upstream is now once the skill has moved on", async () => {
    await render(<EvaluationRevisionLine record={record({ upstream_revision: "2222222bbbb" })} />);
    expect(container.textContent).toBe(
      "evaluationRevisions.evaluatedAt 1111111 · evaluationRevisions.upstreamNow 2222222",
    );
  });

  it("renders nothing for a run whose revision is unknown", async () => {
    await render(
      <EvaluationRevisionLine record={record({ source_revision: null, baseline_revision: null })} />,
    );
    expect(container.innerHTML).toBe("");
  });
});

describe("EvaluateUpdateButton", () => {
  it("starts a run when clicked", async () => {
    const onClick = vi.fn();
    await render(<EvaluateUpdateButton onClick={onClick} disabled={false} />);
    const button = container.querySelector("button")!;
    expect(button.textContent).toBe("evaluationRevisions.evaluateUpdate");
    await act(async () => button.click());
    expect(onClick).toHaveBeenCalledOnce();
  });
});

describe("IncomingEvaluationComparison", () => {
  it("shows the current result beside the incoming one, each with its revision", async () => {
    const incoming = record({
      id: "inc",
      subject: "incoming",
      stale: true,
      source_revision: "2222222bbbb",
      result: { schema_version: 1, verdict: "Better.", grade: "A", report_markdown: "# r" },
    });
    await render(<IncomingEvaluationComparison library={record()} incoming={incoming} />);
    const text = container.textContent ?? "";
    expect(text).toContain("evaluationRevisions.currentAt 1111111");
    expect(text).toContain("Solid.");
    expect(text).toContain("90/120");
    expect(text).toContain("evaluationRevisions.incomingAt 2222222");
    expect(text).toContain("Better.");
    expect(text).toContain("evaluationRevisions.notApplied");
  });

  it("says when the current version has no result to compare with", async () => {
    const incoming = record({ id: "inc", subject: "incoming", stale: true });
    await render(<IncomingEvaluationComparison library={null} incoming={incoming} />);
    expect(container.textContent).toContain("evaluationRevisions.noCurrentResult");
    expect(container.textContent).toContain("evaluationRevisions.current");
  });
});
