// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SkillBorrowedFromSection } from "./SkillBorrowedFromSection";
import type { ManagedSkill } from "../lib/tauri";
import { upstreamWebUrl, type WatchedUpstream } from "../lib/watchedUpstreamsApi";

const api = vi.hoisted(() => ({
  getWatchedUpstreams: vi.fn(),
  checkWatchedUpstreams: vi.fn(),
  markWatchedUpstreamSeen: vi.fn(),
  openUrl: vi.fn(),
}));
const messages = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn() }));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("sonner", () => ({ toast: messages }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: api.openUrl }));
vi.mock("../lib/watchedUpstreamsApi", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/watchedUpstreamsApi")>()),
  getWatchedUpstreams: api.getWatchedUpstreams,
  checkWatchedUpstreams: api.checkWatchedUpstreams,
  markWatchedUpstreamSeen: api.markWatchedUpstreamSeen,
}));

const skill: ManagedSkill = {
  id: "mine", name: "mine", description: null, source_type: "local", source_ref: null,
  source_ref_resolved: null, source_subpath: null, source_branch: null,
  source_revision: null, remote_revision: null, update_status: "local_only",
  last_checked_at: null, last_check_error: null, central_path: "/fixture/mine",
  enabled: true, created_at: 0, updated_at: 0, status: "ok", targets: [],
  preset_ids: [], tags: [],
};

const watch = (overrides: Partial<WatchedUpstream> = {}): WatchedUpstream => ({
  id: "ab12cd34",
  git_url: "https://github.com/owner/repo.git",
  subpath: "skills/x",
  branch: "main",
  last_seen_revision: "1".repeat(40),
  note: "lifted the retry loop",
  added_at: 0,
  remote_revision: "2".repeat(40),
  moved: true,
  checked_at: 1,
  check_error: null,
  ...overrides,
});

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.clearAllMocks();
  api.openUrl.mockResolvedValue(undefined);
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
  await act(async () => root.render(<SkillBorrowedFromSection skill={skill} />));
};
const button = (label: string) =>
  [...container.querySelectorAll("button")].find((b) => b.textContent?.includes(label));

describe("SkillBorrowedFromSection", () => {
  it("renders nothing for a skill that watches nothing", async () => {
    api.getWatchedUpstreams.mockResolvedValue([]);
    await render();
    expect(container.innerHTML).toBe("");
  });

  it("lists watched upstreams with a moved marker, and mark as seen clears it", async () => {
    api.getWatchedUpstreams.mockResolvedValue([watch()]);
    api.markWatchedUpstreamSeen.mockResolvedValue(
      watch({ moved: false, last_seen_revision: "2".repeat(40) }),
    );
    await render();

    expect(container.textContent).toContain("borrowed.title");
    expect(container.textContent).toContain("https://github.com/owner/repo.git");
    expect(container.textContent).toContain("lifted the retry loop");
    expect(container.querySelector('[data-testid="borrowed-moved"]')).not.toBeNull();

    await act(async () => button("borrowed.open")!.click());
    expect(api.openUrl).toHaveBeenCalledWith(
      `https://github.com/owner/repo/compare/${"1".repeat(40)}...${"2".repeat(40)}`,
    );

    await act(async () => button("borrowed.markSeen")!.click());
    expect(api.markWatchedUpstreamSeen).toHaveBeenCalledWith("mine", "ab12cd34");
    expect(container.querySelector('[data-testid="borrowed-moved"]')).toBeNull();
    expect(button("borrowed.markSeen")).toBeUndefined();
    expect(container.textContent).toContain("borrowed.unchanged");
  });

  it("Check replaces the list with the checked one", async () => {
    api.getWatchedUpstreams.mockResolvedValue([watch({ moved: false, remote_revision: null })]);
    api.checkWatchedUpstreams.mockResolvedValue([watch()]);
    await render();
    expect(container.querySelector('[data-testid="borrowed-moved"]')).toBeNull();
    await act(async () => button("borrowed.check")!.click());
    expect(api.checkWatchedUpstreams).toHaveBeenCalledWith("mine");
    expect(container.querySelector('[data-testid="borrowed-moved"]')).not.toBeNull();
  });
});

describe("upstreamWebUrl", () => {
  it("links GitHub to the directory at the branch when nothing moved", () => {
    expect(upstreamWebUrl(watch({ moved: false }))).toBe(
      "https://github.com/owner/repo/tree/main/skills/x",
    );
    expect(upstreamWebUrl(watch({ moved: false, git_url: "git@github.com:owner/repo.git" }))).toBe(
      "https://github.com/owner/repo/tree/main/skills/x",
    );
  });

  it("uses other http(s) URLs as they are and has no page for local or ssh remotes", () => {
    expect(upstreamWebUrl(watch({ git_url: "https://gitlab.com/a/b.git" }))).toBe(
      "https://gitlab.com/a/b.git",
    );
    expect(upstreamWebUrl(watch({ git_url: "file:///C:/repos/x.git" }))).toBeNull();
    expect(upstreamWebUrl(watch({ git_url: "ssh://git@example.com/a/b.git" }))).toBeNull();
  });
});
