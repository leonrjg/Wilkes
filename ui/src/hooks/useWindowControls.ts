import { useEffect, useState } from "react";
import { api } from "../services";
import type { WindowControls } from "../services/api";

/** Where the window's own controls -- macOS's traffic lights -- are drawn over
 *  the page, or null while they are not. */
export function useWindowControls(): WindowControls | null {
  const [controls, setControls] = useState<WindowControls | null>(null);

  useEffect(() => {
    if (!api.watchWindowControls) return;
    let disposed = false;
    let stop: (() => void) | undefined;
    api
      .watchWindowControls((next) => {
        if (!disposed) setControls(next);
      })
      .then((unwatch) => {
        if (disposed) unwatch();
        else stop = unwatch;
      })
      .catch((error) => console.error("Could not find the window's controls:", error));
    return () => {
      disposed = true;
      stop?.();
    };
  }, []);

  return controls;
}
