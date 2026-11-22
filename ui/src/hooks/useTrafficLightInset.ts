import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isTauri } from "../services";

/** The macOS window has no title bar of its own: its traffic lights sit over
 *  the page, in the top row (see `trafficLightPosition` in tauri.conf.json). */
const OVERLAID_TITLE_BAR = isTauri && navigator.platform.startsWith("Mac");

/**
 * Whether the window's traffic lights are drawn over the page, so the top row
 * has to leave room for them. They are not in full screen, where macOS takes
 * them away with the menu bar.
 */
export function useTrafficLightInset(): boolean {
  const [fullscreen, setFullscreen] = useState(false);

  useEffect(() => {
    if (!OVERLAID_TITLE_BAR) return;
    const window = getCurrentWindow();
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const check = () =>
      window
        .isFullscreen()
        .then((value) => {
          if (!disposed) setFullscreen(value);
        })
        .catch((error) => console.error("Could not tell whether the window is full screen:", error));
    check();
    // Entering and leaving full screen both resize the window.
    window
      .onResized(check)
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch((error) => console.error("Could not follow the window's size:", error));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  return OVERLAID_TITLE_BAR && !fullscreen;
}
