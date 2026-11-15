import { afterEach, describe, expect, it, vi } from "vitest";

// The shared test setup stubs CodeMirror; this checks the real gutter.
vi.unmock("@codemirror/view");
vi.unmock("@codemirror/state");

import { undo } from "@codemirror/commands";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { editorSetup } from "./editorSetup";

const views: EditorView[] = [];

afterEach(() => {
  for (const view of views.splice(0)) view.destroy();
});

function mount(lineNumbers: boolean, prose = false) {
  const setup = editorSetup({ lineNumbers, prose });
  const view = new EditorView({
    state: EditorState.create({ doc: "one\ntwo\nthree", extensions: setup.extension }),
    parent: document.body,
  });
  views.push(view);
  return { setup, view };
}

describe("editor setup", () => {
  it("shows the line-number gutter only when asked to", () => {
    expect(mount(true).view.dom.querySelector(".cm-lineNumbers")).not.toBeNull();
    expect(mount(false).view.dom.querySelector(".cm-lineNumbers")).toBeNull();
  });

  it("switches line numbers in place, keeping the document and its history", () => {
    const { setup, view } = mount(false);
    view.dispatch({ changes: { from: 0, insert: "zero\n" } });

    setup.setLineNumbers(view, true);
    expect(view.dom.querySelector(".cm-lineNumbers")).not.toBeNull();
    expect(view.state.doc.toString()).toBe("zero\none\ntwo\nthree");

    setup.setLineNumbers(view, false);
    expect(view.dom.querySelector(".cm-lineNumbers")).toBeNull();
    // The fold gutter is not a line-number display and stays.
    expect(view.dom.querySelector(".cm-foldGutter")).not.toBeNull();

    undo(view);
    expect(view.state.doc.toString()).toBe("one\ntwo\nthree");
  });

  it("draws prose without a fold gutter or an active-line band, and line numbers as set", () => {
    const { setup, view } = mount(false, true);
    expect(view.dom.querySelector(".cm-gutters")).toBeNull();
    expect(view.dom.querySelector(".cm-activeLine")).toBeNull();

    setup.setLineNumbers(view, true);
    expect(view.dom.querySelector(".cm-lineNumbers")).not.toBeNull();
    expect(view.dom.querySelector(".cm-foldGutter")).toBeNull();
    expect(view.dom.querySelector(".cm-activeLineGutter")).toBeNull();
  });
});
