// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MergeConflictDialog } from "./MergeConflictDialog";
import type { MergeReport } from "../lib/tauri";

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

const report = (overrides: Partial<MergeReport> = {}): MergeReport => ({
  target_revision: "b".repeat(40),
  baseline_revision: "a".repeat(40),
  merged: ["tips.md", "guide.md"],
  kept_local: ["mine.md"],
  resolved: [],
  conflicts: [
    { path: "SKILL.md", kind: "text", local_sha256: "1", upstream_sha256: "2" },
    { path: "logo.png", kind: "binary", local_sha256: "3", upstream_sha256: "4" },
  ],
  base_source: "base_store",
  base_message: null,
  ...overrides,
});

function render(props: Partial<Parameters<typeof MergeConflictDialog>[0]> = {}) {
  const onApply = props.onApply ?? vi.fn(async () => {});
  const onClose = props.onClose ?? vi.fn();
  const onOpenVersions = props.onOpenVersions ?? vi.fn();
  act(() =>
    root.render(
      <MergeConflictDialog
        open
        skillName="demo"
        report={report()}
        {...props}
        onApply={onApply}
        onClose={onClose}
        onOpenVersions={onOpenVersions}
      />
    )
  );
  return { onApply, onClose, onOpenVersions };
}

const buttonIn = (path: string, label: string) => {
  const row = container.querySelector(`li[data-path="${path}"]`);
  const button = Array.from(row?.querySelectorAll("button") ?? []).find((b) =>
    b.textContent?.includes(label)
  );
  if (!button) throw new Error(`no ${label} button for ${path}`);
  return button;
};

const applyButton = () =>
  Array.from(container.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("mergeUpdate.apply")
  ) as HTMLButtonElement;

describe("MergeConflictDialog", () => {
  it("says how many files merged automatically and how many need a decision", () => {
    render();
    const summary = container.textContent ?? "";
    expect(summary).toContain('"merged":2');
    expect(summary).toContain('"count":2');
    expect(summary).toContain("mergeUpdate.kind.text");
    expect(summary).toContain("mergeUpdate.kind.binary");
    expect(container.querySelectorAll("li[data-path]")).toHaveLength(2);
  });

  it("applies only once every conflict has a choice, and passes the choices", async () => {
    const { onApply } = render();
    expect(applyButton().disabled).toBe(true);
    act(() => buttonIn("SKILL.md", "mergeUpdate.keepMine").click());
    expect(applyButton().disabled).toBe(true);
    act(() => buttonIn("logo.png", "mergeUpdate.takeTheirs").click());
    expect(buttonIn("logo.png", "mergeUpdate.takeTheirs").getAttribute("aria-pressed")).toBe("true");
    expect(applyButton().disabled).toBe(false);
    await act(async () => applyButton().click());
    expect(onApply).toHaveBeenCalledWith({ "SKILL.md": "local", "logo.png": "upstream" });
  });

  it("changing a choice replaces it", async () => {
    const { onApply } = render({
      report: report({ conflicts: [report().conflicts[0]] }),
    });
    act(() => buttonIn("SKILL.md", "mergeUpdate.keepMine").click());
    act(() => buttonIn("SKILL.md", "mergeUpdate.takeTheirs").click());
    expect(buttonIn("SKILL.md", "mergeUpdate.keepMine").getAttribute("aria-pressed")).toBe("false");
    await act(async () => applyButton().click());
    expect(onApply).toHaveBeenCalledWith({ "SKILL.md": "upstream" });
  });

  it("opens both versions of a file without choosing", () => {
    const { onOpenVersions, onApply } = render();
    act(() => buttonIn("SKILL.md", "mergeUpdate.openBoth").click());
    expect(onOpenVersions).toHaveBeenCalledWith("SKILL.md");
    expect(onApply).not.toHaveBeenCalled();
    expect(applyButton().disabled).toBe(true);
  });

  it("cancel closes without applying", () => {
    const { onClose, onApply } = render();
    const cancel = Array.from(container.querySelectorAll("button")).find(
      (b) => b.textContent === "common.cancel"
    );
    act(() => cancel?.click());
    expect(onClose).toHaveBeenCalled();
    expect(onApply).not.toHaveBeenCalled();
  });

  it("explains a missing base", () => {
    render({
      report: report({
        conflicts: [{ path: "SKILL.md", kind: "base_gone", local_sha256: "1", upstream_sha256: "2" }],
        base_message: "revision is gone upstream",
      }),
    });
    expect(container.textContent).toContain("mergeUpdate.baseMissing");
    expect(container.textContent).toContain("mergeUpdate.kind.base_gone");
  });

  it("renders nothing when closed", () => {
    render({ open: false });
    expect(container.innerHTML).toBe("");
  });
});
