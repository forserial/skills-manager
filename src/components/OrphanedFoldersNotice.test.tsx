// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { OrphanedFoldersNotice } from "./OrphanedFoldersNotice";
import type { OrphanedFolder } from "../lib/orphanedFoldersApi";

const api = vi.hoisted(() => ({
  getOrphanedFolders: vi.fn(),
  adoptOrphanedFolder: vi.fn(),
}));
const toast = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn() }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, options?: { count?: number; name?: string }) =>
      options?.count !== undefined
        ? `${key}:${options.count}`
        : options?.name !== undefined
          ? `${key}:${options.name}`
          : key,
  }),
}));
vi.mock("sonner", () => ({ toast }));
vi.mock("../lib/orphanedFoldersApi", () => api);

const folder = (overrides: Partial<OrphanedFolder> = {}): OrphanedFolder => ({
  dir_name: "lost-skill",
  path: "/lib/lost-skill",
  has_skill_md: true,
  name: "lost",
  size: 2048,
  modified: 1_700_000_000_000,
  kind: "unmanaged",
  note: null,
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

const render = async (onAdopted = vi.fn()) => {
  await act(async () => root.render(<OrphanedFoldersNotice skills={[]} onAdopted={onAdopted} />));
  return onAdopted;
};
const notice = () => container.querySelector('[data-testid="orphaned-folders-notice"]');
const rows = () => [...container.querySelectorAll('[data-testid="orphaned-folder-row"]')];
const buttonIn = (el: Element | null | undefined, text: string) =>
  [...(el?.querySelectorAll("button") ?? [])].find((b) => b.textContent?.includes(text)) as
    | HTMLButtonElement
    | undefined;
const openList = async () => {
  await act(async () => buttonIn(notice(), "orphanedFolders.review")!.click());
};

describe("OrphanedFoldersNotice", () => {
  it("renders nothing when every folder is managed", async () => {
    api.getOrphanedFolders.mockResolvedValue([]);
    await render();
    expect(container.innerHTML).toBe("");
  });

  it("says how many folders are not managed", async () => {
    api.getOrphanedFolders.mockResolvedValue([folder(), folder({ dir_name: "notes", has_skill_md: false, name: null })]);
    await render();
    expect(notice()?.textContent).toContain("orphanedFolders.notice:2");
  });

  it("lists the folders with the warning about what cannot be recovered", async () => {
    api.getOrphanedFolders.mockResolvedValue([folder(), folder({ dir_name: "notes", has_skill_md: false, name: null })]);
    await render();
    await openList();

    const warning = container.querySelector('[data-testid="orphaned-folders-warning"]');
    expect(warning?.textContent).toContain("orphanedFolders.warningLost");
    expect(warning?.textContent).toContain("orphanedFolders.warningSource");
    expect(rows().map((r) => r.querySelector("p")?.textContent)).toEqual(["lost-skill(lost)", "notes"]);
    expect(rows()[0].textContent).toContain("2.0 KB");
    // A folder without SKILL.md is listed, but cannot be adopted.
    expect(rows()[1].textContent).toContain("orphanedFolders.noSkillMd");
    expect(buttonIn(rows()[1], "orphanedFolders.adopt")!.disabled).toBe(true);
    expect(buttonIn(rows()[0], "orphanedFolders.adopt")!.disabled).toBe(false);
  });

  it("shows an interrupted install's hidden copies with their kind, never adoptable", async () => {
    api.getOrphanedFolders.mockResolvedValue([
      folder({ dir_name: ".keep.staged-1", name: "keep", kind: "staged", note: "may still be in use" }),
      folder({ dir_name: ".keep.backup-2", name: "keep", kind: "backup", note: "an old copy" }),
    ]);
    await render();
    await openList();

    const kinds = rows().map((r) => r.querySelector('[data-testid="orphaned-folder-leftover"]')?.textContent);
    expect(kinds).toEqual(["orphanedFolders.staged", "orphanedFolders.backup"]);
    for (const row of rows()) {
      const adopt = buttonIn(row, "orphanedFolders.adopt")!;
      expect(adopt.disabled).toBe(true);
    }
    expect(buttonIn(rows()[0], "orphanedFolders.adopt")!.title).toBe("may still be in use");
  });

  it("adopts a folder, reloads the skills and drops it from the list", async () => {
    api.getOrphanedFolders.mockResolvedValue([folder(), folder({ dir_name: "other", name: "other" })]);
    api.adoptOrphanedFolder.mockResolvedValue({
      skill_id: "new-id",
      name: "lost",
      dir_name: "lost-skill",
      central_path: "/lib/lost-skill",
    });
    const onAdopted = await render();
    await openList();

    await act(async () => buttonIn(rows()[0], "orphanedFolders.adopt")!.click());

    expect(api.adoptOrphanedFolder).toHaveBeenCalledWith("lost-skill");
    expect(onAdopted).toHaveBeenCalledTimes(1);
    expect(toast.success).toHaveBeenCalledWith("orphanedFolders.adopted:lost");
    expect(rows().map((r) => r.querySelector("p")?.textContent)).toEqual(["other"]);
    expect(notice()?.textContent).toContain("orphanedFolders.notice:1");
  });

  it("shows the backend's refusal and keeps the folder", async () => {
    api.getOrphanedFolders.mockResolvedValue([folder()]);
    api.adoptOrphanedFolder.mockRejectedValue({ kind: "invalid_input", message: "'lost-skill' has no SKILL.md" });
    const onAdopted = await render();
    await openList();

    await act(async () => buttonIn(rows()[0], "orphanedFolders.adopt")!.click());

    expect(toast.error).toHaveBeenCalledTimes(1);
    expect(onAdopted).not.toHaveBeenCalled();
    expect(rows()).toHaveLength(1);
  });
});
