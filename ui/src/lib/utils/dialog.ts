import { ask } from "@tauri-apps/plugin-dialog";

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/**
 * The one confirmation in the application.
 *
 * `window.confirm` is not an option in the desktop shell: the dialog plugin
 * patches it and the capability that would allow it is not granted, so a call
 * fails with `plugin:dialog|confirm not allowed by ACL` and the caller reads
 * the rejection as a refusal — the delete that was confirmed never happens,
 * and nothing on screen says why. `ask` is what this installation permits.
 * The browser fallback is for the served build, where there is no plugin.
 */
export async function confirmDialog(message: string): Promise<boolean> {
  if (isTauri) return ask(message, { kind: "warning" });
  return window.confirm(message);
}
