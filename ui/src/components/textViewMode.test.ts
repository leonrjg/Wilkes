import { describe, expect, it } from "vitest";
import { readTextViewMode, saveTextViewMode } from "./textViewMode";

describe("textViewMode", () => {
  it("defaults a document to rendered while remembering its selected mode", () => {
    expect(readTextViewMode("/new.md")).toBe("rendered");

    saveTextViewMode("/notes.md", "source");

    expect(readTextViewMode("/notes.md")).toBe("source");
    expect(readTextViewMode("/other.md")).toBe("rendered");
  });

  it("remembers Markdown and HTML documents in the same place", () => {
    saveTextViewMode("/page.html", "source");

    expect(readTextViewMode("/page.html")).toBe("source");
    expect(readTextViewMode("/page.md")).toBe("rendered");
  });
});
