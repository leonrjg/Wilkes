import { ask } from "@tauri-apps/plugin-dialog";

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export async function confirmDialog(message: string): Promise<boolean> {
  if (isTauri) return ask(message, { kind: "warning" });
  return window.confirm(message);
}
