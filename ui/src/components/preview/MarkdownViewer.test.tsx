import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import MarkdownViewer from "./MarkdownViewer";

describe("MarkdownViewer", () => {
  it("renders headings and GFM tables", () => {
    render(
      <MarkdownViewer
        documentPath="/notes.md"
        highlightRange={{ start: 0, end: 0 }}
        content={"## Summary table\n\n| Metric | Recommendation |\n| --- | --- |\n| Complexity | Keep |"}
      />,
    );

    expect(screen.getByRole("heading", { name: "Summary table", level: 2 })).toBeInTheDocument();
    expect(screen.getByRole("table")).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Metric" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "Keep" })).toBeInTheDocument();
  });

  it("opens Markdown links outside the app", () => {
    render(<MarkdownViewer documentPath="/notes.md" content="[Wilkes](https://example.com)" highlightRange={{ start: 0, end: 0 }} />);

    expect(screen.getByRole("link", { name: "Wilkes" })).toHaveAttribute("target", "_blank");
  });

  it("opens the larger find bar at the top of the viewer", () => {
    render(<MarkdownViewer documentPath="/notes.md" content="Searchable text" highlightRange={{ start: 0, end: 0 }} />);

    fireEvent.keyDown(window, { key: "f", metaKey: true });

    const input = screen.getByPlaceholderText("Find in document...");
    expect(input).toHaveClass("w-56", "text-sm");
    expect(input.closest(".absolute")).toHaveClass("top-4");
    expect(input.closest(".absolute")).not.toHaveClass("bottom-4");
  });

  it("segments overlapping search and bookmark annotations from source byte ranges", () => {
    const content = "Start **café🙂** end";
    const encoder = new TextEncoder();
    const start = encoder.encode("Start **").length;
    const cafeEnd = start + encoder.encode("café").length;
    const wordEnd = cafeEnd + encoder.encode("🙂").length;
    render(
      <MarkdownViewer
        documentPath="/notes.md"
        content={content}
        restoreScrollPosition={false}
        highlightRange={{ start, end: wordEnd }}
        bookmarkHighlights={[{ id: "cafe", range: { start, end: cafeEnd } }]}
      />,
    );

    const overlap = document.querySelector<HTMLElement>(".markdown-search-highlight.markdown-bookmark-highlight");
    expect(overlap).toHaveTextContent("café");
    expect(overlap).toHaveAttribute("data-bookmark-ids", "cafe");
    expect(document.querySelector<HTMLElement>(".markdown-search-highlight:not(.markdown-bookmark-highlight)"))
      .toHaveTextContent("🙂");
  });

  it("opens a bookmark when its rendered highlight is clicked", () => {
    const onBookmarkOpen = vi.fn();
    render(
      <MarkdownViewer
        documentPath="/notes.md"
        content="Read this note"
        highlightRange={{ start: 0, end: 0 }}
        bookmarkHighlights={[{ id: "note-1", range: { start: 5, end: 9 } }]}
        onBookmarkOpen={onBookmarkOpen}
      />,
    );

    fireEvent.click(document.querySelector(".markdown-bookmark-highlight")!);
    expect(onBookmarkOpen).toHaveBeenCalledWith("note-1", {
      left: 0,
      top: 0,
      right: 0,
      bottom: 0,
    });
  });

  it("zooms the rendered article font size via the controls", () => {
    render(<MarkdownViewer documentPath="/zoom.md" content="Readable body text" highlightRange={{ start: 0, end: 0 }} />);
    const article = document.querySelector<HTMLElement>(".prose-document")!;
    const controls = screen.getByRole("button", { name: "Zoom in" }).parentElement;
    expect(controls).toHaveClass("px-2.5", "py-1.5", "text-sm");
    expect(article.style.fontSize).toBe("1rem");

    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    expect(article.style.fontSize).toBe("1.1rem");

    fireEvent.click(screen.getByRole("button", { name: "Zoom out" }));
    fireEvent.click(screen.getByRole("button", { name: "Zoom out" }));
    expect(article.style.fontSize).toBe("0.9rem");
  });

  it("restores a document's remembered zoom on reopen", () => {
    const { rerender } = render(
      <MarkdownViewer documentPath="/remember.md" content="body" highlightRange={{ start: 0, end: 0 }} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    expect(document.querySelector<HTMLElement>(".prose-document")!.style.fontSize).toBe("1.1rem");

    // Switch to another document, then back: the remembered zoom returns.
    rerender(<MarkdownViewer documentPath="/other.md" content="body" highlightRange={{ start: 0, end: 0 }} />);
    expect(document.querySelector<HTMLElement>(".prose-document")!.style.fontSize).toBe("1rem");

    rerender(<MarkdownViewer documentPath="/remember.md" content="body" highlightRange={{ start: 0, end: 0 }} />);
    expect(document.querySelector<HTMLElement>(".prose-document")!.style.fontSize).toBe("1.1rem");
  });

  it("maps a rendered selection back to the existing text bookmark shape", () => {
    const onAddBookmark = vi.fn();
    render(
      <MarkdownViewer
        documentPath="/notes.md"
        content={"# Title\n\nPick **this** text"}
        highlightRange={{ start: 0, end: 0 }}
        onAddBookmark={onAddBookmark}
      />,
    );
    const run = Array.from(document.querySelectorAll<HTMLElement>(".markdown-source-run"))
      .find((element) => element.textContent === "this")!;
    const text = run.firstChild!;
    const rect = { top: 10, left: 10, right: 50, bottom: 30, width: 40, height: 20, x: 10, y: 10, toJSON: () => ({}) } as DOMRect;
    const range = {
      startContainer: text,
      endContainer: text,
      startOffset: 0,
      endOffset: 4,
      getBoundingClientRect: () => rect,
      getClientRects: () => [rect] as unknown as DOMRectList,
    } as unknown as Range;
    vi.spyOn(window, "getSelection").mockReturnValue({
      isCollapsed: false,
      rangeCount: 1,
      getRangeAt: () => range,
      toString: () => "this",
      removeAllRanges: vi.fn(),
    } as unknown as Selection);

    fireEvent.mouseUp(run);
    fireEvent.click(screen.getByRole("button", { name: "Bookmark" }));

    expect(onAddBookmark).toHaveBeenCalledWith({
      quote: "this",
      origin: { TextFile: { line: 3, col: 7 } },
      text_range: { start: 16, end: 20 },
      rects: [],
    });
  });
});
