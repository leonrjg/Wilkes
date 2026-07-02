import { describe, expect, it } from "vitest";
import { toMarkdown } from "./bookmarkMarkdown";
import type { Bookmark } from "../types";

describe("toMarkdown", () => {
  it("formats PDF bookmarks with quote, file link, and page", () => {
    const bookmark: Bookmark = {
      id: "bookmark-1",
      path: "/tmp/example.pdf",
      origin: { PdfPage: { page: 7, bbox: null } },
      quote: "quoted passage",
      created_at: "2026-01-01T00:00:00Z",
      note: null,
    };

    expect(toMarkdown(bookmark)).toBe("> quoted passage\n\n- [example.pdf](/tmp/example.pdf), p.7");
  });

  it("includes the note between the quote and the citation when present", () => {
    const bookmark: Bookmark = {
      id: "bookmark-2",
      path: "/tmp/example.pdf",
      origin: { PdfPage: { page: 7, bbox: null } },
      quote: "quoted passage",
      created_at: "2026-01-01T00:00:00Z",
      note: "my thoughts",
    };

    expect(toMarkdown(bookmark)).toBe(
      "> quoted passage\n\nmy thoughts\n\n- [example.pdf](/tmp/example.pdf), p.7",
    );
  });
});
