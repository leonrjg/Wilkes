import { afterEach, describe, expect, it, vi } from "vitest";

// The shared test setup replaces CodeMirror with stubs for components that
// only mount it; this suite is about what CodeMirror draws.
vi.unmock("@codemirror/view");
vi.unmock("@codemirror/state");

const mermaidRender = vi.fn(async (_id: string, code: string) => ({ svg: `<svg data-code="${code}"></svg>` }));
vi.mock("mermaid", () => ({
  default: { initialize: vi.fn(), render: mermaidRender },
}));

import { markdownLanguage } from "@codemirror/lang-markdown";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { markdownLivePreview } from ".";
import { mathSyntax } from "./mathSyntax";

const views: EditorView[] = [];

afterEach(() => {
  for (const view of views.splice(0)) {
    view.dom.remove();
    view.destroy();
  }
  vi.clearAllMocks();
});

function mount(doc: string, cursor = doc.length, options: { readOnly?: boolean } = {}) {
  const resolveAsset = vi.fn((path: string) => `asset://${path}`);
  const openLink = vi.fn();
  const parent = document.createElement("div");
  document.body.append(parent);
  const view = new EditorView({
    state: EditorState.create({
      doc,
      selection: { anchor: cursor },
      extensions: [
        markdownLivePreview({ documentPath: "/notes/draft.md", dark: false, resolveAsset, openLink }),
        ...(options.readOnly ? [EditorState.readOnly.of(true)] : []),
      ],
    }),
    parent,
  });
  views.push(view);
  return { view, resolveAsset, openLink };
}

function lineText(view: EditorView, number: number): string {
  return view.contentDOM.querySelectorAll(".cm-line")[number - 1]?.textContent ?? "";
}

function nodeNames(doc: string): string[] {
  const parser = (markdownLanguage.parser as unknown as { configure(c: unknown): { parse(t: string): { iterate(s: object): void } } })
    .configure(mathSyntax);
  const names: string[] = [];
  parser.parse(doc).iterate({ enter: (node: { name: string }) => { names.push(node.name); } });
  return names;
}

describe("math syntax", () => {
  it("parses inline and display math but leaves prices alone", () => {
    expect(nodeNames("Euler: $e^{i\\pi}+1=0$.")).toContain("InlineMath");
    expect(nodeNames("It costs $5 and $6 today.")).not.toContain("InlineMath");
    expect(nodeNames("Escaped \\$x$ is text.")).not.toContain("InlineMath");
    expect(nodeNames("$$\n\\int_0^1 x\\,dx\n$$\n")).toContain("BlockMath");
    expect(nodeNames("$$ a^2 $$\n")).toContain("BlockMath");
  });

  it("stops an unclosed display formula at the first blank line", () => {
    const { view } = mount("$$\nx\n\nAfter the formula.", 0);
    expect(view.contentDOM.textContent).toContain("After the formula.");
    expect(nodeNames("$$\nx\n\nAfter the formula.")).toContain("Paragraph");
  });
});

describe("markdown live preview", () => {
  it("never changes the document text", () => {
    const doc = "# Title\n\nSome **bold** and *em* with `code` and $x^2$.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
    const { view } = mount(doc, 0);
    expect(view.state.doc.toString()).toBe(doc);
  });

  it("hides heading and emphasis markup off the cursor line and shows it on it", () => {
    const doc = "# Title\n\nSome **bold** text.\n\nLast";
    const { view } = mount(doc, doc.length);
    expect(lineText(view, 1)).toBe("Title");
    expect(view.contentDOM.querySelector(".cm-md-h1")).not.toBeNull();
    expect(lineText(view, 3)).toBe("Some bold text.");
    expect(view.contentDOM.querySelector(".cm-md-strong")?.textContent).toBe("bold");

    view.dispatch({ selection: { anchor: 2 } });
    expect(lineText(view, 1)).toBe("# Title");
    view.dispatch({ selection: { anchor: doc.indexOf("bold") } });
    expect(lineText(view, 3)).toBe("Some **bold** text.");
  });

  it("draws a table with alignment and empty cells, and shows its source when the cursor enters", () => {
    const doc = "Intro\n\n| Name | Score | Note |\n|:-----|------:|:----:|\n| *Ada* |  | $x$ |\n| Bob | 2 | ok |\n\nEnd";
    const { view } = mount(doc, 0);
    const table = view.contentDOM.querySelector("table.cm-md-table");
    expect(table).not.toBeNull();
    const headers = Array.from(table!.querySelectorAll("th")).map((cell) => cell.textContent);
    expect(headers).toEqual(["Name", "Score", "Note"]);
    const firstRow = Array.from(table!.querySelectorAll("tbody tr")[0].querySelectorAll("td"));
    expect(firstRow).toHaveLength(3);
    expect(firstRow[0].querySelector("em")?.textContent).toBe("Ada");
    expect(firstRow[1].textContent).toBe("");
    expect(firstRow[2].querySelector(".katex")).not.toBeNull();
    expect((table!.querySelectorAll("th")[1] as HTMLElement).style.textAlign).toBe("right");
    expect((table!.querySelectorAll("th")[2] as HTMLElement).style.textAlign).toBe("center");

    view.dispatch({ selection: { anchor: doc.indexOf("Bob") } });
    expect(view.contentDOM.querySelector("table.cm-md-table")).toBeNull();
    expect(view.contentDOM.textContent).toContain("| Bob | 2 | ok |");
  });

  it("puts the cursor in the clicked cell's source", () => {
    const doc = "Intro\n\n| A | B |\n|---|---|\n| one | two |\n\nEnd";
    const { view } = mount(doc, 0);
    const cell = Array.from(view.contentDOM.querySelectorAll("td")).find((td) => td.textContent === "two")!;
    cell.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 0 }));
    expect(view.state.selection.main.head).toBe(doc.indexOf("two"));
    expect(view.contentDOM.querySelector("table.cm-md-table")).toBeNull();
  });

  it("steps into a table with the arrow keys", () => {
    const doc = "Intro\n| A |\n|---|\n| 1 |\nEnd";
    const { view } = mount(doc, 0);
    expect(view.contentDOM.querySelector("table.cm-md-table")).not.toBeNull();
    view.contentDOM.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    expect(view.state.selection.main.head).toBe(doc.indexOf("| A |"));
    expect(view.contentDOM.querySelector("table.cm-md-table")).toBeNull();
  });

  it("renders inline and display math with KaTeX and previews display math being edited", () => {
    const doc = "Inline $a^2$ here.\n\n$$\n\\frac{1}{2}\n$$\n\nEnd";
    const { view } = mount(doc, doc.length);
    expect(view.contentDOM.querySelector(".cm-md-math-inline .katex")).not.toBeNull();
    expect(view.contentDOM.querySelector(".cm-md-math-block .katex-display")).not.toBeNull();

    view.dispatch({ selection: { anchor: doc.indexOf("\\frac") } });
    expect(view.contentDOM.textContent).toContain("\\frac{1}{2}");
    expect(view.contentDOM.querySelector(".cm-md-math-preview .katex-display")).not.toBeNull();
  });

  it("toggles a task when its checkbox is clicked, unless read-only", () => {
    const doc = "- [ ] Write\n- [x] Read\n\nEnd";
    const { view } = mount(doc, doc.length);
    const boxes = view.contentDOM.querySelectorAll<HTMLInputElement>("input.cm-md-checkbox");
    expect(Array.from(boxes).map((box) => box.checked)).toEqual([false, true]);
    boxes[0].click();
    expect(view.state.doc.toString()).toBe("- [x] Write\n- [x] Read\n\nEnd");

    const readOnly = mount(doc, doc.length, { readOnly: true }).view;
    const box = readOnly.contentDOM.querySelector<HTMLInputElement>("input.cm-md-checkbox")!;
    expect(box.disabled).toBe(true);
    box.click();
    expect(readOnly.state.doc.toString()).toBe(doc);
  });

  it("loads local images beside the document but not remote ones", () => {
    const doc = "![diagram](../img/fig%201.png)\n\n![remote](https://example.com/x.png)\n\nEnd";
    const { view, resolveAsset } = mount(doc, doc.length);
    expect(resolveAsset).toHaveBeenCalledWith("/img/fig 1.png");
    expect(view.contentDOM.querySelector<HTMLImageElement>("img.cm-md-image")?.getAttribute("src")).toBe("asset:///img/fig 1.png");
    expect(view.contentDOM.querySelector(".cm-md-image-missing")?.textContent).toBe("remote");
  });

  it("opens a Mod-clicked link resolved against the document", () => {
    const doc = "See [notes](other.md) and [site](https://example.com).\n\nEnd";
    const { view, openLink } = mount(doc, doc.length);
    const links = view.contentDOM.querySelectorAll<HTMLElement>(".cm-md-link");
    links[0].dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 0, metaKey: true }));
    links[1].dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 0, ctrlKey: true }));
    expect(openLink.mock.calls).toEqual([["/notes/other.md"], ["https://example.com"]]);
  });

  it("renders a Mermaid diagram and shows its source when the cursor enters", async () => {
    const doc = "Intro\n\n```mermaid\ngraph TD; A-->B\n```\n\nEnd";
    const { view } = mount(doc, 0);
    await vi.waitFor(() => expect(view.contentDOM.querySelector(".cm-md-mermaid svg")).not.toBeNull());
    expect(mermaidRender).toHaveBeenCalledWith(expect.any(String), "graph TD; A-->B");

    view.dispatch({ selection: { anchor: doc.indexOf("graph") } });
    expect(view.contentDOM.textContent).toContain("```mermaid");
  });

  it("styles fenced code and hides its fences off the cursor", () => {
    const doc = "```python\nprint(1)\n```\n\nEnd";
    const { view } = mount(doc, doc.length);
    expect(view.contentDOM.querySelectorAll(".cm-md-codeblock")).toHaveLength(3);
    expect(view.contentDOM.querySelector(".cm-md-code-label")?.textContent).toBe("python");
    expect(view.contentDOM.textContent).not.toContain("```");
  });
});
