import { render, screen } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import CodeViewer from "./CodeViewer";

vi.mock("codemirror", () => ({ basicSetup: [] }));

describe("CodeViewer", () => {
  const defaultProps = {
    content: "test content",
    language: "typescript",
    highlightLine: 1,
    highlightRange: { start: 0, end: 4 },
  };

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders correctly", () => {
    const { container, rerender } = render(<CodeViewer {...defaultProps} />);
    expect(container.firstChild).toBeDefined();

    // Rerender with different props to trigger effects
    rerender(<CodeViewer {...defaultProps} language="javascript" />);
    rerender(<CodeViewer {...defaultProps} content="new content" />);
  });
});
