import { act, fireEvent, render, screen } from "@testing-library/react";
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../services", () => ({
  isTauri: true,
  api: {
    browserPaneShow: vi.fn(() => Promise.resolve()),
    browserPaneSetBounds: vi.fn(() => Promise.resolve()),
    browserPaneSetVisible: vi.fn(() => Promise.resolve()),
    browserPaneSetInversion: vi.fn(() => Promise.resolve()),
    browserPaneStep: vi.fn(() => Promise.resolve()),
    browserPaneClearData: vi.fn(() => Promise.resolve()),
    browserPaneSavePage: vi.fn(() => Promise.resolve()),
    browserPaneFind: vi.fn(() => Promise.resolve({ found: true, matches: null })),
    browserPaneEndFind: vi.fn(() => Promise.resolve()),
    browserPaneClose: vi.fn(() => Promise.resolve()),
    onBrowserPanePage: vi.fn(() => Promise.resolve(() => {})),
    openPath: vi.fn(() => Promise.resolve()),
  },
}));
vi.mock("../lib/utils/dialog", () => ({ confirmDialog: vi.fn(() => Promise.resolve(true)) }));

import { api } from "../services";
import { useBrowserStore } from "../stores/useBrowserStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import type { Settings } from "../lib/types";
import BrowserPane, { addressFrom } from "./BrowserPane";

describe("BrowserPane", () => {
  beforeAll(() => {
    // The colour the app's own stylesheet gives the pane.
    const style = document.createElement("style");
    style.textContent = `[class~="bg-[var(--bg-sidebar)]"] { background-color: rgb(30, 31, 34); }`;
    document.head.appendChild(style);
  });

  beforeEach(() => {
    vi.clearAllMocks();
    useBrowserStore.setState({
      paneOpen: true,
      request: null,
      url: null,
      title: null,
      loading: false,
      held: false,
      invertLightPages: true,
      finding: false,
      findRequests: 0,
    });
    useSettingsStore.setState({ colorScheme: "light", settings: { dark_mode_inversion: 80 } as Settings });
  });

  it("inverts light pages in dark mode at the reader's strength, until switched off", async () => {
    useSettingsStore.setState({ colorScheme: "dark" });
    await act(async () => {
      render(<BrowserPane />);
    });
    expect(api.browserPaneSetInversion).toHaveBeenLastCalledWith(0.8);
    await act(async () => {
      fireEvent.click(screen.getByLabelText("Invert light pages"));
    });
    expect(api.browserPaneSetInversion).toHaveBeenLastCalledWith(null);
  });

  it("does not invert in light mode", async () => {
    await act(async () => {
      render(<BrowserPane />);
    });
    expect(api.browserPaneSetInversion).toHaveBeenLastCalledWith(null);
    expect(screen.queryByLabelText("Invert light pages")).toBeNull();
  });

  it("opens the requested address over its stage, in the colour behind it", async () => {
    useBrowserStore.setState({ request: { url: "https://example.org/" } });
    await act(async () => {
      render(<BrowserPane />);
    });
    expect(api.browserPaneShow).toHaveBeenCalledWith(
      "https://example.org/",
      expect.objectContaining({ x: expect.any(Number), width: expect.any(Number) }),
      [30, 31, 34],
    );
  });

  it("opens the find strip for Cmd/Ctrl+F in the pane, and ends the find on Escape", async () => {
    useBrowserStore.setState({ request: { url: "https://example.org/" }, url: "https://example.org/" });
    await act(async () => {
      render(<BrowserPane />);
    });
    expect(screen.queryByLabelText("Find in page")).toBeNull();
    await act(async () => {
      useBrowserStore.getState().applyPageEvent({ kind: "find_requested" });
    });
    const input = screen.getByLabelText("Find in page");
    expect(input).toHaveFocus();
    await act(async () => {
      fireEvent.keyDown(input, { key: "Escape" });
    });
    expect(screen.queryByLabelText("Find in page")).toBeNull();
    expect(api.browserPaneEndFind).toHaveBeenCalled();
  });

  it("navigates to what is typed in the address bar", async () => {
    await act(async () => {
      render(<BrowserPane />);
    });
    const address = screen.getByLabelText("Address");
    fireEvent.change(address, { target: { value: "arxiv.org" } });
    await act(async () => {
      fireEvent.submit(address.closest("form")!);
    });
    expect(api.browserPaneShow).toHaveBeenCalledWith("https://arxiv.org", expect.anything(), expect.anything());
  });

  it("steps aside while the page holds the pointer, once the webview exists", async () => {
    useBrowserStore.setState({ request: { url: "https://example.org/" } });
    await act(async () => {
      render(<BrowserPane />);
    });
    await act(async () => {
      useBrowserStore.getState().setHeld(true);
    });
    expect(api.browserPaneSetVisible).toHaveBeenLastCalledWith(false);
    await act(async () => {
      useBrowserStore.getState().setHeld(false);
    });
    expect(api.browserPaneSetVisible).toHaveBeenLastCalledWith(true);
  });

  it("closes its webview when the pane closes", async () => {
    let view: ReturnType<typeof render>;
    await act(async () => {
      view = render(<BrowserPane />);
    });
    view!.unmount();
    expect(api.browserPaneClose).toHaveBeenCalled();
  });

  it("saves what it is showing to the library", async () => {
    useBrowserStore.setState({ request: { url: "https://arxiv.org/pdf/1706.03762" }, url: "https://arxiv.org/pdf/1706.03762" });
    await act(async () => {
      render(<BrowserPane />);
    });
    fireEvent.click(screen.getByLabelText("Save to library"));
    expect(api.browserPaneSavePage).toHaveBeenCalled();
  });

  it("reads a bare host as its https site", () => {
    expect(addressFrom(" example.org/a ")).toBe("https://example.org/a");
    expect(addressFrom("http://example.org")).toBe("http://example.org");
  });
});
