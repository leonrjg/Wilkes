import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach, vi } from "vitest";

const localStorageValues = new Map<string, string>();
Object.defineProperty(window, "localStorage", {
  configurable: true,
  value: {
    getItem: vi.fn((key: string) => localStorageValues.get(key) ?? null),
    setItem: vi.fn((key: string, value: string) => {
      localStorageValues.set(key, String(value));
    }),
    removeItem: vi.fn((key: string) => {
      localStorageValues.delete(key);
    }),
    clear: vi.fn(() => {
      localStorageValues.clear();
    }),
    key: vi.fn((index: number) => Array.from(localStorageValues.keys())[index] ?? null),
    get length() {
      return localStorageValues.size;
    },
  },
});

// pdf.js constructs `new DOMMatrix()` at module-evaluation time (its canvas
// module's SCALE_MATRIX), and jsdom does not implement DOMMatrix. Any test that
// imports the readers' public surface therefore loads pdf.js and dies on
// import, before a single assertion runs. No test renders a PDF canvas, so an
// identity-matrix stand-in is enough to let the module evaluate; a test that
// genuinely needs matrix maths should mock pdf.js itself rather than lean on
// this.
if (!("DOMMatrix" in globalThis)) {
  class DOMMatrixStub {
    a = 1; b = 0; c = 0; d = 1; e = 0; f = 0;
    constructor(_init?: unknown) {}
  }
  Object.defineProperty(globalThis, "DOMMatrix", {
    configurable: true,
    writable: true,
    value: DOMMatrixStub,
  });
}

// Runs a cleanup after each test case (e.g. clearing jsdom)
afterEach(() => {
  cleanup();
});

// Mocking some common browser APIs that might be missing in jsdom
Object.defineProperty(window, "matchMedia", {
  writable: true,
  value: vi.fn().mockImplementation((query) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: vi.fn(), // deprecated
    removeListener: vi.fn(), // deprecated
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    dispatchEvent: vi.fn(),
  })),
});

// Mock ResizeObserver
global.ResizeObserver = vi.fn().mockImplementation(function() {
  this.observe = vi.fn();
  this.unobserve = vi.fn();
  this.disconnect = vi.fn();
});

Object.defineProperty(navigator, "clipboard", {
  configurable: true,
  value: {
    writeText: vi.fn().mockResolvedValue(undefined),
  },
});

// Mock CodeMirror for all tests
vi.mock("@codemirror/view", () => {
  function MockView(config?: { state?: unknown }) {
    this.destroy = vi.fn();
    this.dispatch = vi.fn();
    this.state = config?.state ?? {
      doc: { toString: () => "{}", length: 2 },
      selection: { main: { empty: true, head: 2 } },
      sliceDoc: (from: number, to: number) => "{}".slice(from, to),
    };
  }
  MockView.theme = vi.fn().mockReturnValue({});
  MockView.baseTheme = vi.fn().mockReturnValue({});
  MockView.decorations = { from: vi.fn() };
  MockView.lineWrapping = {};
  MockView.scrollIntoView = vi.fn();
  MockView.updateListener = { of: vi.fn() };
  class MockWidgetType {}
  return {
    EditorView: MockView,
    Decoration: {
      none: {},
      mark: vi.fn(),
      set: vi.fn().mockReturnValue({}),
      widget: vi.fn().mockReturnValue({ range: vi.fn().mockReturnValue({}) }),
    },
    WidgetType: MockWidgetType,
    keymap: { of: vi.fn() },
  };
});

vi.mock("@codemirror/state", () => ({
  EditorState: {
    create: vi.fn(({ doc }: { doc: string }) => ({
      doc: { toString: () => doc, length: doc.length },
      selection: { main: { empty: true, head: doc.length } },
      sliceDoc: (from: number, to: number) => doc.slice(from, to),
    })),
    readOnly: { of: vi.fn() },
  },
  RangeSetBuilder: vi.fn().mockImplementation(() => ({
    add: vi.fn(),
    finish: vi.fn(),
  })),
  StateField: { define: vi.fn() },
  StateEffect: { define: vi.fn(() => ({ of: vi.fn(), is: vi.fn() })) },
  Prec: { highest: vi.fn((value) => value) },
}));

// Mocking Tauri APIs (since we're in a Tauri app)
vi.mock("@tauri-apps/api", () => ({
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
  save: vi.fn(),
  ask: vi.fn().mockResolvedValue(true),
}));
