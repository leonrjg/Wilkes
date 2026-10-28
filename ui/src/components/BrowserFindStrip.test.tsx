import { act, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../services", () => ({
  isTauri: true,
  api: {
    browserPaneFind: vi.fn(() => Promise.resolve({ found: true, matches: 3 })),
  },
}));

import { api } from "../services";
import BrowserFindStrip, { findStatus } from "./BrowserFindStrip";

async function type(text: string) {
  await act(async () => {
    fireEvent.change(screen.getByLabelText("Find in page"), { target: { value: text } });
  });
}

describe("BrowserFindStrip", () => {
  beforeEach(() => vi.clearAllMocks());

  it("says what a find turned up, in as much as the platform tells", () => {
    expect(findStatus(null)).toBe("");
    expect(findStatus({ found: false, matches: 0 })).toBe("No matches");
    expect(findStatus({ found: true, matches: 1 })).toBe("1 match");
    expect(findStatus({ found: true, matches: 12 })).toBe("12 matches");
    expect(findStatus({ found: true, matches: null })).toBe("");
  });

  it("finds as it is typed, and has the keyboard when it opens", async () => {
    render(<BrowserFindStrip requests={1} onClose={vi.fn()} />);
    expect(screen.getByLabelText("Find in page")).toHaveFocus();
    await type("attention");
    expect(api.browserPaneFind).toHaveBeenLastCalledWith("attention", false);
    expect(screen.getByRole("status")).toHaveTextContent("3 matches");
  });

  it("steps forward on Enter and back on Shift+Enter", async () => {
    render(<BrowserFindStrip requests={1} onClose={vi.fn()} />);
    await type("attention");
    const input = screen.getByLabelText("Find in page");
    await act(async () => {
      fireEvent.keyDown(input, { key: "Enter" });
    });
    expect(api.browserPaneFind).toHaveBeenLastCalledWith("attention", false);
    await act(async () => {
      fireEvent.keyDown(input, { key: "Enter", shiftKey: true });
    });
    expect(api.browserPaneFind).toHaveBeenLastCalledWith("attention", true);
  });

  it("does not search for nothing", async () => {
    render(<BrowserFindStrip requests={1} onClose={vi.fn()} />);
    await type("a");
    await type("");
    expect(api.browserPaneFind).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("reports only the latest of finds still in flight", async () => {
    let answerFirst: (found: { found: boolean; matches: number | null }) => void = () => {};
    vi.mocked(api.browserPaneFind!)
      .mockImplementationOnce(() => new Promise((resolve) => (answerFirst = resolve)))
      .mockImplementationOnce(() => Promise.resolve({ found: false, matches: 0 }));
    render(<BrowserFindStrip requests={1} onClose={vi.fn()} />);
    await type("a");
    await type("ax");
    await act(async () => answerFirst({ found: true, matches: 40 }));
    expect(screen.getByRole("status")).toHaveTextContent("No matches");
  });

  it("closes on Escape", async () => {
    const onClose = vi.fn();
    render(<BrowserFindStrip requests={1} onClose={onClose} />);
    fireEvent.keyDown(screen.getByLabelText("Find in page"), { key: "Escape" });
    expect(onClose).toHaveBeenCalled();
  });
});
