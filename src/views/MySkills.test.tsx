// @vitest-environment jsdom
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ManagedSkill } from "../lib/tauri";
import type { LocalChangeMarker } from "../lib/localChangesApi";
import { cleanup, mount } from "../test-utils/mount";
import { MySkills } from "./MySkills";

const fixture = vi.hoisted(() => ({
  skills: [] as ManagedSkill[],
  markers: {} as Record<string, LocalChangeMarker>,
}));

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, options?: Record<string, unknown>) =>
      key + (options ? JSON.stringify(options) : ""),
  }),
}));
vi.mock("react-router-dom", () => ({ useNavigate: () => vi.fn() }));
vi.mock("../context/AppContext", () => {
  const preset = { id: "preset", name: "Fixture" };
  const context = {
    viewedPreset: preset,
    activePreset: preset,
    tools: [],
    projects: [],
    detailSkillId: null,
    evaluationSummaries: {},
    refreshPresets: vi.fn(),
    refreshManagedSkills: vi.fn(),
    refreshTools: vi.fn(),
    refreshProjects: vi.fn(),
    openSkillDetailById: vi.fn(),
    closeSkillDetail: vi.fn(),
  };
  return { useApp: () => ({ ...context, managedSkills: fixture.skills }) };
});
vi.mock("../hooks/useSkillViewMode", async () => {
  const { useState } = await import("react");
  return {
    useSkillViewMode: () => {
      const [view, setView] = useState("list");
      const [grouped, setGrouped] = useState(false);
      return [view, setView, grouped, setGrouped];
    },
  };
});
vi.mock("../hooks/useLocalChangeMarkers", () => ({
  useLocalChangeMarkers: () => fixture.markers,
}));
vi.mock("../hooks/useWatchedUpstreamMarkers", () => ({
  useWatchedUpstreamMarkers: () => ({}),
}));
vi.mock("../hooks/useEvaluateCardAction", () => ({
  useEvaluateCardAction: () => vi.fn(),
}));
vi.mock("../hooks/useForkCardAction", () => ({
  useForkCardAction: () => ({ forkAction: vi.fn(), dialog: null }),
}));
vi.mock("../components/SkillDetailPanel", () => ({ SkillDetailPanel: () => null }));
vi.mock("../components/OrphanedFoldersNotice", () => ({ OrphanedFoldersNotice: () => null }));
vi.mock("../lib/tauri", async (importOriginal) => ({
  ...await importOriginal<typeof import("../lib/tauri")>(),
  getPresetSkillOrder: vi.fn(async () => []),
  gitBackupPendingConflicts: vi.fn(async () => []),
  getAllTags: vi.fn(async () => ["design", "docs"]),
  getSettings: vi.fn(async () => null),
  gitBackupStatus: vi.fn(async () => null),
}));

function skill(
  name: string,
  repo: string,
  tags: string[],
  sourceType = "git",
  enabled = true,
): ManagedSkill {
  return {
    id: name,
    name,
    description: null,
    source_type: sourceType,
    source_ref: "https://github.com/example/" + repo,
    source_ref_resolved: null,
    source_subpath: null,
    source_branch: null,
    source_revision: "installed",
    remote_revision: "installed",
    update_status: "up_to_date",
    last_checked_at: null,
    last_check_error: null,
    central_path: "/fixture/" + name,
    enabled: true,
    created_at: 0,
    updated_at: 0,
    status: "ok",
    targets: [],
    preset_ids: enabled ? ["preset"] : [],
    tags,
  };
}

function changed(...ids: string[]): Record<string, LocalChangeMarker> {
  return Object.fromEntries(ids.map((id) => [
    id,
    { skill_id: id, state: "changed", change_count: 1 },
  ]));
}

beforeEach(() => {
  fixture.skills = [
    skill("cedar", "repo-a", ["design"]),
    skill("birch", "repo-a", ["design"]),
    skill("willow", "repo-b", ["design"]),
    skill("maple", "repo-a", ["docs"], "git", false),
    skill("spruce", "repo-a", ["design"], "skillssh"),
  ];
  fixture.markers = changed("cedar", "willow", "maple", "spruce");
});

afterEach(() => cleanup());

async function render() {
  let node!: HTMLDivElement;
  await act(async () => { node = mount(<MySkills />); });
  return node;
}

function button(node: HTMLElement, text: string): HTMLButtonElement {
  const found = [...node.querySelectorAll<HTMLButtonElement>("button")]
    .find((candidate) => candidate.textContent?.trim() === text);
  expect(found, "button " + text).toBeDefined();
  return found!;
}

async function click(target: HTMLElement) {
  await act(async () => target.click());
}

function expectSkills(node: HTMLElement, names: string[]) {
  for (const { name } of fixture.skills) {
    expect(node.textContent?.includes(name), name).toBe(names.includes(name));
  }
}

describe("MySkills upstream filters with the fork's Edited filter", () => {
  it("combines Edited with channel, tag and source filters without hiding other source choices", async () => {
    const node = await render();
    await click(node.querySelector<HTMLButtonElement>('[data-testid="edited-filter-pill"]')!);
    await click(button(node, "design"));
    await click(button(node, "mySkills.sourceFilter.git"));
    expectSkills(node, ["cedar", "willow"]);

    await click(button(node, "mySkills.sourcePicker.label"));
    await click(node.querySelector<HTMLButtonElement>('button[title="example/repo-a"]')!);
    expect(node.querySelector('button[title="example/repo-b"]')).not.toBeNull();
    await click(button(node, "mySkills.sourcePicker.label (1)"));
    expectSkills(node, ["cedar"]);
  });

  it("refreshes the filtered rows when only local markers change and clears an empty Edited filter", async () => {
    const node = await render();
    await click(node.querySelector<HTMLButtonElement>('[data-testid="edited-filter-pill"]')!);
    fixture.markers = changed("birch");
    await render();
    expectSkills(node, ["birch"]);

    fixture.markers = {};
    await render();
    expectSkills(node, []);
    expect(node.querySelector('[data-testid="edited-filter-pill"]')?.getAttribute("aria-pressed")).toBe("true");
    await click(button(node, "mySkills.clearFilters"));
    expectSkills(node, fixture.skills.map(({ name }) => name));
    expect(node.querySelector('[data-testid="edited-filter-pill"]')).toBeNull();
  });

  it("combines Edited with preset status and normalized search", async () => {
    const node = await render();
    await click(node.querySelector<HTMLButtonElement>('[data-testid="edited-filter-pill"]')!);
    await click(button(node, "mySkills.filters.available"));
    expectSkills(node, ["maple"]);
    await click(button(node, "mySkills.filters.enabled"));
    expectSkills(node, ["cedar", "willow", "spruce"]);

    const input = node.querySelector<HTMLInputElement>('input[type="text"]')!;
    const setValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    await act(async () => {
      setValue.call(input, " CEDAR ");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expectSkills(node, ["cedar"]);
  });

  it("discloses selections hidden when the Edited markers change", async () => {
    const node = await render();
    await click(node.querySelector<HTMLButtonElement>('[data-testid="edited-filter-pill"]')!);
    await click(button(node, "mySkills.selectMode"));
    await click(button(node, "mySkills.selectAll"));
    fixture.markers = changed("birch");
    await render();

    expectSkills(node, ["birch"]);
    expect(node.textContent).toContain('mySkills.selectedCount{"count":4}');
    expect(node.textContent).toContain('mySkills.notShownCount{"count":4}');
  });
});
