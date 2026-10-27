import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../services", () => ({
  isTauri: true,
  api: { openPath: vi.fn(() => Promise.resolve()) },
}));

import { api } from "../services";
import { useBrowserStore } from "../stores/useBrowserStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import type { Settings } from "./types";
import { openLink, routeLinkClicks } from "./openLink";

function linksInApp(enabled: boolean) {
  useSettingsStore.setState({ settings: { open_links_in_app: enabled } as Settings });
}

function clickLink(href: string, setup?: (anchor: HTMLAnchorElement) => void) {
  const anchor = document.createElement("a");
  anchor.setAttribute("href", href);
  anchor.textContent = "link";
  setup?.(anchor);
  document.body.appendChild(anchor);
  const event = new MouseEvent("click", { bubbles: true, cancelable: true, button: 0 });
  anchor.dispatchEvent(event);
  anchor.remove();
  return event;
}

describe("openLink", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useBrowserStore.setState({ paneOpen: false, request: null, url: null });
  });

  it("opens a web address in the browser pane when that is turned on", async () => {
    linksInApp(true);
    await openLink("https://example.org/paper");
    expect(useBrowserStore.getState()).toMatchObject({
      paneOpen: true,
      request: { url: "https://example.org/paper" },
    });
    expect(api.openPath).not.toHaveBeenCalled();
  });

  it("opens a web address in the system browser when it is turned off", async () => {
    linksInApp(false);
    await openLink("https://example.org/paper");
    expect(api.openPath).toHaveBeenCalledWith("https://example.org/paper");
    expect(useBrowserStore.getState().paneOpen).toBe(false);
  });

  it("leaves anything that is not a web address to the operating system", async () => {
    linksInApp(true);
    await openLink("/Users/me/notes/figure.png");
    expect(api.openPath).toHaveBeenCalledWith("/Users/me/notes/figure.png");
    expect(useBrowserStore.getState().paneOpen).toBe(false);
  });
});

describe("routeLinkClicks", () => {
  let stop: () => void;

  beforeEach(() => {
    vi.clearAllMocks();
    linksInApp(true);
    useBrowserStore.setState({ paneOpen: false, request: null, url: null });
    stop?.();
    stop = routeLinkClicks();
  });

  it("sends a click on a web link through openLink", () => {
    const event = clickLink("https://example.org/", (a) => a.setAttribute("target", "_blank"));
    expect(event.defaultPrevented).toBe(true);
    expect(useBrowserStore.getState().request).toEqual({ url: "https://example.org/" });
  });

  it("leaves a click a component already handled to that component", () => {
    clickLink("https://example.org/", (a) => a.addEventListener("click", (e) => e.preventDefault()));
    expect(useBrowserStore.getState().request).toBeNull();
  });

  it("ignores relative and in-page links", () => {
    for (const href of ["#section-2", "chapter-3.html", "/about"]) {
      const event = clickLink(href);
      expect(event.defaultPrevented).toBe(false);
    }
    expect(useBrowserStore.getState().request).toBeNull();
    expect(api.openPath).not.toHaveBeenCalled();
  });
});
