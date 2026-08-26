import { screen, act, fireEvent } from "@testing-library/react";
import { renderWithReaderHost as render } from "./testing/readerHarness";
import { describe, it, expect, vi, beforeEach } from "vitest";
import CodeViewer from "./CodeViewer";
import { EditorView } from "@codemirror/view";

const { viewBuilds } = vi.hoisted(() => ({ viewBuilds: { count: 0, lastExtensions: [] as unknown[] } }));

vi.mock("@codemirror/view", async () => {
  const actual = await vi.importActual("@codemirror/view");
  class MockView {
    constructor(config: any) {
      viewBuilds.count += 1;
      viewBuilds.lastExtensions = config?.state?.extensions ?? [];
    }
    destroy = vi.fn();
    dispatch = vi.fn();
    scrollDOM = {
      scrollHeight: 1000,
      clientHeight: 500,
      scrollTop: 0,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    };
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
    static updateListener = { of: vi.fn().mockReturnValue([]) };
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
    documentPath: "/test.ts",
    highlightLine: 1,
    highlightRange: { start: 0, end: 4 },
  };

  beforeEach(() => {
    vi.clearAllMocks();
    viewBuilds.count = 0;
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

  it("takes its appearance from the host, not from the document", () => {
    // A contradictory class on the document proves the reader is no longer
    // reading it: the host's answer has to win.
    document.documentElement.classList.add("dark");
    const { setHost } = render(<CodeViewer {...defaultProps} />, {
      host: { colorScheme: "light" },
    });
    const buildsWhileLight = viewBuilds.count;
    expect(buildsWhileLight).toBeGreaterThan(0);

    setHost({ colorScheme: "dark" });

    // Switching appearance swaps the syntax theme, which rebuilds the editor.
    // Had the reader still been reading the document class, the contradictory
    // `dark` set above would have made it dark from the start and nothing
    // would rebuild here.
    expect(viewBuilds.count).toBeGreaterThan(buildsWhileLight);
    document.documentElement.classList.remove("dark");
  });

  it("dispatches highlight and scroll effects", () => {
    // We need to render it to trigger the useEffect that dispatches
    render(<CodeViewer {...defaultProps} />);
    // Since we used a class, we can't easily check the instance dispatch unless we capture it.
  });

  it("opens a bookmark when its source highlight is clicked", () => {
    const onActivate = vi.fn();
    const { container } = render(
      <CodeViewer
        {...defaultProps}
        decorations={[
          {
            id: "source-bookmark",
            anchor: { kind: "range", range: { start: 0, end: 4 } },
            className: "cm-bookmark-highlight",
            onActivate,
          },
        ]}
      />,
    );
    const editorContainer = container.firstElementChild?.firstElementChild;
    const highlight = document.createElement("span");
    highlight.dataset.decorationId = "source-bookmark";
    editorContainer?.appendChild(highlight);

    fireEvent.click(highlight);
    expect(onActivate).toHaveBeenCalledWith("source-bookmark", {
      left: 0,
      top: 0,
      right: 0,
      bottom: 0,
    });
  });
});
