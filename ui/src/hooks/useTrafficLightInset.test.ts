import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const windowMock = vi.hoisted(() => ({
  fullscreen: false,
  resized: null as (() => void) | null,
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    isFullscreen: () => Promise.resolve(windowMock.fullscreen),
    onResized: (handler: () => void) => {
      windowMock.resized = handler;
      return Promise.resolve(() => {
        windowMock.resized = null;
      });
    },
  }),
}));

/** The hook as a desktop shell on `platform` would load it: whether the title
 *  bar is overlaid is decided once, when the module loads. */
async function load(platform: string, tauri: boolean) {
  vi.resetModules();
  vi.spyOn(navigator, "platform", "get").mockReturnValue(platform);
  vi.doMock("../services", () => ({ isTauri: tauri }));
  return (await import("./useTrafficLightInset")).useTrafficLightInset;
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.doUnmock("../services");
  windowMock.fullscreen = false;
  windowMock.resized = null;
});

describe("useTrafficLightInset", () => {
  it("leaves room for the traffic lights in a macOS desktop window", async () => {
    const useTrafficLightInset = await load("MacIntel", true);
    const { result } = renderHook(() => useTrafficLightInset());
    expect(result.current).toBe(true);
  });

  it("gives the room back in full screen, and takes it again on leaving", async () => {
    const useTrafficLightInset = await load("MacIntel", true);
    const { result } = renderHook(() => useTrafficLightInset());
    await waitFor(() => expect(windowMock.resized).not.toBeNull());

    windowMock.fullscreen = true;
    await act(async () => windowMock.resized?.());
    expect(result.current).toBe(false);

    windowMock.fullscreen = false;
    await act(async () => windowMock.resized?.());
    expect(result.current).toBe(true);
  });

  it("leaves no room where the title bar is the system's own", async () => {
    const onWindows = await load("Win32", true);
    expect(renderHook(() => onWindows()).result.current).toBe(false);
    const inBrowser = await load("MacIntel", false);
    expect(renderHook(() => inBrowser()).result.current).toBe(false);
  });
});
