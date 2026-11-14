import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import { useEditorStore } from "../stores/useEditorStore";
import { activeViewerTab, useViewerStore, type ViewerTab } from "../stores/useViewerStore";
import { ToastProvider } from "./Toast";
import ViewerTabs from "./ViewerTabs";

function renderTabs() {
  return render(
    <ToastProvider>
      <ViewerTabs paneId="pane" />
    </ToastProvider>,
  );
}

function tab(id: string, path: string): ViewerTab {
  const match = {
    path,
    origin: { TextFile: { line: 0, col: 0 } },
  } as const;
  return {
    id,
    path,
    paneId: "pane",
    match,
    history: [match],
    historyIndex: 0,
    previewData: null,
    previewLoading: false,
    previewError: null,
    pdfLoadAttempt: 0,
    metadata: null,
    metadataStatus: "idle",
    requestId: 1,
  };
}

describe("ViewerTabs", () => {
  beforeEach(() => {
    useEditorStore.setState({ buffers: {}, activeEditorPath: null });
    useViewerStore.setState({
      tabs: [
        tab("one", "/docs/one.txt"),
        tab("two", "/other/two.txt"),
        tab("three", "/docs/three.txt"),
      ],
      panes: [{ id: "pane", activeTabId: "one" }],
      focusedPaneId: "pane",
    });
  });

  it("renders an accessible tablist and activates a clicked document", () => {
    renderTabs();

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

    expect((activeViewerTab(useViewerStore.getState())?.id ?? null)).toBe("two");
    expect(screen.getByRole("tab", { name: "two.txt" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  it("supports arrow, Home, and End keyboard navigation", () => {
    renderTabs();
    const first = screen.getByRole("tab", { name: "one.txt" });

    fireEvent.keyDown(first, { key: "ArrowRight" });
    expect((activeViewerTab(useViewerStore.getState())?.id ?? null)).toBe("two");

    fireEvent.keyDown(screen.getByRole("tab", { name: "two.txt" }), { key: "End" });
    expect((activeViewerTab(useViewerStore.getState())?.id ?? null)).toBe("three");

    fireEvent.keyDown(screen.getByRole("tab", { name: "three.txt" }), { key: "Home" });
    expect((activeViewerTab(useViewerStore.getState())?.id ?? null)).toBe("one");
  });

  it("closes tabs from the close button, Delete key, and middle click", () => {
    renderTabs();

    fireEvent.click(screen.getByRole("button", { name: "Close two.txt" }));
    fireEvent.keyDown(screen.getByRole("tab", { name: "one.txt" }), { key: "Delete" });
    fireEvent(
      screen.getByRole("tab", { name: "three.txt" }),
      new MouseEvent("auxclick", { bubbles: true, button: 1 }),
    );

    expect(useViewerStore.getState().tabs).toEqual([]);
    expect(screen.queryByRole("tablist")).not.toBeInTheDocument();
  });

  it("offers the shared file context menu on a tab", () => {
    renderTabs();

    fireEvent.contextMenu(screen.getByRole("tab", { name: "two.txt" }));

    expect(screen.getByRole("menuitem", { name: "Open" })).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "Copy path" })).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "Rename" })).toBeInTheDocument();
  });

  it("activates the tab from the context menu Open item", () => {
    renderTabs();

    fireEvent.contextMenu(screen.getByRole("tab", { name: "three.txt" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Open" }));

    expect((activeViewerTab(useViewerStore.getState())?.id ?? null)).toBe("three");
  });

  it("closes the right-clicked tab from the context menu", () => {
    renderTabs();

    fireEvent.contextMenu(screen.getByRole("tab", { name: "two.txt" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Close" }));

    expect(useViewerStore.getState().tabs.map((tab) => tab.id)).toEqual(["one", "three"]);
    expect((activeViewerTab(useViewerStore.getState())?.id ?? null)).toBe("one");
  });

  it("closes every tab from the context menu", () => {
    renderTabs();

    fireEvent.contextMenu(screen.getByRole("tab", { name: "two.txt" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Close All" }));

    expect(useViewerStore.getState().tabs).toEqual([]);
    expect((activeViewerTab(useViewerStore.getState())?.id ?? null)).toBeNull();
  });

  it("pins another viewer tab into the active editor completion scope", () => {
    const editor = useEditorStore.getState();
    editor.ensureBuffer("/docs/one.txt", "Draft");
    editor.setActiveEditor("/docs/one.txt");
    renderTabs();

    fireEvent.contextMenu(screen.getByRole("tab", { name: "two.txt" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Pin to completion context" }));

    expect(useEditorStore.getState().buffers["/docs/one.txt"].scope).toEqual({
      mode: "prefer",
      pinned: ["/other/two.txt"],
      excluded: [],
    });
  });

  describe("with several panes", () => {
    beforeEach(() => {
      useViewerStore.setState({
        tabs: [
          { ...tab("one", "/docs/one.txt"), paneId: "left" },
          { ...tab("two", "/other/two.txt"), paneId: "middle" },
          { ...tab("four", "/docs/four.txt"), paneId: "middle" },
          { ...tab("three", "/docs/three.txt"), paneId: "right" },
        ],
        panes: [
          { id: "left", activeTabId: "one" },
          { id: "middle", activeTabId: "two" },
          { id: "right", activeTabId: "three" },
        ],
        focusedPaneId: "middle",
      });
    });

    const renderPane = (paneId: string) =>
      render(
        <ToastProvider>
          <ViewerTabs paneId={paneId} />
        </ToastProvider>,
      );

    it("shows only its own pane's tabs, says which pane it is, and marks the focused one", () => {
      renderPane("middle");

      expect(screen.getByRole("tablist", { name: "Open documents, pane 2 of 3" })).toBeInTheDocument();
      expect(screen.getAllByRole("tab").map((element) => element.textContent)).toEqual(["two.txt", "four.txt"]);
      expect(screen.getByRole("tab", { name: "two.txt" })).toHaveAttribute("aria-controls", "viewer-tabpanel-middle");
      expect(screen.getByRole("tab", { name: "two.txt" }).closest(".group")).toHaveClass("border-t-[var(--accent-blue)]");
    });

    it("does not mark the active tab of an unfocused pane as focused", () => {
      renderPane("left");

      expect(screen.getByRole("tab", { name: "one.txt" }).closest(".group")).toHaveClass("border-t-[var(--border-strong)]");
    });

    it("splits a tab off to the right of its pane", () => {
      renderPane("middle");

      fireEvent.contextMenu(screen.getByRole("tab", { name: "four.txt" }));
      fireEvent.click(screen.getByRole("menuitem", { name: "Split Right" }));

      const state = useViewerStore.getState();
      expect(state.panes.map((pane) => pane.id)).toHaveLength(4);
      expect(state.panes[2].activeTabId).toBe("four");
      expect(state.focusedPaneId).toBe(state.panes[2].id);
    });

    it("moves a tab into the pane to either side", () => {
      const { unmount } = renderPane("middle");

      fireEvent.contextMenu(screen.getByRole("tab", { name: "four.txt" }));
      fireEvent.click(screen.getByRole("menuitem", { name: "Move to Left Pane" }));
      expect(useViewerStore.getState().tabs.find((candidate) => candidate.id === "four")?.paneId).toBe("left");
      unmount();

      renderPane("middle");
      fireEvent.contextMenu(screen.getByRole("tab", { name: "two.txt" }));
      fireEvent.click(screen.getByRole("menuitem", { name: "Move to Right Pane" }));
      const state = useViewerStore.getState();
      expect(state.tabs.find((candidate) => candidate.id === "two")?.paneId).toBe("right");
      // The middle pane had nothing left, so it is gone.
      expect(state.panes.map((pane) => pane.id)).toEqual(["left", "right"]);
    });

    it("offers no move past the outermost panes, and no split of a pane's only tab", () => {
      renderPane("left");

      fireEvent.contextMenu(screen.getByRole("tab", { name: "one.txt" }));

      expect(screen.queryByRole("menuitem", { name: "Move to Left Pane" })).not.toBeInTheDocument();
      expect(screen.getByRole("menuitem", { name: "Move to Right Pane" })).toBeInTheDocument();
      expect(screen.queryByRole("menuitem", { name: "Split Right" })).not.toBeInTheDocument();
    });

    it("joins every pane, or closes only its own", () => {
      const { unmount } = renderPane("right");

      fireEvent.contextMenu(screen.getByRole("tab", { name: "three.txt" }));
      fireEvent.click(screen.getByRole("menuitem", { name: "Close All in Pane" }));
      expect(useViewerStore.getState().tabs.map((candidate) => candidate.id)).toEqual(["one", "two", "four"]);
      expect(useViewerStore.getState().panes.map((pane) => pane.id)).toEqual(["left", "middle"]);
      unmount();

      renderPane("left");
      fireEvent.contextMenu(screen.getByRole("tab", { name: "one.txt" }));
      fireEvent.click(screen.getByRole("menuitem", { name: "Join All Panes" }));
      expect(useViewerStore.getState().panes).toHaveLength(1);
      expect(useViewerStore.getState().tabs.every((candidate) => candidate.paneId === "left")).toBe(true);
    });
  });
});
