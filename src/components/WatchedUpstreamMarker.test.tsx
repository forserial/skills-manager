// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WatchedUpstreamMarker } from "./WatchedUpstreamMarker";
import { useWatchedUpstreamMarkers } from "../hooks/useWatchedUpstreamMarkers";
import type { ManagedSkill } from "../lib/tauri";
import {
  notifyWatchedUpstreamsChanged,
  type WatchedMarker,
} from "../lib/watchedUpstreamsApi";

const api = vi.hoisted(() => ({ getWatchedUpstreamMarkers: vi.fn() }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, options?: { count?: number }) =>
      options?.count === undefined ? key : `${key}:${options.count}`,
  }),
}));
vi.mock("../lib/watchedUpstreamsApi", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/watchedUpstreamsApi")>()),
  getWatchedUpstreamMarkers: api.getWatchedUpstreamMarkers,
}));

const marker = (overrides: Partial<WatchedMarker> = {}): WatchedMarker => ({
  skill_id: "mine",
  watch_count: 2,
  moved_count: 1,
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

const markerEl = () => container.querySelector('[data-testid="watched-upstream-marker"]');

describe("WatchedUpstreamMarker", () => {
  it("shows when a watched location moved, with the count in its title", async () => {
    await act(async () => root.render(<WatchedUpstreamMarker marker={marker({ moved_count: 2 })} />));
    expect(markerEl()?.textContent).toBe("borrowed.marker");
    expect(markerEl()?.getAttribute("title")).toBe("borrowed.markerTitle:2");
  });

  it("renders nothing for no watches or nothing moved", async () => {
    await act(async () => root.render(<WatchedUpstreamMarker marker={undefined} />));
    expect(container.innerHTML).toBe("");
    await act(async () => root.render(<WatchedUpstreamMarker marker={marker({ moved_count: 0 })} />));
    expect(container.innerHTML).toBe("");
  });
});

function Harness({ skills }: { skills: ManagedSkill[] }) {
  const markers = useWatchedUpstreamMarkers(skills);
  return <WatchedUpstreamMarker marker={markers.mine} />;
}

describe("useWatchedUpstreamMarkers", () => {
  it("loads per skill id and reloads when the detail panel changes a watch", async () => {
    api.getWatchedUpstreamMarkers.mockResolvedValue([marker({ moved_count: 0 })]);
    await act(async () => root.render(<Harness skills={[]} />));
    expect(markerEl()).toBeNull();

    api.getWatchedUpstreamMarkers.mockResolvedValue([marker()]);
    await act(async () => notifyWatchedUpstreamsChanged());
    expect(markerEl()).not.toBeNull();
    expect(api.getWatchedUpstreamMarkers).toHaveBeenCalledTimes(2);
  });

  it("reloads when the skill list is refreshed, e.g. after Check all", async () => {
    api.getWatchedUpstreamMarkers.mockResolvedValue([]);
    await act(async () => root.render(<Harness skills={[]} />));
    expect(markerEl()).toBeNull();

    api.getWatchedUpstreamMarkers.mockResolvedValue([marker()]);
    await act(async () => root.render(<Harness skills={[]} />));
    expect(markerEl()).not.toBeNull();
  });
});
