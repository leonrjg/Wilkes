import { act, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../services", () => ({
  isTauri: true,
  api: {
    browserPaneShow: vi.fn(() => Promise.resolve()),
    browserPaneSetBounds: vi.fn(() => Promise.resolve()),
    browserPaneSetVisible: vi.fn(() => Promise.resolve()),
    browserPaneSetDarkTheme: vi.fn(() => Promise.resolve()),
    browserPaneStep: vi.fn(() => Promise.resolve()),
    browserPaneClearData: vi.fn(() => Promise.resolve()),
    browserPaneSavePage: vi.fn(() => Promise.resolve()),
    browserPaneClose: vi.fn(() => Promise.resolve()),
    onBrowserPanePage: vi.fn(() => Promise.resolve(() => {})),
    openPath: vi.fn(() => Promise.resolve()),
  },
}));
vi.mock("../lib/utils/dialog", () => ({ confirmDialog: vi.fn(() => Promise.resolve(true)) }));

import { api } from "../services";
import { useBrowserStore } from "../stores/useBrowserStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import BrowserPane, { addressFrom } from "./BrowserPane";

describe("BrowserPane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useBrowserStore.setState({
      paneOpen: true,
      request: null,
      url: null,
      title: null,
      loading: false,
      held: false,
      darkenLightPages: true,
    });
    useSettingsStore.setState({ colorScheme: "light" });
    document.documentElement.style.setProperty("--bg-app", "#0a0a0a");
    document.documentElement.style.setProperty("--text-main", "#ededed");
  });

  it("darkens light pages in dark mode in the app's own colours, until switched off", async () => {
    useSettingsStore.setState({ colorScheme: "dark" });
    await act(async () => {
      render(<BrowserPane />);
    });
    expect(api.browserPaneSetDarkTheme).toHaveBeenLastCalledWith({ background: "#0a0a0a", text: "#ededed" });
    await act(async () => {
      fireEvent.click(screen.getByLabelText("Darken light pages"));
    });
    expect(api.browserPaneSetDarkTheme).toHaveBeenLastCalledWith(null);
  });

  it("does not darken in light mode", async () => {
    await act(async () => {
      render(<BrowserPane />);
    });
    expect(api.browserPaneSetDarkTheme).toHaveBeenLastCalledWith(null);
    expect(screen.queryByLabelText("Darken light pages")).toBeNull();
  });

  it("opens the requested address over its stage", async () => {
    useBrowserStore.setState({ request: { url: "https://example.org/" } });
    await act(async () => {
      render(<BrowserPane />);
    });
    expect(api.browserPaneShow).toHaveBeenCalledWith(
      "https://example.org/",
      expect.objectContaining({ x: expect.any(Number), width: expect.any(Number) }),
    );
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
    expect(api.browserPaneShow).toHaveBeenCalledWith("https://arxiv.org", expect.anything());
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
