import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach, vi } from "vitest";

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
  function MockView() {
    this.destroy = vi.fn();
    this.dispatch = vi.fn();
    this.state = { doc: { toString: () => "{}", length: 0 } };
  }
  MockView.theme = vi.fn().mockReturnValue({});
  MockView.baseTheme = vi.fn().mockReturnValue({});
  MockView.decorations = { from: vi.fn() };
  MockView.lineWrapping = {};
  MockView.scrollIntoView = vi.fn();
  return {
    EditorView: MockView,
    Decoration: { none: {}, mark: vi.fn() },
    keymap: { of: vi.fn() },
  };
});

vi.mock("@codemirror/state", () => ({
  EditorState: {
    create: vi.fn().mockReturnValue({ doc: { toString: () => "{}", length: 0 } }),
    readOnly: { of: vi.fn() },
  },
  RangeSetBuilder: vi.fn().mockImplementation(() => ({
    add: vi.fn(),
    finish: vi.fn(),
  })),
  StateField: { define: vi.fn() },
  StateEffect: { define: vi.fn(() => ({ of: vi.fn(), is: vi.fn() })) },
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
