import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import MarkdownViewer from "./MarkdownViewer";

describe("MarkdownViewer", () => {
  it("renders headings and GFM tables", () => {
    render(
      <MarkdownViewer
        documentPath="/notes.md"
        content={"## Summary table\n\n| Metric | Recommendation |\n| --- | --- |\n| Complexity | Keep |"}
      />,
    );

    expect(screen.getByRole("heading", { name: "Summary table", level: 2 })).toBeInTheDocument();
    expect(screen.getByRole("table")).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Metric" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "Keep" })).toBeInTheDocument();
  });

  it("opens Markdown links outside the app", () => {
    render(<MarkdownViewer documentPath="/notes.md" content="[Wilkes](https://example.com)" />);

    expect(screen.getByRole("link", { name: "Wilkes" })).toHaveAttribute("target", "_blank");
  });
});
