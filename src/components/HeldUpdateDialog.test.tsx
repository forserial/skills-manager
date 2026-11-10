// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HeldUpdateDialog } from "./HeldUpdateDialog";
import type { PendingOverwrite, PendingRemoval } from "../lib/tauri";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, unknown>) =>
      opts ? `${key} ${JSON.stringify(opts)}` : key,
  }),
}));

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

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

const removal = (path: string, location = "library"): PendingRemoval => ({ location, path });
const overwrite = (path: string): PendingOverwrite => ({
  location: "library",
  path,
  local: "modified",
});

function render(props: Partial<Parameters<typeof HeldUpdateDialog>[0]> = {}) {
  const onConfirm = props.onConfirm ?? vi.fn(async () => {});
  const onClose = props.onClose ?? vi.fn();
  act(() =>
    root.render(
      <HeldUpdateDialog
        open
        skillName="demo"
        removals={[]}
        {...props}
        onConfirm={onConfirm}
        onClose={onClose}
      />
    )
  );
  return { onConfirm, onClose };
}

const groups = () =>
  Array.from(container.querySelectorAll('[role="group"]')).map((g) => ({
    label: g.getAttribute("aria-label") ?? "",
    paths: Array.from(g.querySelectorAll("span")).map((s) => s.textContent),
  }));

const button = (label: string) =>
  Array.from(container.querySelectorAll("button")).find((b) => b.textContent === label)!;

describe("HeldUpdateDialog", () => {
  it("shows a removals-only hold exactly as before", () => {
    render({ removals: [removal("old.md"), removal("x.md", "claude_code")] });
    const text = container.textContent ?? "";
    expect(text).toContain("mySkills.updateActions.removalTitle");
    expect(text).toContain('mySkills.updateActions.removalMessage {"name":"demo","count":2}');
    expect(text).toContain("old.md");
    expect(text).toContain("claude_code: x.md");
    expect(text).not.toContain("heldUpdate");
    expect(button("mySkills.updateActions.removalConfirm")).toBeTruthy();
  });

  it("lists overwrites in their own group, worded as replacing the user's edits", () => {
    render({
      removals: [removal("old.md")],
      overwrites: [overwrite("SKILL.md"), overwrite("notes.md")],
      reason: "local_changes",
    });
    const text = container.textContent ?? "";
    expect(text).toContain("heldUpdate.title");
    expect(text).toContain('heldUpdate.message {"name":"demo","count":2}');
    // Local changes need no extra explanation.
    expect(text).not.toContain("heldUpdate.reason");
    expect(groups()).toEqual([
      { label: 'heldUpdate.overwritesGroup {"count":2}', paths: ["SKILL.md", "notes.md"] },
      { label: 'heldUpdate.removalsGroup {"count":1}', paths: ["old.md"] },
    ]);
    expect(button("heldUpdate.confirm")).toBeTruthy();
  });

  it.each(["baseline_not_backfilled", "baseline_unrecoverable", "baseline_suspect"] as const)(
    "explains why every changed file is held when the reason is %s",
    (reason) => {
      render({ overwrites: [overwrite("SKILL.md")], reason });
      expect(container.textContent).toContain(`heldUpdate.reason.${reason}`);
      expect(groups()).toHaveLength(1);
    }
  );

  it("confirming hands back to the caller, which re-runs the update with the token", async () => {
    const { onConfirm, onClose } = render({ overwrites: [overwrite("SKILL.md")] });
    await act(async () => {
      button("heldUpdate.confirm").click();
    });
    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalled();
  });

  it("cancelling does not confirm", () => {
    const { onConfirm, onClose } = render({ overwrites: [overwrite("SKILL.md")] });
    act(() => button("common.cancel").click());
    expect(onClose).toHaveBeenCalled();
    expect(onConfirm).not.toHaveBeenCalled();
  });
});
