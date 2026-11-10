// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SkillLocalChangesSection } from "./SkillLocalChangesSection";
import { LocalChangeMarker } from "./LocalChangeMarker";
import type { ManagedSkill } from "../lib/tauri";
import type { LocalDiff } from "../lib/localChangesApi";
import type { ChangesReport } from "../lib/changeNotesApi";

const api = vi.hoisted(() => ({
  getSkillChangeNotes: vi.fn(),
  setSkillChangeNote: vi.fn(),
  exportSkillLocalPatch: vi.fn(),
  getSkillLocalDiff: vi.fn(),
  save: vi.fn(),
}));
const messages = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn() }));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("sonner", () => ({ toast: messages }));
vi.mock("../lib/localChangesApi", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/localChangesApi")>()),
  getSkillLocalDiff: api.getSkillLocalDiff,
}));
vi.mock("../lib/changeNotesApi", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/changeNotesApi")>()),
  getSkillChangeNotes: api.getSkillChangeNotes,
  setSkillChangeNote: api.setSkillChangeNote,
  exportSkillLocalPatch: api.exportSkillLocalPatch,
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: api.save }));

const skill: ManagedSkill = {
  id: "demo", name: "demo", description: null, source_type: "git",
  source_ref: "https://github.com/owner/repo.git", source_ref_resolved: null,
  source_subpath: "skills/demo", source_branch: null, source_revision: "1".repeat(40),
  remote_revision: null, update_status: "up_to_date", last_checked_at: null,
  last_check_error: null, central_path: "/fixture/demo", enabled: true, created_at: 0,
  updated_at: 0, status: "ok", targets: [], preset_ids: [], tags: [],
};

const noted = (path: string, state: "modified" | "added" | "deleted", note: string | null = null) => ({
  path,
  state,
  note,
  note_updated_at: note ? 1 : null,
});

const changes = (overrides: Partial<ChangesReport> = {}): ChangesReport => ({
  skill_id: "demo",
  name: "demo",
  state: "changed",
  baseline_revision: "1".repeat(40),
  files: [
    noted("SKILL.md", "modified"),
    noted("refs/gone.md", "deleted"),
    noted("refs/new.md", "added"),
  ],
  held_notes: [],
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
    api.getSkillChangeNotes.mockResolvedValue(changes());
    await render();
    expect(api.getSkillChangeNotes).toHaveBeenCalledWith("demo");
    expect(container.textContent).toContain("localChanges.title");
    const rows = [...container.querySelectorAll("li")].map((li) => li.textContent);
    expect(rows).toEqual([
      "localChanges.state.modifiedSKILL.md",
      "localChanges.state.deletedrefs/gone.md",
      "localChanges.state.addedrefs/new.md",
    ]);
  });

  it("renders nothing for a clean skill or a local skill", async () => {
    api.getSkillChangeNotes.mockResolvedValue(changes({ state: "clean", files: [] }));
    await render();
    expect(container.innerHTML).toBe("");

    api.getSkillChangeNotes.mockResolvedValue(changes({ state: "untracked", files: [] }));
    await act(async () => root.render(<SkillLocalChangesSection skill={{ ...skill, id: "x" }} />));
    expect(container.innerHTML).toBe("");
  });

  it("says when the baseline is unknown or suspect", async () => {
    api.getSkillChangeNotes.mockResolvedValue(
      changes({ state: "baseline_unknown", files: [], baseline_revision: null }),
    );
    await render();
    expect(container.textContent).toContain("localChanges.unknownHint");

    api.getSkillChangeNotes.mockResolvedValue(changes({ state: "baseline_suspect", files: [] }));
    await act(async () => root.render(<SkillLocalChangesSection skill={{ ...skill, id: "y" }} />));
    expect(container.textContent).toContain("localChanges.suspectHint");
  });

  it("loads the diff on demand and toggles it", async () => {
    api.getSkillChangeNotes.mockResolvedValue(changes());
    const diff: LocalDiff = {
      skill_id: "demo",
      state: "changed",
      baseline_revision: "1".repeat(40),
      files: [{ path: "SKILL.md", state: "modified" }],
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
    api.getSkillChangeNotes.mockResolvedValue(changes());
    api.getSkillLocalDiff.mockResolvedValue({
      skill_id: "demo",
      state: "changed",
      baseline_revision: "1".repeat(40),
      files: [],
      base_source: null,
      entries: null,
      message: "baseline content unavailable",
    } satisfies LocalDiff);
    await render();
    await act(async () => button("localChanges.showDiff")!.click());
    expect(container.textContent).toContain("localChanges.diffUnavailable");
    expect(container.querySelectorAll("li")).toHaveLength(3);
  });

  it("shows each file's note", async () => {
    api.getSkillChangeNotes.mockResolvedValue(
      changes({ files: [noted("SKILL.md", "modified", "removed the usage ping"), noted("b.md", "added")] }),
    );
    await render();
    const notes = [...container.querySelectorAll('[data-testid="change-note"]')];
    expect(notes.map((n) => n.textContent)).toEqual(["removed the usage ping"]);
    const edits = [...container.querySelectorAll('[data-testid="change-note-edit"]')];
    expect(edits.map((b) => b.getAttribute("aria-label"))).toEqual([
      "localChanges.editNote",
      "localChanges.addNote",
    ]);
  });

  const typeInto = async (input: HTMLInputElement, value: string) => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    await act(async () => {
      setter.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
  };
  const press = async (input: HTMLInputElement, key: string) => {
    await act(async () => {
      input.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
    });
  };
  const editButton = () =>
    container.querySelector<HTMLButtonElement>('[data-testid="change-note-edit"]')!;
  const noteInput = () =>
    container.querySelector<HTMLInputElement>('[data-testid="change-note-input"]');

  it("edits a note and saves it on Enter", async () => {
    api.getSkillChangeNotes.mockResolvedValue(changes());
    api.setSkillChangeNote.mockResolvedValue(
      changes({ files: [noted("SKILL.md", "modified", "Windows paths")] }),
    );
    await render();
    await act(async () => editButton().click());
    expect(noteInput()!.value).toBe("");
    await typeInto(noteInput()!, "  Windows paths ");
    await press(noteInput()!, "Enter");
    expect(api.setSkillChangeNote).toHaveBeenCalledWith("demo", "SKILL.md", "Windows paths");
    expect(noteInput()).toBeNull();
    expect(container.querySelector('[data-testid="change-note"]')?.textContent).toBe("Windows paths");
  });

  it("clears a note saved empty, and Escape cancels an edit", async () => {
    api.getSkillChangeNotes.mockResolvedValue(
      changes({ files: [noted("SKILL.md", "modified", "old reason")] }),
    );
    api.setSkillChangeNote.mockResolvedValue(changes({ files: [noted("SKILL.md", "modified")] }));
    await render();

    await act(async () => editButton().click());
    expect(noteInput()!.value).toBe("old reason");
    await typeInto(noteInput()!, "something else");
    await press(noteInput()!, "Escape");
    expect(api.setSkillChangeNote).not.toHaveBeenCalled();
    expect(container.querySelector('[data-testid="change-note"]')?.textContent).toBe("old reason");

    await act(async () => editButton().click());
    await typeInto(noteInput()!, "   ");
    await act(async () => button("common.save")!.click());
    expect(api.setSkillChangeNote).toHaveBeenCalledWith("demo", "SKILL.md", null);
    expect(container.querySelector('[data-testid="change-note"]')).toBeNull();
  });

  it("keeps the edit open and reports a refused note", async () => {
    api.getSkillChangeNotes.mockResolvedValue(changes());
    api.setSkillChangeNote.mockRejectedValue(new Error("SKILL.md does not differ"));
    await render();
    await act(async () => editButton().click());
    await typeInto(noteInput()!, "why");
    await press(noteInput()!, "Enter");
    expect(messages.error).toHaveBeenCalled();
    expect(noteInput()).toBeTruthy();
  });

  it("exports the patch to the chosen file", async () => {
    api.getSkillChangeNotes.mockResolvedValue(changes());
    api.save.mockResolvedValue("/tmp/demo.patch");
    api.exportSkillLocalPatch.mockResolvedValue({
      skill_id: "demo",
      name: "demo",
      baseline_revision: "1".repeat(40),
      base_source: "base_store",
      path_prefix: "skills/demo",
      file_count: 3,
      noted_file_count: 0,
    });
    await render();
    await act(async () => button("localChanges.exportPatch")!.click());
    expect(api.save).toHaveBeenCalledWith(
      expect.objectContaining({ defaultPath: "demo-local-changes.patch" }),
    );
    expect(api.exportSkillLocalPatch).toHaveBeenCalledWith("demo", "/tmp/demo.patch");
    expect(messages.success).toHaveBeenCalledWith("localChanges.exported");
  });

  it("does nothing when the save dialog is cancelled, and reports a failed export", async () => {
    api.getSkillChangeNotes.mockResolvedValue(changes());
    api.save.mockResolvedValue(null);
    await render();
    await act(async () => button("localChanges.exportPatch")!.click());
    expect(api.exportSkillLocalPatch).not.toHaveBeenCalled();

    api.save.mockResolvedValue("/tmp/demo.patch");
    api.exportSkillLocalPatch.mockRejectedValue(new Error("baseline content could not be obtained"));
    await act(async () => button("localChanges.exportPatch")!.click());
    expect(messages.error).toHaveBeenCalled();
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
