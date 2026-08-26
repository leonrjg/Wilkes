import { describe, expect, it } from "vitest";
import { readTextScrollPosition, saveTextScrollPosition } from "./textScrollMemory";

describe("textScrollMemory", () => {
  it("keeps source and rendered positions independent", () => {
    saveTextScrollPosition("/notes.md", "source", 0.25);
    saveTextScrollPosition("/notes.md", "rendered", 0.75);

    expect(readTextScrollPosition("/notes.md", "source")).toBe(0.25);
    expect(readTextScrollPosition("/notes.md", "rendered")).toBe(0.75);
  });

  it("clamps saved positions to the scrollable range", () => {
    saveTextScrollPosition("/clamped.md", "source", -1);
    saveTextScrollPosition("/clamped.md", "rendered", 2);

    expect(readTextScrollPosition("/clamped.md", "source")).toBe(0);
    expect(readTextScrollPosition("/clamped.md", "rendered")).toBe(1);
  });
});
