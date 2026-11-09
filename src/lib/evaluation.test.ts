import { describe, expect, it } from "vitest";
import type { ManagedSkill } from "./tauri";
import type { EvaluationProgress, EvaluationSummary } from "./evaluationApi";
import {
  MAX_PROGRESS_LINES,
  checkPositiveNumberSetting,
  evaluationRunsReducer,
  evaluatorCandidates,
  formatCost,
  formatDuration,
  formatScore,
  gradeBadgeProps,
  gradeTone,
  groupFindingsBySeverity,
  hasSkillJudge,
  isRunActive,
  pickAgent,
  pickEvaluator,
  scorePercent,
  type EvaluationRuns,
} from "./evaluation";

function progress(over: Partial<EvaluationProgress>): EvaluationProgress {
  return {
    run_id: "run-1",
    skill_id: "skill-a",
    phase: "progress",
    detail: null,
    evaluation_id: null,
    error: null,
    ...over,
  };
}

const at = 1000;

describe("evaluationRunsReducer", () => {
  it("tracks a run from request to finish", () => {
    let state: EvaluationRuns = {};
    state = evaluationRunsReducer(state, { type: "requested", skillId: "skill-a", at });
    expect(state["skill-a"]).toMatchObject({ runId: null, phase: "starting", startedAt: at });
    expect(isRunActive(state["skill-a"])).toBe(true);

    state = evaluationRunsReducer(state, { type: "started", skillId: "skill-a", runId: "run-1" });
    expect(state["skill-a"].runId).toBe("run-1");

    state = evaluationRunsReducer(state, { type: "progress", event: progress({ phase: "started" }), at });
    expect(state["skill-a"].phase).toBe("started");
    state = evaluationRunsReducer(state, {
      type: "progress",
      event: progress({ detail: "Reading SKILL.md" }),
      at,
    });
    expect(state["skill-a"].lines).toEqual(["Reading SKILL.md"]);

    state = evaluationRunsReducer(state, {
      type: "progress",
      event: progress({ phase: "finished", evaluation_id: "eval-1" }),
      at,
    });
    expect(state["skill-a"]).toMatchObject({ phase: "finished", evaluationId: "eval-1", error: null });
    expect(isRunActive(state["skill-a"])).toBe(false);
  });

  it("adopts the run id from an event that beats the invoke result", () => {
    let state = evaluationRunsReducer({}, { type: "requested", skillId: "skill-a", at });
    state = evaluationRunsReducer(state, {
      type: "progress",
      event: progress({ phase: "failed", error: "binary missing" }),
      at,
    });
    expect(state["skill-a"]).toMatchObject({ runId: "run-1", phase: "failed", error: "binary missing" });
    // The late invoke result must not reopen the finished run.
    const after = evaluationRunsReducer(state, { type: "started", skillId: "skill-a", runId: "run-1" });
    expect(after).toBe(state);
  });

  it("keeps only the last progress lines", () => {
    let state = evaluationRunsReducer({}, { type: "requested", skillId: "skill-a", at });
    for (let i = 0; i < MAX_PROGRESS_LINES + 5; i++) {
      state = evaluationRunsReducer(state, { type: "progress", event: progress({ detail: `line ${i}` }), at });
    }
    const lines = state["skill-a"].lines;
    expect(lines).toHaveLength(MAX_PROGRESS_LINES);
    expect(lines[0]).toBe("line 5");
    expect(lines.at(-1)).toBe(`line ${MAX_PROGRESS_LINES + 4}`);
  });

  it("ignores events of another run while one is active, and events after the end", () => {
    let state = evaluationRunsReducer({}, { type: "requested", skillId: "skill-a", at });
    state = evaluationRunsReducer(state, { type: "started", skillId: "skill-a", runId: "run-1" });
    const stray = evaluationRunsReducer(state, {
      type: "progress",
      event: progress({ run_id: "run-0", phase: "failed", error: "old" }),
      at,
    });
    expect(stray).toBe(state);

    state = evaluationRunsReducer(state, { type: "progress", event: progress({ phase: "cancelled" }), at });
    const late = evaluationRunsReducer(state, { type: "progress", event: progress({ detail: "late" }), at });
    expect(late).toBe(state);
  });

  it("does not start a second run while one is active", () => {
    const state = evaluationRunsReducer({}, { type: "requested", skillId: "skill-a", at });
    expect(evaluationRunsReducer(state, { type: "requested", skillId: "skill-a", at: at + 1 })).toBe(state);
  });

  it("records a refused start as failed", () => {
    let state = evaluationRunsReducer({}, { type: "requested", skillId: "skill-a", at });
    state = evaluationRunsReducer(state, { type: "startFailed", skillId: "skill-a", error: "already running" });
    expect(state["skill-a"]).toMatchObject({ phase: "failed", error: "already running" });
  });

  it("picks up a run it did not start (e.g. after a reload)", () => {
    const state = evaluationRunsReducer({}, {
      type: "progress",
      event: progress({ run_id: "run-9", detail: "working" }),
      at,
    });
    expect(state["skill-a"]).toMatchObject({ runId: "run-9", phase: "progress", lines: ["working"] });
  });
});

function skill(name: string, over: Partial<ManagedSkill> = {}): ManagedSkill {
  return {
    id: `id-${name}`,
    name,
    description: null,
    source_type: "local",
    source_ref: null,
    source_ref_resolved: null,
    source_subpath: null,
    source_branch: null,
    source_revision: null,
    remote_revision: null,
    update_status: "local_only",
    last_checked_at: null,
    last_check_error: null,
    central_path: `C:\\Users\\me\\.skills-manager\\skills\\${name}`,
    enabled: true,
    created_at: 0,
    updated_at: 0,
    status: "ok",
    targets: [],
    preset_ids: [],
    tags: [],
    ...over,
  };
}

describe("evaluators", () => {
  it("offers skill-judge first, then skills tagged evaluator", () => {
    const skills = [
      skill("zeta-judge", { tags: ["evaluator"] }),
      skill("plain"),
      skill("alpha-judge", { tags: ["Evaluator", "x"] }),
      skill("skill-judge"),
    ];
    expect(evaluatorCandidates(skills)).toEqual(["skill-judge", "alpha-judge", "zeta-judge"]);
  });

  it("finds skill-judge by directory name, as the backend does", () => {
    const renamed = skill("Skill Judge", {
      central_path: "/home/me/.skills-manager/skills/skill-judge",
    });
    expect(hasSkillJudge([renamed])).toBe(true);
    expect(hasSkillJudge([skill("other")])).toBe(false);
    expect(evaluatorCandidates([skill("other")])).toEqual([]);
  });

  it("preselects the configured evaluator only when it is offered", () => {
    expect(pickEvaluator(["skill-judge", "mine"], "mine")).toBe("mine");
    expect(pickEvaluator(["skill-judge", "mine"], "gone")).toBe("skill-judge");
    expect(pickEvaluator(["mine"], null)).toBe("mine");
    expect(pickEvaluator([], "mine")).toBe("");
  });

  it("preselects the configured agent, else Claude Code", () => {
    expect(pickAgent(["claude_code", "codex"], "codex")).toBe("codex");
    expect(pickAgent(["claude_code", "codex"], "nope")).toBe("claude_code");
    expect(pickAgent(["codex"], null)).toBe("codex");
  });
});

describe("groupFindingsBySeverity", () => {
  it("orders groups by severity and keeps unknown severities last", () => {
    const groups = groupFindingsBySeverity([
      { severity: "minor", title: "m1" },
      { severity: "weird", title: "w" },
      { severity: "Critical", title: "c" },
      { severity: "minor", title: "m2" },
    ]);
    expect(groups.map((g) => g.severity)).toEqual(["critical", "minor", "other"]);
    expect(groups[1].findings.map((f) => f.title)).toEqual(["m1", "m2"]);
  });

  it("handles a missing list", () => {
    expect(groupFindingsBySeverity(undefined)).toEqual([]);
  });
});

describe("formatting", () => {
  it("formats scores", () => {
    expect(formatScore(97, 120)).toBe("97/120");
    expect(formatScore(7.25, 10)).toBe("7.3/10");
    expect(formatScore(5, null)).toBe("5");
    expect(formatScore(null, 10)).toBeNull();
  });

  it("clamps bar widths", () => {
    expect(scorePercent(15, 20)).toBe(75);
    expect(scorePercent(30, 20)).toBe(100);
    expect(scorePercent(-1, 20)).toBe(0);
    expect(scorePercent(5, 0)).toBe(0);
    expect(scorePercent(undefined, 20)).toBe(0);
  });

  it("formats durations and costs", () => {
    expect(formatDuration(42_400)).toBe("42s");
    expect(formatDuration(160_000)).toBe("2m 40s");
    expect(formatDuration(120_000)).toBe("2m");
    expect(formatDuration(null)).toBe("—");
    expect(formatCost(1.0249)).toBe("$1.02");
    expect(formatCost(null)).toBe("—");
  });
});

describe("checkPositiveNumberSetting", () => {
  it("accepts empty as unset and positive values", () => {
    expect(checkPositiveNumberSetting("  ", true)).toEqual({ ok: true, value: "" });
    expect(checkPositiveNumberSetting(" 600 ", true)).toEqual({ ok: true, value: "600" });
    expect(checkPositiveNumberSetting("0.5", false)).toEqual({ ok: true, value: "0.5" });
  });

  it("refuses what the backend would refuse", () => {
    expect(checkPositiveNumberSetting("0", true).ok).toBe(false);
    expect(checkPositiveNumberSetting("1.5", true).ok).toBe(false);
    expect(checkPositiveNumberSetting("ten", true).ok).toBe(false);
    expect(checkPositiveNumberSetting("-1", false).ok).toBe(false);
    expect(checkPositiveNumberSetting("0.0", false).ok).toBe(false);
  });
});

describe("gradeTone", () => {
  it.each([
    ["A", "excellent"],
    ["a+", "excellent"],
    ["S", "excellent"],
    ["B-", "good"],
    [" C ", "fair"],
    ["D", "poor"],
    ["F", "poor"],
  ] as const)("puts %s in the %s band", (grade, tone) => {
    expect(gradeTone(grade)).toBe(tone);
  });

  it.each(["Excellent", "77", "", "A++", null, undefined])("leaves %s neutral", (grade) => {
    expect(gradeTone(grade)).toBe("neutral");
  });
});

describe("gradeBadgeProps", () => {
  const summary = (over: Partial<EvaluationSummary>): EvaluationSummary => ({
    skill_id: "skill-a",
    evaluation_id: "e1",
    grade: "B",
    score_value: 77,
    score_max: 120,
    stale: false,
    finished_at: 1_700_000_000_000,
    ...over,
  });

  it("is null without a summary", () => {
    expect(gradeBadgeProps(undefined)).toBeNull();
    expect(gradeBadgeProps(null)).toBeNull();
  });

  it("shows the grade, with the score for the tooltip", () => {
    expect(gradeBadgeProps(summary({}))).toEqual({
      label: "B",
      tone: "good",
      dimmed: false,
      score: "77/120",
      grade: "B",
      finishedAt: 1_700_000_000_000,
    });
  });

  it("falls back to the score, in the neutral band, when there is no grade", () => {
    const badge = gradeBadgeProps(summary({ grade: null }));
    expect(badge).toMatchObject({ label: "77/120", tone: "neutral", grade: null });
    expect(gradeBadgeProps(summary({ grade: "  " }))?.label).toBe("77/120");
  });

  it("is dimmed when the evaluation is stale", () => {
    expect(gradeBadgeProps(summary({ stale: true }))?.dimmed).toBe(true);
  });

  it("is null with neither grade nor score", () => {
    expect(gradeBadgeProps(summary({ grade: null, score_value: null, score_max: null }))).toBeNull();
  });
});