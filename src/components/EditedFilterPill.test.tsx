// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { EditedFilterPill } from "./EditedFilterPill";
import { countEdited, isEdited } from "../lib/localStateFilter";
import type { LocalChangeMarker } from "../lib/localChangesApi";

vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const marker = (skill_id: string, state: LocalChangeMarker["state"]): LocalChangeMarker => ({
  skill_id,
  state,
  change_count: state === "changed" ? 1 : null,
});

describe("localStateFilter", () => {
  it("counts only changed skills as edited", () => {
    expect(isEdited(marker("a", "changed"))).toBe(true);
    for (const state of ["clean", "baseline_unknown", "baseline_suspect", "untracked"] as const) {
      expect(isEdited(marker("a", state))).toBe(false);
    }
    expect(isEdited(undefined)).toBe(false);
  });

  it("counts edited skills that are still in the list", () => {
    const markers = {
      a: marker("a", "changed"),
      b: marker("b", "clean"),
      c: marker("c", "baseline_unknown"),
      gone: marker("gone", "changed"),
    };
    expect(countEdited([{ id: "a" }, { id: "b" }, { id: "c" }, { id: "d" }], markers)).toBe(1);
    expect(countEdited([], markers)).toBe(0);
  });
});

describe("EditedFilterPill", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  const render = async (count: number, active: boolean, onToggle = vi.fn()) => {
    await act(async () => root.render(<EditedFilterPill count={count} active={active} onToggle={onToggle} />));
    return container.querySelector<HTMLButtonElement>('[data-testid="edited-filter-pill"]');
  };

  it("shows the label and count, and toggles", async () => {
    const onToggle = vi.fn();
    const pill = await render(3, false, onToggle);
    expect(pill?.textContent).toBe("mySkills.editedFilter.label3");
    expect(pill?.getAttribute("aria-pressed")).toBe("false");
    await act(async () => pill!.click());
    expect(onToggle).toHaveBeenCalledTimes(1);

    expect((await render(3, true))?.getAttribute("aria-pressed")).toBe("true");
  });

  it("hides when nothing is edited, unless it is on", async () => {
    expect(await render(0, false)).toBeNull();
    expect(container.innerHTML).toBe("");

    // Left on after the last edit was reverted: it must stay, so it can be turned off.
    const pill = await render(0, true);
    expect(pill).toBeTruthy();
    expect(pill?.getAttribute("aria-pressed")).toBe("true");
  });
});
