import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import PdfOutline from "./PdfOutline";
import type { PdfOutlineNode } from "./usePdfOutline";

const outline: PdfOutlineNode[] = [
  {
    title: "Chapter 1",
    dest: "ch1",
    url: null,
    items: [{ title: "Section 1.1", dest: "sec11", url: null, items: [] }],
  },
  { title: "External", dest: null, url: "https://example.com", items: [] },
  { title: "Unlinked heading", dest: null, url: null, items: [] },
];

describe("PdfOutline", () => {
  it("renders nested entries and routes clicks by kind", () => {
    const onNavigate = vi.fn();
    const onOpen = vi.fn();
    render(
      <PdfOutline
        outline={outline}
        onNavigateToDestination={onNavigate}
        onOpenExternal={onOpen}
        onClose={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByText("Section 1.1"));
    expect(onNavigate).toHaveBeenCalledWith("sec11");

    fireEvent.click(screen.getByText("External"));
    expect(onOpen).toHaveBeenCalledWith("https://example.com");
  });

  it("disables entries with no destination", () => {
    render(
      <PdfOutline
        outline={outline}
        onNavigateToDestination={vi.fn()}
        onOpenExternal={vi.fn()}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByText("Unlinked heading")).toBeDisabled();
  });

  it("invokes onClose from the close button", () => {
    const onClose = vi.fn();
    render(
      <PdfOutline
        outline={outline}
        onNavigateToDestination={vi.fn()}
        onOpenExternal={vi.fn()}
        onClose={onClose}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Close table of contents" }));
    expect(onClose).toHaveBeenCalled();
  });
});
