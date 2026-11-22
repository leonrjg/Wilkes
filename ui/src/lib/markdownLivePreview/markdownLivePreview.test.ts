import { afterEach, describe, expect, it, vi } from "vitest";

// The shared test setup replaces CodeMirror with stubs for components that
// only mount it; this suite is about what CodeMirror draws.
vi.unmock("@codemirror/view");
vi.unmock("@codemirror/state");

const mermaidRender = vi.fn(async (_id: string, code: string) => ({ svg: `<svg data-code="${code}"></svg>` }));
vi.mock("mermaid", () => ({
  default: { initialize: vi.fn(), render: mermaidRender },
}));

import { defaultKeymap } from "@codemirror/commands";
import { markdownLanguage } from "@codemirror/lang-markdown";
import { EditorSelection, EditorState, type SelectionRange } from "@codemirror/state";
import { EditorView, keymap, lineNumbers } from "@codemirror/view";
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
        // What the preview's own keys fall back to, as in the editor.
        keymap.of(defaultKeymap),
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

function lineClasses(view: EditorView): string[] {
  return [...view.contentDOM.querySelectorAll(".cm-line")].map((line) => line.className.replace("cm-line", "").trim());
}

function press(view: EditorView, key: string, modifiers: KeyboardEventInit = {}) {
  view.contentDOM.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...modifiers }));
}

/** jsdom lays nothing out, so vertical motion is given the answer a browser
 *  measures: the start of the next or previous line. */
function moveLineWise(view: EditorView) {
  return vi.spyOn(view, "moveVertically").mockImplementation((start: SelectionRange, forward: boolean) => {
    const { doc } = view.state;
    const number = doc.lineAt(start.head).number + (forward ? 1 : -1);
    return number < 1 || number > doc.lines ? start : EditorSelection.cursor(doc.line(number).from);
  });
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
    // A browser's own motion goes past the widget.
    vi.spyOn(view, "moveVertically").mockReturnValue(EditorSelection.cursor(doc.indexOf("End")));
    press(view, "ArrowDown");
    expect(view.state.selection.main.head).toBe(doc.indexOf("| A |"));
    expect(view.contentDOM.querySelector("table.cm-md-table")).toBeNull();
  });

  it("steps into a table across the margin above or below it", () => {
    const doc = "Intro\n\n| A |\n|---|\n| 1 |\n\nEnd";
    const { view } = mount(doc, 0);
    moveLineWise(view);
    press(view, "ArrowDown");
    expect(view.state.selection.main.head).toBe(doc.indexOf("| A |"));

    view.dispatch({ selection: { anchor: doc.length } });
    expect(view.contentDOM.querySelector("table.cm-md-table")).not.toBeNull();
    press(view, "ArrowUp");
    expect(view.state.selection.main.head).toBe(doc.indexOf("| 1 |"));
  });

  it("leaves Down inside a wrapped line to the editor", () => {
    const doc = "A long line that a browser wraps\n| A |\n|---|\n| 1 |\nEnd";
    const { view } = mount(doc, 0);
    // The next row of the same line.
    vi.spyOn(view, "moveVertically").mockReturnValue(EditorSelection.cursor(12));
    press(view, "ArrowDown");
    expect(view.state.selection.main.head).toBe(12);
    expect(view.contentDOM.querySelector("table.cm-md-table")).not.toBeNull();
  });

  it("passes over the margin between two blocks with Up and Down", () => {
    const doc = "First\n\nSecond\n\n\n\nThird\n";
    const { view } = mount(doc, 0);
    moveLineWise(view);
    const line = () => view.state.doc.lineAt(view.state.selection.main.head).number;
    const visited = [line()];
    for (let step = 0; step < 5; step++) {
      press(view, "ArrowDown");
      visited.push(line());
    }
    // Lines 2 and 6 are margins; 4 and 5 are blank lines drawn as lines. The
    // margin that ends the document is where writing goes on, so it is reached.
    expect(visited).toEqual([1, 3, 4, 5, 7, 8]);
    for (let step = 0; step < 5; step++) {
      press(view, "ArrowUp");
      visited.push(line());
    }
    expect(visited.slice(6)).toEqual([7, 5, 4, 3, 1]);
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
    // The fences leave only the block's padding: no text, no language label.
    expect(view.contentDOM.querySelectorAll(".cm-md-fence")).toHaveLength(2);
    expect(view.contentDOM.textContent).not.toContain("```");
    expect(view.contentDOM.textContent).not.toContain("python");

    view.dispatch({ selection: { anchor: doc.indexOf("print") } });
    expect(view.contentDOM.querySelectorAll(".cm-md-fence")).toHaveLength(0);
    expect(view.contentDOM.textContent).toContain("```python");
  });

  it("shrinks one blank line between blocks to the reader's margin", () => {
    const doc = "Intro\n\n\n## Heading\n\nText\n- one\n- two\n\n```\na\n\nb\n```\n\nEnd";
    const { view } = mount(doc, doc.indexOf("Text"));
    const classes = (number: number) => lineClasses(view)[number - 1];
    // A run before a heading: its last line is the heading's margin, the
    // other a blank line like any.
    expect(classes(2)).toBe("");
    expect(classes(3)).toBe("cm-md-gap cm-md-gap-heading");
    expect(classes(5)).toBe("cm-md-gap cm-md-gap-block");
    // The heading touches nothing; tight list items are parted a little.
    expect(classes(4)).not.toContain("cm-md-space");
    expect(classes(8)).toContain("cm-md-item-next");
    // A blank line inside code is code.
    expect(classes(12)).not.toContain("cm-md-gap");
    expect(classes(15)).toBe("cm-md-gap cm-md-gap-block");

    // The blank line holding the caret keeps its height to type on.
    view.dispatch({ selection: { anchor: doc.indexOf("Heading") + "Heading\n".length } });
    expect(classes(5)).toBe("");
  });

  it("draws every blank line Enter adds", () => {
    const doc = "First\n\nSecond";
    const { view } = mount(doc, "First".length);
    const enter = () => {
      const at = view.state.selection.main.head;
      view.dispatch({ changes: { from: at, insert: "\n" }, selection: { anchor: at + 1 } });
    };
    const blankLinesDrawn = () => lineClasses(view).slice(1, -1).filter((classes) => classes === "").length;
    const margins = () => lineClasses(view).filter((classes) => classes.includes("cm-md-gap")).length;
    expect([blankLinesDrawn(), margins()]).toEqual([0, 1]);
    for (let presses = 1; presses <= 4; presses++) {
      enter();
      // One line more each time, and still the one margin, under the caret's run.
      expect([blankLinesDrawn(), margins()]).toEqual([presses, 1]);
    }
    expect(lineClasses(view)).toEqual(["", "", "", "", "", "cm-md-gap cm-md-gap-block", ""]);

    // The margin is never the caret's line: on the run's last line it moves up.
    view.dispatch({ selection: { anchor: view.state.doc.line(6).from } });
    expect(lineClasses(view)).toEqual(["", "", "", "", "cm-md-gap cm-md-gap-block", "", ""]);
  });

  it("draws blank lines above the first block as lines", () => {
    const doc = "First";
    const { view } = mount(doc, 0);
    view.dispatch({ changes: { from: 0, insert: "\n" }, selection: { anchor: 1 } });
    expect(lineClasses(view)).toEqual(["", ""]);
    view.dispatch({ changes: { from: 0, insert: "\n\n" }, selection: { anchor: 3 } });
    expect(lineClasses(view)).toEqual(["", "", "", ""]);
  });

  it("keeps a new paragraph at the end of the document where it was begun", () => {
    const { view } = mount("First\n\n", "First\n\n".length);
    // The caret's line is a line; the blank one above it is the margin.
    expect(lineClasses(view)).toEqual(["", "cm-md-gap cm-md-gap-block", ""]);
    view.dispatch({ changes: { from: view.state.doc.length, insert: "N" }, selection: { anchor: view.state.doc.length + 1 } });
    expect(lineClasses(view)).toEqual(["", "cm-md-gap cm-md-gap-block", ""]);
  });

  it("gives each line number its line's classes, so collapsed lines show none", () => {
    const parent = document.createElement("div");
    document.body.append(parent);
    const doc = "# Title\n\nText";
    const view = new EditorView({
      state: EditorState.create({
        doc,
        selection: { anchor: doc.length },
        extensions: [
          lineNumbers(),
          markdownLivePreview({ documentPath: "/notes/draft.md", dark: false, resolveAsset: vi.fn(), openLink: vi.fn() }),
        ],
      }),
      parent,
    });
    views.push(view);
    const numbers = [...view.dom.querySelectorAll(".cm-lineNumbers .cm-gutterElement")]
      .filter((element) => /^\d+$/.test(element.textContent ?? ""));
    const byNumber = (n: string) => numbers.find((element) => element.textContent === n)?.className ?? "";
    expect(byNumber("1")).toContain("cm-md-h1");
    expect(byNumber("2")).toContain("cm-md-gap");
    expect(byNumber("3")).not.toContain("cm-md-gap");
  });

  it("keeps a heading's own margins where no blank line parts it from its neighbours", () => {
    const doc = "Intro\n# Title\nText";
    const { view } = mount(doc, doc.length);
    const heading = view.contentDOM.querySelectorAll(".cm-line")[1].className;
    expect(heading).toContain("cm-md-space-above");
    expect(heading).toContain("cm-md-space-below");
  });

  it("nests and lifts list items with Tab and Shift-Tab", () => {
    const doc = "- one\n- two\n  more\n  - inner\n- three\n\n1. first\n2. second";
    const { view } = mount(doc, doc.indexOf("two"));
    press(view, "Tab");
    // Under the previous item's text, with everything the item holds.
    expect(view.state.doc.toString()).toBe("- one\n  - two\n    more\n    - inner\n- three\n\n1. first\n2. second");
    // Already as deep as it can go.
    press(view, "Tab");
    expect(view.state.doc.toString()).toBe("- one\n  - two\n    more\n    - inner\n- three\n\n1. first\n2. second");
    press(view, "Tab", { shiftKey: true });
    expect(view.state.doc.toString()).toBe(doc);
    // A numbered item's text starts three columns in.
    view.dispatch({ selection: { anchor: doc.indexOf("second") } });
    press(view, "Tab");
    expect(view.state.doc.toString()).toBe("- one\n- two\n  more\n  - inner\n- three\n\n1. first\n   2. second");
  });

  it("takes Tab in prose without changing it, and leaves code to the editor", () => {
    const doc = "Prose\n\n```\ncode\n```";
    const { view } = mount(doc, 2);
    const tab = () => {
      const event = new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
      view.contentDOM.dispatchEvent(event);
      return event.defaultPrevented;
    };
    // Taken, so focus stays in the editor; a paragraph indented is a code block.
    expect(tab()).toBe(true);
    expect(view.state.doc.toString()).toBe(doc);
    view.dispatch({ selection: { anchor: doc.indexOf("code") } });
    // Nothing else is bound here; the editor's setup indents.
    expect(tab()).toBe(false);
  });

  it("toggles strong and plain emphasis around the selection", () => {
    const doc = "Some plain words.";
    const { view } = mount(doc, 0);
    const text = () => view.state.doc.toString();
    const mod = { ctrlKey: true };
    view.dispatch({ selection: { anchor: 5, head: 10 } });
    press(view, "b", mod);
    expect(text()).toBe("Some **plain** words.");
    expect(view.state.sliceDoc(view.state.selection.main.from, view.state.selection.main.to)).toBe("plain");
    press(view, "i", mod);
    expect(text()).toBe("Some ***plain*** words.");
    press(view, "b", mod);
    expect(text()).toBe("Some *plain* words.");
    press(view, "i", mod);
    expect(text()).toBe(doc);

    // Nothing selected: empty markers to type into, and out again.
    view.dispatch({ selection: { anchor: 5 } });
    press(view, "b", mod);
    expect(text()).toBe("Some ****plain words.");
    expect(view.state.selection.main.head).toBe(7);
    press(view, "i", mod);
    expect(text()).toBe("Some ******plain words.");
    press(view, "b", mod);
    expect(text()).toBe("Some **plain words.");
    press(view, "i", mod);
    expect(text()).toBe(doc);
  });

  it("makes the selection a link", () => {
    const doc = "See the notes at https://example.com now.";
    const { view } = mount(doc, 0);
    view.dispatch({ selection: { anchor: doc.indexOf("notes"), head: doc.indexOf("notes") + 5 } });
    press(view, "k", { ctrlKey: true });
    expect(view.state.doc.toString()).toBe("See the [notes]() at https://example.com now.");
    expect(view.state.selection.main.head).toBe("See the [notes](".length);

    const url = view.state.doc.toString().indexOf("https");
    view.dispatch({ selection: { anchor: url, head: url + "https://example.com".length } });
    press(view, "k", { ctrlKey: true });
    expect(view.state.doc.toString()).toBe("See the [notes]() at [](https://example.com) now.");
    expect(view.state.selection.main.head).toBe(url + 1);
  });
});
