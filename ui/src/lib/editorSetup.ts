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

export interface EditorSetupOptions {
  /** Show the line-number gutter: the user's setting. */
  lineNumbers: boolean;
  /** The document reads as prose (Markdown in live preview) and is laid out
   *  like the rendered page: no fold gutter and no active-line band, in the
   *  text or beside its number. Line numbers still follow the setting. */
  prose: boolean;
}

export interface EditorSetup {
  extension: Extension;
  /** Show or hide the line-number gutter of the editor this setup belongs to. */
  setLineNumbers(view: EditorView, show: boolean): void;
}

function lineNumberGutter(show: boolean, prose: boolean): Extension {
  if (!show) return [];
  return prose ? lineNumbers() : [lineNumbers(), highlightActiveLineGutter()];
}

/** CodeMirror's `basicSetup`, taken apart as its documentation recommends
 *  for a configurable editor, so the line numbers can be switched off. One
 *  setup per editor: the line numbers live in a compartment of its own. */
export function editorSetup({ lineNumbers, prose }: EditorSetupOptions): EditorSetup {
  const gutter = new Compartment();
  return {
    extension: [
      gutter.of(lineNumberGutter(lineNumbers, prose)),
      highlightSpecialChars(),
      history(),
      prose ? [] : foldGutter(),
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
      prose ? [] : highlightActiveLine(),
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
      view.dispatch({ effects: gutter.reconfigure(lineNumberGutter(show, prose)) });
    },
  };
}
