import { beforeEach, describe, expect, it } from "vitest";
import { useBrowserStore } from "./useBrowserStore";

describe("useBrowserStore", () => {
  beforeEach(() => {
    useBrowserStore.setState({
      paneOpen: false,
      request: null,
      url: null,
      title: null,
      loading: false,
      held: false,
    });
  });

  it("asks again for an address followed twice", () => {
    const { show } = useBrowserStore.getState();
    show("https://example.org/");
    const first = useBrowserStore.getState().request;
    show("https://example.org/");
    expect(useBrowserStore.getState().request).not.toBe(first);
  });

  it("reopens where the page was when the pane closed", () => {
    const store = useBrowserStore.getState();
    store.show("https://example.org/");
    store.applyPageEvent({ kind: "loaded", url: "https://example.org/next" });
    store.closePane();
    store.togglePane();
    expect(useBrowserStore.getState()).toMatchObject({
      paneOpen: true,
      request: { url: "https://example.org/next" },
    });
  });

  it("opens empty when nothing has been shown", () => {
    useBrowserStore.getState().togglePane();
    expect(useBrowserStore.getState()).toMatchObject({ paneOpen: true, request: null });
  });

  it("follows the page's loading, address and title", () => {
    const { applyPageEvent } = useBrowserStore.getState();
    applyPageEvent({ kind: "loading", url: "https://example.org/a" });
    expect(useBrowserStore.getState()).toMatchObject({ url: "https://example.org/a", loading: true });
    applyPageEvent({ kind: "title", title: "A" });
    applyPageEvent({ kind: "loaded", url: "https://example.org/b" });
    expect(useBrowserStore.getState()).toMatchObject({
      url: "https://example.org/b",
      title: "A",
      loading: false,
    });
  });
});

describe("useBrowserStore downloads", () => {
  beforeEach(() => {
    useBrowserStore.setState({ downloads: [] });
  });

  it("follows a download from saving to saved, newest first", () => {
    const { applyPageEvent } = useBrowserStore.getState();
    applyPageEvent({ kind: "download_started", url: "https://x.org/a.epub", name: "a.epub", path: "/lib/a.epub" });
    applyPageEvent({ kind: "download_started", url: "https://x.org/b.zip", name: "b.zip", path: "/lib/b.zip" });
    applyPageEvent({ kind: "download_finished", url: "https://x.org/a.epub", name: "a.epub", path: "/lib/a.epub", success: true });
    expect(useBrowserStore.getState().downloads).toEqual([
      { url: "https://x.org/a.epub", name: "a.epub", path: "/lib/a.epub", state: "saved" },
      { url: "https://x.org/b.zip", name: "b.zip", path: "/lib/b.zip", state: "saving" },
    ]);
  });

  it("keeps a refused download's reason, named after its address", () => {
    useBrowserStore.getState().applyPageEvent({
      kind: "download_refused",
      url: "https://x.org/files/report.pdf?token=1",
      reason: "No library folder is open to save it in",
    });
    expect(useBrowserStore.getState().downloads[0]).toMatchObject({
      name: "report.pdf",
      state: "refused",
      reason: "No library folder is open to save it in",
    });
  });

  it("clears", () => {
    useBrowserStore.getState().applyPageEvent({ kind: "download_started", url: "u", name: "n", path: "/p" });
    useBrowserStore.getState().clearDownloads();
    expect(useBrowserStore.getState().downloads).toEqual([]);
  });
});

describe("useBrowserStore find", () => {
  beforeEach(() => {
    useBrowserStore.setState({ paneOpen: true, finding: false, findRequests: 0 });
  });

  it("opens the find strip for Cmd/Ctrl+F, counting each ask", () => {
    const { applyPageEvent } = useBrowserStore.getState();
    applyPageEvent({ kind: "find_requested" });
    applyPageEvent({ kind: "find_requested" });
    expect(useBrowserStore.getState()).toMatchObject({ finding: true, findRequests: 2 });
  });

  it("closes the strip with the pane", () => {
    useBrowserStore.getState().applyPageEvent({ kind: "find_requested" });
    useBrowserStore.getState().closePane();
    expect(useBrowserStore.getState().finding).toBe(false);
  });
});
