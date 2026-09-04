import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { CHAT_COMMANDS } from "@leonrjg/wilkes-chat";

// The one seam nothing else can check.
//
// `invoke("chat_send")` is a string in the package and a symbol in
// `generate_handler![…]` here. Neither compiler sees both, so a command the
// shell stopped registering fails at runtime, in the window, as "command not
// found" — and only for whoever opens the chat pane. Type-checking passes,
// `cargo check` passes, the unit tests pass.
//
// `CHAT_COMMANDS` is the package's own list of what it calls, which is why it
// is worth comparing against: it is not a second copy of these names, it is
// the list the transport is written from.

const here = dirname(fileURLToPath(import.meta.url));
const shell = readFileSync(resolve(here, "../../../crates/desktop/src/lib.rs"), "utf8");

/** Every command the shell registered, from `generate_handler![…]`. */
function registered(source: string): string[] {
  const start = source.indexOf("tauri::generate_handler![");
  if (start === -1) throw new Error("no generate_handler! in the shell");
  const end = source.indexOf("]", start);
  return source
    .slice(source.indexOf("[", start) + 1, end)
    .split(",")
    // The chat's commands live in a module of their own and are registered as
    // `chat::chat_send`; the name that crosses the IPC is the last segment.
    .map((name) => name.trim().split("::").pop() ?? "")
    .filter((name) => /^[a-z0-9_]+$/.test(name));
}

describe("the chat commands the shell registers", () => {
  const actual = registered(shell);

  it("were found at all", () => {
    // A parser that quietly stopped matching would make the check below
    // vacuously true, which is this kind of test's whole failure mode.
    expect(actual.length).toBeGreaterThan(20);
  });

  it("include every one the package calls", () => {
    expect(actual).toEqual(expect.arrayContaining([...CHAT_COMMANDS]));
  });

  it("no longer include the three that pushed context into a live session", () => {
    // What the chat is about is the window's, carried on the calls that need
    // it. A command that could push it separately would be a second owner.
    for (const gone of ["chat_add_context", "chat_remove_context", "chat_set_active_doc"]) {
      expect(actual).not.toContain(gone);
    }
  });
});
