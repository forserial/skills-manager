import { describe, expect, it } from "vitest";
import type { EvaluationRecord } from "./evaluationApi";
import { canEvaluateIncoming, revisionNote, runsBySubject, shortSha } from "./evaluationRevisions";

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
  result: null,
  report_md: null,
  error: null,
  stale: false,
  source_revision: "1111111aaaa",
  baseline_revision: "1111111aaaa",
  subject: "library",
  upstream_revision: "1111111aaaa",
  ...over,
});

describe("shortSha", () => {
  it("keeps seven characters and passes null through", () => {
    expect(shortSha("0123456789abcdef")).toBe("0123456");
    expect(shortSha(null)).toBeNull();
    expect(shortSha(undefined)).toBeNull();
  });
});

describe("revisionNote", () => {
  it("names the judged revision and nothing more while upstream has not moved", () => {
    expect(revisionNote(record())).toEqual({ evaluatedAt: "1111111", upstreamNow: null });
  });

  it("adds where upstream is now once the skill has moved on", () => {
    expect(revisionNote(record({ upstream_revision: "2222222bbbb" }))).toEqual({
      evaluatedAt: "1111111",
      upstreamNow: "2222222",
    });
  });

  it("falls back to the baseline, and says nothing for runs without either", () => {
    expect(
      revisionNote(record({ source_revision: null, baseline_revision: "3333333cccc" })),
    ).toEqual({ evaluatedAt: "3333333", upstreamNow: "1111111" });
    expect(
      revisionNote(record({ source_revision: null, baseline_revision: null })),
    ).toEqual({ evaluatedAt: null, upstreamNow: null });
  });
});

describe("runsBySubject", () => {
  it("keeps a pending incoming run apart from the library's latest", () => {
    const incoming = record({ id: "inc", subject: "incoming", stale: true, started_at: 5 });
    const library = record({ id: "lib", started_at: 3 });
    const failed = record({ id: "f", status: "failed", started_at: 9 });
    expect(runsBySubject([failed, incoming, library])).toEqual({ library, incoming });
  });

  it("drops an incoming run older than the library's latest", () => {
    const incoming = record({ id: "inc", subject: "incoming", stale: true, started_at: 1 });
    const library = record({ id: "lib", started_at: 3 });
    expect(runsBySubject([library, incoming])).toEqual({ library, incoming: null });
  });

  it("treats an applied incoming run as the library's result", () => {
    const applied = record({ id: "inc", subject: "incoming", stale: false, started_at: 5 });
    expect(runsBySubject([applied])).toEqual({ library: applied, incoming: null });
    expect(runsBySubject(null)).toEqual({ library: null, incoming: null });
  });
});

describe("canEvaluateIncoming", () => {
  it("needs an upstream with an update waiting", () => {
    expect(canEvaluateIncoming({ source_type: "git", update_status: "update_available" })).toBe(true);
    expect(canEvaluateIncoming({ source_type: "skillssh", update_status: "update_available" })).toBe(
      true,
    );
    expect(canEvaluateIncoming({ source_type: "git", update_status: "up_to_date" })).toBe(false);
    expect(canEvaluateIncoming({ source_type: "local", update_status: "update_available" })).toBe(
      false,
    );
  });
});
