import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import {
  bracketMatching,
  defaultHighlightStyle,
  foldGutter,
  foldKeymap,
  indentOnInput,
  syntaxHighlighting,
} from "@codemirror/language";
import { lintKeymap } from "@codemirror/lint";
import { highlightSelectionMatches, searchKeymap } from "@codemirror/search";
import { Compartment, EditorState, type Extension } from "@codemirror/state";
import {
  crosshairCursor,
  drawSelection,
  dropCursor,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  rectangularSelection,
} from "@codemirror/view";

export interface EditorSetup {
  extension: Extension;
  /** Show or hide the line-number gutter of the editor this setup belongs to. */
  setLineNumbers(view: EditorView, show: boolean): void;
}

function lineNumberGutter(show: boolean): Extension {
  return show ? [lineNumbers(), highlightActiveLineGutter()] : [];
}

/** CodeMirror's `basicSetup`, taken apart as its documentation recommends
 *  for a configurable editor, so the line numbers can be switched off. One
 *  setup per editor: the line numbers live in a compartment of its own. */
export function editorSetup(showLineNumbers: boolean): EditorSetup {
  const gutter = new Compartment();
  return {
    extension: [
      gutter.of(lineNumberGutter(showLineNumbers)),
      highlightSpecialChars(),
      history(),
      foldGutter(),
      drawSelection(),
      dropCursor(),
      EditorState.allowMultipleSelections.of(true),
      indentOnInput(),
      syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
      bracketMatching(),
      closeBrackets(),
      autocompletion(),
      rectangularSelection(),
      crosshairCursor(),
      highlightActiveLine(),
      highlightSelectionMatches(),
      keymap.of([
        ...closeBracketsKeymap,
        ...defaultKeymap,
        ...searchKeymap,
        ...historyKeymap,
        ...foldKeymap,
        ...completionKeymap,
        ...lintKeymap,
      ]),
    ],
    setLineNumbers(view, show) {
      view.dispatch({ effects: gutter.reconfigure(lineNumberGutter(show)) });
    },
  };
}
