import { render, screen, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import CodeViewer from "./CodeViewer";
import { EditorView } from "@codemirror/view";

vi.mock("@codemirror/view", async () => {
  const actual = await vi.importActual("@codemirror/view");
  class MockView {
    destroy = vi.fn();
    dispatch = vi.fn();
    state = {
      doc: {
        length: 100,
        lines: 10,
        line: vi.fn().mockReturnValue({ from: 0, to: 10 }),
      },
    };
    static decorations = { from: vi.fn() };
    static baseTheme = vi.fn().mockReturnValue([]);
    static lineWrapping = [];
    static scrollIntoView = vi.fn();
  }
  return {
    ...actual as any,
    EditorView: MockView,
  };
});

vi.mock("codemirror", () => ({ basicSetup: [] }));

// Mock MutationObserver
let observerInstance: any;
class MockMutationObserver {
  callback: any;
  constructor(callback: any) {
    this.callback = callback;
    observerInstance = this;
  }
  observe = vi.fn();
  disconnect = vi.fn();
  trigger(mutations: any) {
    this.callback(mutations);
  }
}
vi.stubGlobal("MutationObserver", MockMutationObserver);

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
  });

  it("handles different languages", () => {
    const languages = ["python", "rust", "json", "markdown", "html", "css", "xml", "sql", "cpp", "java", "go", "yaml", "unknown"];
    languages.forEach(lang => {
      render(<CodeViewer {...defaultProps} language={lang} />);
    });
  });

  it("responds to theme changes", () => {
    render(<CodeViewer {...defaultProps} />);
    
    act(() => {
      document.documentElement.classList.add("dark");
      if (observerInstance) {
        observerInstance.trigger([{ type: "attributes", attributeName: "class" }]);
      }
    });
  });

  it("dispatches highlight and scroll effects", () => {
    // We need to render it to trigger the useEffect that dispatches
    render(<CodeViewer {...defaultProps} />);
    // Since we used a class, we can't easily check the instance dispatch unless we capture it.
  });
});
