// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SkillPinSection } from "./SkillPinSection";
import { SkillUpstreamCommits } from "./SkillUpstreamCommits";
import type { ManagedSkill } from "../lib/tauri";
import type { PinState, UpstreamCommits } from "../lib/pinningApi";

const api = vi.hoisted(() => ({
  pinSkill: vi.fn(),
  unpinSkill: vi.fn(),
  getSkillUpstreamCommits: vi.fn(),
  refreshManagedSkills: vi.fn(),
}));
const messages = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn() }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, unknown>) =>
      opts ? `${key} ${JSON.stringify(opts)}` : key,
  }),
}));
vi.mock("sonner", () => ({ toast: messages }));
vi.mock("../context/AppContext", () => ({
  useApp: () => ({ refreshManagedSkills: api.refreshManagedSkills }),
}));
vi.mock("../lib/pinningApi", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/pinningApi")>()),
  pinSkill: api.pinSkill,
  unpinSkill: api.unpinSkill,
  getSkillUpstreamCommits: api.getSkillUpstreamCommits,
}));

const REV = "a".repeat(40);

const skill = (overrides: Partial<ManagedSkill> = {}): ManagedSkill => ({
  id: "g", name: "browser", description: null, source_type: "git",
  source_ref: "https://github.com/owner/repo.git", source_ref_resolved: null,
  source_subpath: "skills/browser", source_branch: null, source_revision: REV,
  remote_revision: "b".repeat(40), update_status: "update_available",
  last_checked_at: null, last_check_error: null, central_path: "/fixture/browser",
  enabled: true, created_at: 0, updated_at: 0, status: "ok", targets: [],
  preset_ids: [], tags: [], pin: null,
  ...overrides,
});

const pin = (overrides: Partial<PinState> = {}): PinState => ({
  pinned: true, revision: REV, pinned_at: 1, note: null, ...overrides,
});

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.clearAllMocks();
  api.refreshManagedSkills.mockResolvedValue(undefined);
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

const toggle = () => container.querySelector<HTMLButtonElement>('[role="switch"]')!;

describe("SkillPinSection", () => {
  it("renders nothing for a skill with no upstream revision", async () => {
    await act(async () => root.render(<SkillPinSection skill={skill({ source_type: "local" })} />));
    expect(container.innerHTML).toBe("");
  });

  it("pins an unpinned skill and refreshes the list", async () => {
    api.pinSkill.mockResolvedValue(pin());
    await act(async () => root.render(<SkillPinSection skill={skill()} />));
    expect(toggle().getAttribute("aria-checked")).toBe("false");
    expect(container.textContent).toContain("pin.unpinnedHint");

    await act(async () => toggle().click());
    expect(api.pinSkill).toHaveBeenCalledWith("g");
    expect(api.refreshManagedSkills).toHaveBeenCalled();
    expect(toggle().getAttribute("aria-checked")).toBe("true");
    expect(container.textContent).toContain('pin.pinnedAt {"revision":"aaaaaaa"}');
    expect(container.textContent).toContain("pin.updateAvailable");
  });

  it("shows the pinned revision and note, and unpins", async () => {
    api.unpinSkill.mockResolvedValue(pin({ pinned: false, revision: null }));
    await act(async () =>
      root.render(<SkillPinSection skill={skill({ pin: pin({ note: "reviewed" }) })} />),
    );
    expect(toggle().getAttribute("aria-checked")).toBe("true");
    expect(container.textContent).toContain("reviewed");

    await act(async () => toggle().click());
    expect(api.unpinSkill).toHaveBeenCalledWith("g");
    expect(toggle().getAttribute("aria-checked")).toBe("false");
    expect(messages.success).toHaveBeenCalled();
  });

  it("keeps the state and reports the error when the call fails", async () => {
    api.pinSkill.mockRejectedValue(new Error("busy"));
    await act(async () => root.render(<SkillPinSection skill={skill()} />));
    await act(async () => toggle().click());
    expect(toggle().getAttribute("aria-checked")).toBe("false");
    expect(messages.error).toHaveBeenCalled();
  });
});

const commits = (overrides: Partial<UpstreamCommits> = {}): UpstreamCommits => ({
  skill_id: "g", base_revision: REV, base_from: "baseline", remote_revision: "b".repeat(40),
  subpath: "skills/browser", reachable: true, message: null, truncated: false,
  commits: [
    { sha: "c".repeat(40), date: "2026-11-01T10:00:00+00:00", author: "ann", subject: "browser: retry" },
  ],
  ...overrides,
});

describe("SkillUpstreamCommits", () => {
  it("lists the commits since the baseline", async () => {
    api.getSkillUpstreamCommits.mockResolvedValue(commits());
    await act(async () => root.render(<SkillUpstreamCommits skillId="g" />));
    expect(api.getSkillUpstreamCommits).toHaveBeenCalledWith("g");
    expect(container.textContent).toContain("ccccccc");
    expect(container.textContent).toContain("browser: retry");
    expect(container.textContent).toContain("ann");
  });

  it("says plainly when the baseline cannot be reached", async () => {
    api.getSkillUpstreamCommits.mockResolvedValue(
      commits({ reachable: false, commits: [], message: "The baseline revision is gone" }),
    );
    await act(async () => root.render(<SkillUpstreamCommits skillId="g" />));
    expect(container.textContent).toContain("The baseline revision is gone");
  });

  it("says when nothing upstream touched the skill", async () => {
    api.getSkillUpstreamCommits.mockResolvedValue(commits({ commits: [] }));
    await act(async () => root.render(<SkillUpstreamCommits skillId="g" />));
    expect(container.textContent).toContain("pin.commits.none");
  });
});
