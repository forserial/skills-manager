// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SkillLocalChangesSection } from "./SkillLocalChangesSection";
import { LocalChangeMarker } from "./LocalChangeMarker";
import type { ManagedSkill } from "../lib/tauri";
import type { LocalChanges, LocalDiff } from "../lib/localChangesApi";

const api = vi.hoisted(() => ({
  getSkillLocalChanges: vi.fn(),
  getSkillLocalDiff: vi.fn(),
}));
const messages = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn() }));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("sonner", () => ({ toast: messages }));
vi.mock("../lib/localChangesApi", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/localChangesApi")>()),
  getSkillLocalChanges: api.getSkillLocalChanges,
  getSkillLocalDiff: api.getSkillLocalDiff,
}));

const skill: ManagedSkill = {
  id: "demo", name: "demo", description: null, source_type: "git",
  source_ref: "https://github.com/owner/repo.git", source_ref_resolved: null,
  source_subpath: "skills/demo", source_branch: null, source_revision: "1".repeat(40),
  remote_revision: null, update_status: "up_to_date", last_checked_at: null,
  last_check_error: null, central_path: "/fixture/demo", enabled: true, created_at: 0,
  updated_at: 0, status: "ok", targets: [], preset_ids: [], tags: [],
};

const changes = (overrides: Partial<LocalChanges> = {}): LocalChanges => ({
  skill_id: "demo",
  state: "changed",
  baseline_revision: "1".repeat(40),
  files: [
    { path: "SKILL.md", state: "modified" },
    { path: "refs/gone.md", state: "deleted" },
    { path: "refs/new.md", state: "added" },
  ],
  ...overrides,
});

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.clearAllMocks();
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

const render = async () => {
  await act(async () => root.render(<SkillLocalChangesSection skill={skill} />));
};
const button = (label: string) =>
  [...container.querySelectorAll("button")].find((b) => b.textContent?.includes(label));

describe("SkillLocalChangesSection", () => {
  it("lists each changed file with its state", async () => {
    api.getSkillLocalChanges.mockResolvedValue(changes());
    await render();
    expect(api.getSkillLocalChanges).toHaveBeenCalledWith("demo");
    expect(container.textContent).toContain("localChanges.title");
    const rows = [...container.querySelectorAll("li")].map((li) => li.textContent);
    expect(rows).toEqual([
      "localChanges.state.modifiedSKILL.md",
      "localChanges.state.deletedrefs/gone.md",
      "localChanges.state.addedrefs/new.md",
    ]);
  });

  it("renders nothing for a clean skill or a local skill", async () => {
    api.getSkillLocalChanges.mockResolvedValue(changes({ state: "clean", files: [] }));
    await render();
    expect(container.innerHTML).toBe("");

    api.getSkillLocalChanges.mockResolvedValue(changes({ state: "untracked", files: [] }));
    await act(async () => root.render(<SkillLocalChangesSection skill={{ ...skill, id: "x" }} />));
    expect(container.innerHTML).toBe("");
  });

  it("says when the baseline is unknown or suspect", async () => {
    api.getSkillLocalChanges.mockResolvedValue(
      changes({ state: "baseline_unknown", files: [], baseline_revision: null }),
    );
    await render();
    expect(container.textContent).toContain("localChanges.unknownHint");

    api.getSkillLocalChanges.mockResolvedValue(changes({ state: "baseline_suspect", files: [] }));
    await act(async () => root.render(<SkillLocalChangesSection skill={{ ...skill, id: "y" }} />));
    expect(container.textContent).toContain("localChanges.suspectHint");
  });

  it("loads the diff on demand and toggles it", async () => {
    api.getSkillLocalChanges.mockResolvedValue(changes());
    const diff: LocalDiff = {
      ...changes({ files: [{ path: "SKILL.md", state: "modified" }] }),
      base_source: "base_store",
      entries: [
        {
          relative_path: "SKILL.md",
          status: "modified",
          content_kind: "text",
          original_text: "v1\n",
          updated_text: "mine\n",
          executable_before: false,
          executable_after: false,
        },
      ],
      message: null,
    };
    api.getSkillLocalDiff.mockResolvedValue(diff);
    await render();
    await act(async () => button("localChanges.showDiff")!.click());
    expect(api.getSkillLocalDiff).toHaveBeenCalledWith("demo");
    expect(container.textContent).toContain("mine");
    expect(button("localChanges.hideDiff")).toBeTruthy();

    await act(async () => button("localChanges.hideDiff")!.click());
    expect(container.textContent).not.toContain("mine");
    // Opening again reuses the loaded diff.
    await act(async () => button("localChanges.showDiff")!.click());
    expect(api.getSkillLocalDiff).toHaveBeenCalledTimes(1);
  });

  it("lists paths only when the baseline content is unavailable", async () => {
    api.getSkillLocalChanges.mockResolvedValue(changes());
    api.getSkillLocalDiff.mockResolvedValue({
      ...changes(),
      base_source: null,
      entries: null,
      message: "baseline content unavailable",
    } satisfies LocalDiff);
    await render();
    await act(async () => button("localChanges.showDiff")!.click());
    expect(container.textContent).toContain("localChanges.diffUnavailable");
    expect(container.querySelectorAll("li")).toHaveLength(3);
  });
});

describe("LocalChangeMarker", () => {
  const renderMarker = async (marker: Parameters<typeof LocalChangeMarker>[0]["marker"]) => {
    await act(async () => root.render(<LocalChangeMarker marker={marker} />));
  };

  it("marks edited skills and, distinctly, unknown baselines", async () => {
    await renderMarker({ skill_id: "demo", state: "changed", change_count: 2 });
    expect(container.querySelector('[data-testid="local-change-marker"]')).toBeTruthy();
    expect(container.textContent).toBe("localChanges.markerChanged");

    await renderMarker({ skill_id: "demo", state: "baseline_unknown", change_count: null });
    expect(container.querySelector('[data-testid="local-change-marker"]')).toBeNull();
    expect(container.querySelector('[data-testid="baseline-unknown-marker"]')?.getAttribute("title"))
      .toBe("localChanges.unknownHint");

    await renderMarker({ skill_id: "demo", state: "baseline_suspect", change_count: null });
    expect(container.querySelector('[data-testid="baseline-unknown-marker"]')?.getAttribute("title"))
      .toBe("localChanges.suspectHint");
  });

  it("renders nothing without a marker", async () => {
    await renderMarker(undefined);
    expect(container.innerHTML).toBe("");
  });
});
