import { describe, expect, it } from "vitest";
import { readMarkdownViewMode, saveMarkdownViewMode } from "./markdownViewMode";

describe("markdownViewMode", () => {
  it("defaults Markdown documents to rendered while remembering their selected mode", () => {
    expect(readMarkdownViewMode("/new.md")).toBe("rendered");

    saveMarkdownViewMode("/notes.md", "source");

    expect(readMarkdownViewMode("/notes.md")).toBe("source");
    expect(readMarkdownViewMode("/other.md")).toBe("rendered");
  });
});
