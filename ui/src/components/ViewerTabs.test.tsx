import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import { useViewerStore, type ViewerTab } from "../stores/useViewerStore";
import ViewerTabs from "./ViewerTabs";

function tab(id: string, path: string): ViewerTab {
  const match = {
    path,
    origin: { TextFile: { line: 0, col: 0 } },
  } as const;
  return {
    id,
    path,
    match,
    history: [match],
    historyIndex: 0,
    previewData: null,
    previewLoading: false,
    metadata: null,
    metadataStatus: "idle",
    requestId: 1,
  };
}

describe("ViewerTabs", () => {
  beforeEach(() => {
    useViewerStore.setState({
      tabs: [
        tab("one", "/docs/one.txt"),
        tab("two", "/other/two.txt"),
        tab("three", "/docs/three.txt"),
      ],
      activeTabId: "one",
    });
  });

  it("renders an accessible tablist and activates a clicked document", () => {
    render(<ViewerTabs />);

    expect(screen.getByRole("tablist", { name: "Open documents" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "one.txt" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByRole("tab", { name: "one.txt" })).toHaveClass(
      "h-full",
      "select-none",
    );

    fireEvent.click(screen.getByRole("tab", { name: "two.txt" }));

    expect(useViewerStore.getState().activeTabId).toBe("two");
    expect(screen.getByRole("tab", { name: "two.txt" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  it("supports arrow, Home, and End keyboard navigation", () => {
    render(<ViewerTabs />);
    const first = screen.getByRole("tab", { name: "one.txt" });

    fireEvent.keyDown(first, { key: "ArrowRight" });
    expect(useViewerStore.getState().activeTabId).toBe("two");

    fireEvent.keyDown(screen.getByRole("tab", { name: "two.txt" }), { key: "End" });
    expect(useViewerStore.getState().activeTabId).toBe("three");

    fireEvent.keyDown(screen.getByRole("tab", { name: "three.txt" }), { key: "Home" });
    expect(useViewerStore.getState().activeTabId).toBe("one");
  });

  it("closes tabs from the close button, Delete key, and middle click", () => {
    render(<ViewerTabs />);

    fireEvent.click(screen.getByRole("button", { name: "Close two.txt" }));
    fireEvent.keyDown(screen.getByRole("tab", { name: "one.txt" }), { key: "Delete" });
    fireEvent(
      screen.getByRole("tab", { name: "three.txt" }),
      new MouseEvent("auxclick", { bubbles: true, button: 1 }),
    );

    expect(useViewerStore.getState().tabs).toEqual([]);
    expect(screen.queryByRole("tablist")).not.toBeInTheDocument();
  });
});
