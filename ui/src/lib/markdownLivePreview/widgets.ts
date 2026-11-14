import { EditorView, WidgetType } from "@codemirror/view";
import katex from "katex";
import "katex/dist/katex.min.css";

/** What a rendered element needs from the application. */
export interface PreviewHost {
  /** Image URL for a reference, or null when it is not to be loaded. */
  imageUrl(reference: string): string | null;
  /** Follow a link the user Mod-clicked. */
  openLink(reference: string): void;
  dark: boolean;
}

/** Inline content of a table cell, taken off the syntax tree so the widget can
 *  compare itself with its successor by value. */
export type InlineNode =
  | { kind: "text"; text: string }
  | { kind: "em" | "strong" | "del"; children: InlineNode[] }
  | { kind: "code"; text: string }
  | { kind: "math"; tex: string }
  | { kind: "link"; href: string; children: InlineNode[] }
  | { kind: "image"; src: string; alt: string };

export interface TableCell {
  /** Offset of the cell's content from the start of the table's first line. */
  offset: number;
  content: InlineNode[];
}

export interface TableModel {
  align: ("left" | "center" | "right" | null)[];
  header: TableCell[];
  rows: TableCell[][];
}

export function renderMath(target: HTMLElement, tex: string, display: boolean): void {
  try {
    katex.render(tex, target, { displayMode: display, throwOnError: false });
  } catch (error) {
    // `throwOnError: false` turns parse errors into KaTeX's own red source;
    // anything reaching here is not a TeX error, so it is reported.
    console.error("KaTeX failed to render formula", error);
    target.textContent = tex;
    target.classList.add("cm-md-render-error");
  }
}

/** Put the cursor at `offset` past the widget's start, which reveals the
 *  source the widget stands for. */
function revealOnMouseDown(dom: HTMLElement, view: EditorView, offset: (event: MouseEvent) => number = () => 0) {
  dom.addEventListener("mousedown", (event) => {
    if (event.button !== 0) return;
    event.preventDefault();
    const anchor = view.posAtDOM(dom) + offset(event);
    view.dispatch({ selection: { anchor: Math.min(anchor, view.state.doc.length) } });
    view.focus();
  });
}

function imageElement(src: string, alt: string, host: PreviewHost, view: EditorView): HTMLElement {
  const url = host.imageUrl(src);
  if (!url) {
    const missing = document.createElement("span");
    missing.className = "cm-md-image-missing";
    missing.textContent = alt || src;
    missing.title = `Not loaded: ${src}`;
    return missing;
  }
  const image = document.createElement("img");
  image.className = "cm-md-image";
  image.src = url;
  image.alt = alt;
  image.title = src;
  image.addEventListener("load", () => view.requestMeasure());
  image.addEventListener("error", () => {
    console.warn(`Image could not be loaded: ${src}`);
    image.replaceWith(Object.assign(document.createElement("span"), {
      className: "cm-md-image-missing",
      textContent: alt || src,
      title: `Could not load: ${src}`,
    }));
    view.requestMeasure();
  });
  return image;
}

function renderInline(nodes: InlineNode[], host: PreviewHost, view: EditorView): DocumentFragment {
  const fragment = document.createDocumentFragment();
  for (const node of nodes) {
    switch (node.kind) {
      case "text":
        fragment.append(node.text);
        break;
      case "em":
      case "strong":
      case "del": {
        const element = document.createElement(node.kind);
        element.append(renderInline(node.children, host, view));
        fragment.append(element);
        break;
      }
      case "code": {
        const code = document.createElement("code");
        code.className = "cm-md-inline-code";
        code.textContent = node.text;
        fragment.append(code);
        break;
      }
      case "math": {
        const span = document.createElement("span");
        renderMath(span, node.tex, false);
        fragment.append(span);
        break;
      }
      case "link": {
        const link = document.createElement("span");
        link.className = "cm-md-link";
        link.dataset.href = node.href;
        link.title = `${node.href} (${navigator.platform.startsWith("Mac") ? "⌘" : "Ctrl"}-click to open)`;
        link.append(renderInline(node.children, host, view));
        fragment.append(link);
        break;
      }
      case "image":
        fragment.append(imageElement(node.src, node.alt, host, view));
        break;
    }
  }
  return fragment;
}

export class MathWidget extends WidgetType {
  /** `preview` is the rendering shown under a formula whose source is open. */
  constructor(readonly tex: string, readonly display: boolean, readonly preview = false) {
    super();
  }

  eq(other: MathWidget) {
    return other.tex === this.tex && other.display === this.display && other.preview === this.preview;
  }

  toDOM(view: EditorView) {
    const element = document.createElement(this.display ? "div" : "span");
    element.className = this.display
      ? `cm-md-math-block${this.preview ? " cm-md-math-preview" : ""}`
      : "cm-md-math-inline";
    renderMath(element, this.tex, this.display);
    if (!this.preview) revealOnMouseDown(element, view);
    return element;
  }

  ignoreEvent() {
    return true;
  }
}

export class TableWidget extends WidgetType {
  private readonly key: string;

  constructor(readonly model: TableModel, readonly host: PreviewHost) {
    super();
    this.key = JSON.stringify(model);
  }

  eq(other: TableWidget) {
    return other.key === this.key;
  }

  toDOM(view: EditorView) {
    const wrapper = document.createElement("div");
    wrapper.className = "cm-md-table-wrap";
    const table = document.createElement("table");
    table.className = "cm-md-table";
    const addRow = (parent: HTMLElement, cells: TableCell[], tag: "th" | "td") => {
      const row = document.createElement("tr");
      this.model.align.forEach((align, index) => {
        const cell = document.createElement(tag);
        const source = cells[index];
        if (align) cell.style.textAlign = align;
        if (source) {
          cell.dataset.offset = String(source.offset);
          cell.append(renderInline(source.content, this.host, view));
        }
        row.append(cell);
      });
      parent.append(row);
    };
    const head = document.createElement("thead");
    addRow(head, this.model.header, "th");
    const body = document.createElement("tbody");
    for (const row of this.model.rows) addRow(body, row, "td");
    table.append(head, body);
    wrapper.append(table);
    wrapper.addEventListener("mousedown", (event) => {
      const target = event.target instanceof Element ? event.target : null;
      const link = target?.closest<HTMLElement>("[data-href]");
      if (link && (event.metaKey || event.ctrlKey)) {
        event.preventDefault();
        event.stopPropagation();
        this.host.openLink(link.dataset.href ?? "");
      }
    }, { capture: true });
    revealOnMouseDown(wrapper, view, (event) => {
      const cell = event.target instanceof Element
        ? event.target.closest<HTMLElement>("[data-offset]")
        : null;
      return cell ? Number(cell.dataset.offset) : 0;
    });
    return wrapper;
  }

  ignoreEvent() {
    return true;
  }
}

export class ImageWidget extends WidgetType {
  constructor(readonly src: string, readonly alt: string, readonly host: PreviewHost) {
    super();
  }

  eq(other: ImageWidget) {
    return other.src === this.src && other.alt === this.alt;
  }

  toDOM(view: EditorView) {
    const wrapper = document.createElement("span");
    wrapper.className = "cm-md-image-wrap";
    wrapper.append(imageElement(this.src, this.alt, this.host, view));
    revealOnMouseDown(wrapper, view);
    return wrapper;
  }

  ignoreEvent() {
    return true;
  }
}

export class CheckboxWidget extends WidgetType {
  constructor(readonly checked: boolean) {
    super();
  }

  eq(other: CheckboxWidget) {
    return other.checked === this.checked;
  }

  toDOM(view: EditorView) {
    const box = document.createElement("input");
    box.type = "checkbox";
    box.className = "cm-md-checkbox";
    box.checked = this.checked;
    box.disabled = view.state.readOnly;
    box.setAttribute("aria-label", this.checked ? "Mark task not done" : "Mark task done");
    box.addEventListener("mousedown", (event) => event.preventDefault());
    box.addEventListener("click", (event) => {
      event.preventDefault();
      if (view.state.readOnly) return;
      const at = view.posAtDOM(box);
      const marker = view.state.sliceDoc(at, at + 3);
      if (!/^\[[ xX]\]$/.test(marker)) {
        console.error(`Task checkbox is not over a task marker (found ${JSON.stringify(marker)})`);
        return;
      }
      view.dispatch({ changes: { from: at + 1, to: at + 2, insert: this.checked ? " " : "x" } });
    });
    return box;
  }

  ignoreEvent() {
    return true;
  }
}

export class BulletWidget extends WidgetType {
  eq() {
    return true;
  }

  toDOM() {
    const bullet = document.createElement("span");
    bullet.className = "cm-md-bullet";
    bullet.textContent = "•";
    return bullet;
  }
}

export class RuleWidget extends WidgetType {
  eq() {
    return true;
  }

  toDOM() {
    const rule = document.createElement("span");
    rule.className = "cm-md-rule";
    return rule;
  }
}

export class CodeLabelWidget extends WidgetType {
  constructor(readonly language: string) {
    super();
  }

  eq(other: CodeLabelWidget) {
    return other.language === this.language;
  }

  toDOM() {
    const label = document.createElement("span");
    label.className = "cm-md-code-label";
    label.textContent = this.language;
    return label;
  }
}

type Mermaid = (typeof import("mermaid"))["default"];
let mermaidModule: Promise<Mermaid> | null = null;
let mermaidQueue: Promise<unknown> = Promise.resolve();
let mermaidCounter = 0;
const mermaidCache = new Map<string, string>();
const MERMAID_CACHE_LIMIT = 64;

/** Mermaid is large, so it is loaded the first time a document has a diagram.
 *  Renders run one at a time: Mermaid keeps global configuration and scratch
 *  DOM, and concurrent renders trample each other's. */
function renderMermaid(code: string, dark: boolean): Promise<string> {
  const key = `${dark ? "dark" : "light"}\n${code}`;
  const cached = mermaidCache.get(key);
  if (cached !== undefined) return Promise.resolve(cached);
  mermaidModule ??= import("mermaid").then((module) => module.default);
  const run = mermaidQueue.then(async () => {
    const mermaid = await mermaidModule!;
    mermaid.initialize({ startOnLoad: false, securityLevel: "strict", theme: dark ? "dark" : "default" });
    const { svg } = await mermaid.render(`cm-md-mermaid-${++mermaidCounter}`, code);
    mermaidCache.set(key, svg);
    if (mermaidCache.size > MERMAID_CACHE_LIMIT) {
      mermaidCache.delete(mermaidCache.keys().next().value!);
    }
    return svg;
  });
  mermaidQueue = run.catch(() => undefined);
  return run;
}

export class MermaidWidget extends WidgetType {
  constructor(readonly code: string, readonly dark: boolean, readonly preview = false) {
    super();
  }

  eq(other: MermaidWidget) {
    return other.code === this.code && other.dark === this.dark && other.preview === this.preview;
  }

  toDOM(view: EditorView) {
    const element = document.createElement("div");
    element.className = `cm-md-mermaid${this.preview ? " cm-md-math-preview" : ""}`;
    element.textContent = "Rendering diagram…";
    renderMermaid(this.code, this.dark).then(
      (svg) => {
        element.innerHTML = svg;
        view.requestMeasure();
      },
      (error: unknown) => {
        console.error("Mermaid failed to render diagram", error);
        element.textContent = `Diagram error: ${error instanceof Error ? error.message : String(error)}`;
        element.classList.add("cm-md-render-error");
        view.requestMeasure();
      },
    );
    if (!this.preview) revealOnMouseDown(element, view);
    return element;
  }

  ignoreEvent() {
    return true;
  }
}
