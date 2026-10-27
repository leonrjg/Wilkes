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
