import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import DirectoryPicker, { describeCoverage, rootOrderAfterMove } from "./DirectoryPicker";
import { capturePointer, pointerEvent } from "../test/pointerDrag";
import { useFileDragStore } from "../stores/useFileDragStore";
import { useSemanticStore } from "../stores/useSemanticStore";
import { useTidyStore } from "../stores/useTidyStore";
import { ToastProvider } from "./Toast";

const { mockOpenPath, mockIsTauri, mockCreateDirectory, mockListDirectories, mockRenameFile } =
  vi.hoisted(() => ({
    mockOpenPath: vi.fn(),
    mockIsTauri: { value: false },
    mockCreateDirectory: vi.fn(),
    mockListDirectories: vi.fn(),
    mockRenameFile: vi.fn(),
  }));

vi.mock("../services", () => ({
  api: {
    openPath: mockOpenPath,
    renameFile: mockRenameFile,
  },
  source: {
    createDirectory: mockCreateDirectory,
    listDirectories: mockListDirectories,
  },
  get isTauri() {
    return mockIsTauri.value;
  },
}));

vi.mock("../lib/utils/dialog", () => ({
  confirmDialog: vi.fn().mockResolvedValue(true),
}));

beforeEach(() => {
  useFileDragStore.setState({ path: null, target: null });
});

describe("DirectoryPicker", () => {
  const defaultProps = {
    directory: "/home/user/project",
    favorites: ["/home/user/other"],
    recentDirs: ["/home/user/recent"],
    onChange: vi.fn(),
    onPickDirectory: vi.fn(),
    onFavoriteAdd: vi.fn(),
    onFavoriteRemove: vi.fn(),
  };

  beforeEach(() => {
    vi.clearAllMocks();
    mockIsTauri.value = false;
    mockListDirectories.mockResolvedValue([]);
    useSemanticStore.setState({ coverage: {}, refreshCoverage: vi.fn().mockResolvedValue(undefined) });
  });

  const renderWithToasts = (props = defaultProps) =>
    render(
      <ToastProvider>
        <DirectoryPicker {...props} />
      </ToastProvider>,
    );

  it("renders with folders list", () => {
    renderWithToasts();
    expect(screen.getByText("Open")).toBeInTheDocument();
    expect(screen.getByText("other")).toBeInTheDocument();
    expect(screen.getByText("recent")).toBeInTheDocument();
    expect(screen.getByText("project")).toBeInTheDocument();
  });

  it("uses hidden-scrollbar carousel controls when roots overflow", () => {
    renderWithToasts();
    const roots = screen.getByRole("region", { name: "Workspace roots" });
    const scrollBy = vi.fn();
    Object.defineProperties(roots, {
      clientWidth: { configurable: true, value: 300 },
      scrollWidth: { configurable: true, value: 900 },
      scrollLeft: { configurable: true, value: 0, writable: true },
      scrollBy: { configurable: true, value: scrollBy },
    });

    fireEvent.scroll(roots);

    expect(roots).toHaveClass("folder-strip-carousel");
    expect(screen.queryByRole("button", { name: "Scroll roots left" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Scroll roots right" }));
    expect(scrollBy).toHaveBeenCalledWith({ left: 240, behavior: "smooth" });

    roots.scrollLeft = 600;
    fireEvent.scroll(roots);
    expect(screen.getByRole("button", { name: "Scroll roots left" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Scroll roots right" })).not.toBeInTheDocument();
  });

  /** The arrows cover the very edges a drag has to rest on to scroll the strip,
   *  and a drag holds the pointer, so they cannot be pressed anyway. */
  it("takes the carousel arrows out of the hit test while a file is being dragged", () => {
    const { rerender } = renderWithToasts();
    const roots = screen.getByRole("region", { name: "Workspace roots" });
    Object.defineProperties(roots, {
      clientWidth: { configurable: true, value: 300 },
      scrollWidth: { configurable: true, value: 900 },
      scrollLeft: { configurable: true, value: 600, writable: true },
    });
    fireEvent.scroll(roots);
    const arrow = () => screen.getByRole("button", { name: "Scroll roots left" });
    expect(arrow().className).not.toContain("pointer-events-none");

    useFileDragStore.setState({ path: "/home/user/project/paper.pdf" });
    rerender(
      <ToastProvider>
        <DirectoryPicker {...defaultProps} />
      </ToastProvider>,
    );
    expect(arrow().className).toContain("pointer-events-none");
  });

  it("prevents directory tab text from being selected", () => {
    renderWithToasts();
    expect(screen.getByRole("button", { name: "/home/user/other" })).toHaveClass("select-none");
  });

  it("calls onChange when a directory is clicked", () => {
    renderWithToasts();
    const otherDir = screen.getByText("other");
    fireEvent.click(otherDir);
    expect(defaultProps.onChange).toHaveBeenCalledWith("/home/user/other");
  });

  // Queried by its visible text rather than its accessible name: the Tooltip
  // wrapper sets `aria-label` from its own content, so this button announces
  // as the current directory path and not as "Open".
  it("calls onPickDirectory when Open is clicked", () => {
    renderWithToasts();
    fireEvent.click(screen.getByText("Open"));
    expect(defaultProps.onPickDirectory).toHaveBeenCalled();
  });

  it("calls onFavoriteAdd/Remove when favorite button is clicked", () => {
    renderWithToasts();

    // "other" is already favorited
    const otherFavoriteBtn = screen.getByRole("button", { name: "Remove favorite" });
    fireEvent.click(otherFavoriteBtn);
    expect(defaultProps.onFavoriteRemove).toHaveBeenCalledWith("/home/user/other");

    // "recent" is not favorited
    const favoriteBtns = screen.getAllByRole("button", { name: "Favorite this directory" });
    fireEvent.click(favoriteBtns[0]); // Click the first one
    expect(defaultProps.onFavoriteAdd).toHaveBeenCalledWith("/home/user/recent");
  });

  it("calls onForgetDirectory when remove from history button is clicked", async () => {
    const onForgetDirectory = vi.fn();
    const { confirmDialog } = await import("../lib/utils/dialog");

    renderWithToasts({ ...defaultProps, onForgetDirectory });

    const removeBtns = screen.getAllByRole("button", { name: "Remove from history" });
    expect(removeBtns).toHaveLength(3); // one for each directory

    fireEvent.click(removeBtns[1]); // Click the second one (recent)
    await Promise.resolve();

    expect(confirmDialog).toHaveBeenCalledWith('Remove "~/recent" from your history?');
    expect(onForgetDirectory).toHaveBeenCalledWith("/home/user/recent");
  });

  it("opens a directory context menu and reuses Open", () => {
    renderWithToasts();

    fireEvent.contextMenu(screen.getByText("other"));
    expect(screen.getByRole("menuitem", { name: "Open" })).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "Copy path" })).toBeInTheDocument();
    expect(screen.queryByRole("menuitem", { name: "Open in file manager" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("menuitem", { name: "Open" }));
    expect(defaultProps.onChange).toHaveBeenCalledWith("/home/user/other");
  });

  it("creates a folder as a sibling of the top-level roots by default", async () => {
    // The three roots share the parent /home/user, surfaced as a "[user]" node
    // and the default destination — creating there is a sibling of the roots.
    mockCreateDirectory.mockResolvedValue("/home/user/Reference");
    renderWithToasts();

    fireEvent.click(screen.getByRole("button", { name: "New folder" }));
    // The parent's children are the roots themselves — its arbitrary, non-Wilkes
    // contents are never listed.
    expect(await screen.findByText("[user]")).toBeInTheDocument();
    expect(mockListDirectories).not.toHaveBeenCalledWith("/home/user");

    fireEvent.change(screen.getByLabelText("Folder name"), {
      target: { value: "Reference" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    await screen.findByText('Created folder "Reference"');
    expect(mockCreateDirectory).toHaveBeenCalledWith("/home/user", "Reference");
    expect(defaultProps.onChange).toHaveBeenCalledWith("/home/user/Reference");
  });

  it("creates a folder within a chosen root", async () => {
    mockCreateDirectory.mockResolvedValue("/home/user/project/Reference");
    renderWithToasts();

    fireEvent.click(screen.getByRole("button", { name: "New folder" }));

    // The parent auto-expands, revealing the roots as selectable destinations.
    const project = await screen.findByRole("button", { name: /^project$/i });
    fireEvent.click(project);
    fireEvent.change(screen.getByLabelText("Folder name"), {
      target: { value: "Reference" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    await screen.findByText('Created folder "Reference"');
    expect(mockCreateDirectory).toHaveBeenCalledWith("/home/user/project", "Reference");
  });

  it("renames a folder from its context menu and remaps stored paths", async () => {
    mockIsTauri.value = true;
    mockRenameFile.mockResolvedValue("/home/user/renamed");
    const onRenameDirectory = vi.fn();
    renderWithToasts({ ...defaultProps, onRenameDirectory });

    fireEvent.contextMenu(screen.getByText("other"));
    fireEvent.click(screen.getByRole("menuitem", { name: "Rename" }));

    const input = screen.getByLabelText("New folder name");
    fireEvent.change(input, { target: { value: "renamed" } });
    fireEvent.click(screen.getByRole("button", { name: "Rename" }));

    await screen.findByText('Renamed folder to "renamed"');
    expect(mockRenameFile).toHaveBeenCalledWith("/home/user/other", "renamed");
    expect(onRenameDirectory).toHaveBeenCalledWith("/home/user/other", "/home/user/renamed");
  });

  it("does not offer Rename without a rename handler", () => {
    mockIsTauri.value = true;
    renderWithToasts({ ...defaultProps, onRenameDirectory: undefined });

    fireEvent.contextMenu(screen.getByText("other"));
    expect(screen.queryByRole("menuitem", { name: "Rename" })).not.toBeInTheDocument();
  });

  it("opens the tidy-up of a root from its context menu", () => {
    mockIsTauri.value = true;
    useTidyStore.setState({ folder: null });
    renderWithToasts();

    fireEvent.contextMenu(screen.getByText("other"));
    fireEvent.click(screen.getByRole("menuitem", { name: "Tidy up…" }));

    expect(useTidyStore.getState().folder).toBe("/home/user/other");
  });

  it("offers no tidy-up outside the desktop app, which alone can move files", () => {
    renderWithToasts();
    fireEvent.contextMenu(screen.getByText("other"));
    expect(screen.queryByRole("menuitem", { name: "Tidy up…" })).not.toBeInTheDocument();
  });

  it("does not show the new-folder button when there are no roots", () => {
    renderWithToasts({ ...defaultProps, directory: "", favorites: [], recentDirs: [] });
    expect(screen.queryByRole("button", { name: "New folder" })).not.toBeInTheDocument();
  });

  it("shows the desktop file-manager action for directory chips", () => {
    mockIsTauri.value = true;
    renderWithToasts();

    fireEvent.contextMenu(screen.getByText("project"));
    fireEvent.click(screen.getByRole("menuitem", { name: "Open in file manager" }));

    expect(mockOpenPath).toHaveBeenCalledWith("/home/user/project");
  });
  /** The sidebar's tree owns the drag and says where it points; every root the
   *  strip lists offers itself as a destination and shows when it is the one. */
  it("offers every root it lists to a file being dragged out of the sidebar", () => {
    renderWithToasts();
    const roots = Array.from(document.querySelectorAll("[data-file-drop-root-path]"));
    expect(roots.map((root) => root.getAttribute("data-file-drop-root-path"))).toEqual([
      "/home/user/other",
      "/home/user/recent",
      "/home/user/project",
    ]);
    expect(document.querySelectorAll("[data-file-drop-target]")).toHaveLength(0);
  });

  it("marks the root a drag is over, and says when the file is already in it", () => {
    const { rerender } = renderWithToasts();
    const chip = () => document.querySelector('[data-file-drop-root-path="/home/user/other"]')!;

    useFileDragStore.setState({ path: "/home/user/project/paper.pdf", target: "/home/user/other" });
    rerender(
      <ToastProvider>
        <DirectoryPicker {...defaultProps} />
      </ToastProvider>,
    );
    expect(chip().getAttribute("data-file-drop-target")).toBe("true");
    expect(chip().className).toContain("ring-[var(--accent-blue)]");

    useFileDragStore.setState({ path: "/home/user/other/paper.pdf" });
    rerender(
      <ToastProvider>
        <DirectoryPicker {...defaultProps} />
      </ToastProvider>,
    );
    expect(chip().getAttribute("data-file-drop-target")).toBe("true");
    expect(chip().className).not.toContain("ring-[var(--accent-blue)]");
  });


  describe("reordering roots", () => {
    // Favorites first, then the recent roots, then the active one.
    const props = {
      ...defaultProps,
      directory: "/home/user/r3",
      favorites: ["/home/user/f1", "/home/user/f2"],
      recentDirs: ["/home/user/r1", "/home/user/r2"],
      onReorderRoots: vi.fn(),
    };
    const strip = ["/home/user/f1", "/home/user/f2", "/home/user/r1", "/home/user/r2", "/home/user/r3"];

    /** Lays the roots out 100px apart, since jsdom draws nothing. */
    const layOut = () => {
      document.querySelectorAll<HTMLElement>("[data-file-drop-root-path]").forEach((chip, index) => {
        vi.spyOn(chip, "getBoundingClientRect").mockReturnValue({
          left: index * 100, right: index * 100 + 90, width: 90,
          top: 0, bottom: 24, height: 24, x: index * 100, y: 0, toJSON: () => ({}),
        });
        capturePointer(chip);
      });
    };
    const chip = (name: string) => screen.getByText(name).closest<HTMLElement>("[data-file-drop-root-path]")!;
    const drag = (name: string, x: number) => {
      const source = screen.getByText(name);
      const start = chip(name).getBoundingClientRect().left + 20;
      pointerEvent(source, "pointerdown", { clientX: start });
      pointerEvent(window, "pointermove", { clientX: x });
    };

    it("moves a root within its group to where it is dropped", () => {
      renderWithToasts(props);
      layOut();
      drag("r3", 210);
      expect(chip("r1").dataset.rootInsert).toBe("before");
      expect(chip("r3").dataset.rootDragging).toBe("true");
      pointerEvent(window, "pointerup", { clientX: 210, buttons: 0 });
      expect(props.onReorderRoots).toHaveBeenCalledExactlyOnceWith([
        "/home/user/f1", "/home/user/f2", "/home/user/r3", "/home/user/r1", "/home/user/r2",
      ]);
      expect(document.querySelector("[data-root-insert]")).toBeNull();
    });

    it("keeps a root among its own kind when dragged past the other group", () => {
      renderWithToasts(props);
      layOut();
      drag("f1", 480);
      expect(chip("f2").dataset.rootInsert).toBe("after");
      pointerEvent(window, "pointerup", { clientX: 480, buttons: 0 });
      expect(props.onReorderRoots).toHaveBeenCalledExactlyOnceWith([
        "/home/user/f2", "/home/user/f1", "/home/user/r1", "/home/user/r2", "/home/user/r3",
      ]);
    });

    it("does not open the root a drag ended on", () => {
      renderWithToasts(props);
      layOut();
      drag("r3", 210);
      pointerEvent(window, "pointerup", { clientX: 210, buttons: 0 });
      fireEvent.click(screen.getByText("r3"), { detail: 1 });
      expect(props.onChange).not.toHaveBeenCalled();
      fireEvent.click(screen.getByText("r3"), { detail: 1 });
      expect(props.onChange).toHaveBeenCalledWith("/home/user/r3");
    });

    it("moves nothing when dropped where it started, or cancelled with Escape", () => {
      renderWithToasts(props);
      layOut();
      drag("r1", 230);
      expect(document.querySelector("[data-root-insert]")).toBeNull();
      pointerEvent(window, "pointerup", { clientX: 230, buttons: 0 });
      drag("r3", 210);
      fireEvent.keyDown(window, { key: "Escape" });
      pointerEvent(window, "pointerup", { clientX: 210, buttons: 0 });
      expect(props.onReorderRoots).not.toHaveBeenCalled();
    });

    it("does not start a drag from a root's own buttons", () => {
      renderWithToasts(props);
      layOut();
      const favorite = chip("r3").querySelectorAll("button");
      pointerEvent(favorite[favorite.length - 1], "pointerdown", { clientX: 480 });
      pointerEvent(window, "pointermove", { clientX: 210 });
      pointerEvent(window, "pointerup", { clientX: 210, buttons: 0 });
      expect(props.onReorderRoots).not.toHaveBeenCalled();
    });

    it("abandons a drag when the strip changes under it", () => {
      const view = renderWithToasts(props);
      layOut();
      drag("r3", 210);
      view.rerender(
        <ToastProvider>
          <DirectoryPicker {...props} recentDirs={["/home/user/r1", "/home/user/r2", "/home/user/r4"]} />
        </ToastProvider>,
      );
      pointerEvent(window, "pointerup", { clientX: 210, buttons: 0 });
      expect(props.onReorderRoots).not.toHaveBeenCalled();
    });

    it("moves the focused root one place with Alt and an arrow key", () => {
      const raf = vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
        callback(0);
        return 1;
      });
      renderWithToasts(props);
      fireEvent.keyDown(screen.getByText("r2"), { key: "ArrowLeft", altKey: true });
      expect(props.onReorderRoots).toHaveBeenLastCalledWith([
        "/home/user/f1", "/home/user/f2", "/home/user/r2", "/home/user/r1", "/home/user/r3",
      ]);
      fireEvent.keyDown(screen.getByText("r2"), { key: "ArrowRight", altKey: true });
      expect(props.onReorderRoots).toHaveBeenLastCalledWith([
        "/home/user/f1", "/home/user/f2", "/home/user/r1", "/home/user/r3", "/home/user/r2",
      ]);
      // The last favorite does not cross into the other roots.
      props.onReorderRoots.mockClear();
      fireEvent.keyDown(screen.getByText("f2"), { key: "ArrowRight", altKey: true });
      fireEvent.keyDown(screen.getByText("r2"), { key: "ArrowLeft" });
      expect(props.onReorderRoots).not.toHaveBeenCalled();
      raf.mockRestore();
    });

    it("offers no reordering in a workspace another application manages", () => {
      renderWithToasts({ ...props, onReorderRoots: undefined } as typeof props);
      layOut();
      drag("r3", 210);
      expect(document.querySelector("[data-root-dragging]")).toBeNull();
    });

    it("computes the order a move within a group gives", () => {
      expect(rootOrderAfterMove(strip, props.favorites, "/home/user/r1", 3)).toEqual([
        "/home/user/f1", "/home/user/f2", "/home/user/r2", "/home/user/r3", "/home/user/r1",
      ]);
      expect(rootOrderAfterMove(strip, props.favorites, "/home/user/r1", 0)).toBeNull();
      expect(rootOrderAfterMove(strip, props.favorites, "/home/user/r1", 1)).toBeNull();
      expect(() => rootOrderAfterMove(strip, props.favorites, "/elsewhere", 0)).toThrow();
    });
  });
});

describe("DirectoryPicker index coverage", () => {
  const props = {
    directory: "/home/user/project",
    favorites: ["/home/user/other"],
    recentDirs: [],
    onChange: vi.fn(),
    onPickDirectory: vi.fn(),
  };

  const renderWithToasts = () =>
    render(
      <ToastProvider>
        <DirectoryPicker {...props} />
      </ToastProvider>,
    );

  it("asks about exactly the roots it lists", () => {
    const refreshCoverage = vi.fn().mockResolvedValue(undefined);
    useSemanticStore.setState({ coverage: {}, refreshCoverage });
    renderWithToasts();
    expect(refreshCoverage).toHaveBeenCalledWith([
      "/home/user/other",
      "/home/user/project",
    ]);
  });

  it("marks a fully indexed root apart from one the index is behind on", () => {
    useSemanticStore.setState({
      coverage: {
        "/home/user/other": {
          root: "/home/user/other",
          indexable: 12,
          covered: 12,
          complete: true,
        },
        "/home/user/project": {
          root: "/home/user/project",
          indexable: 40,
          covered: 9,
          complete: false,
        },
      },
      refreshCoverage: vi.fn().mockResolvedValue(undefined),
    });
    renderWithToasts();

    const marks = document.querySelectorAll("[data-root-coverage]");
    expect(Array.from(marks).map((m) => m.getAttribute("data-root-coverage"))).toEqual([
      "complete",
      "incomplete",
    ]);
    expect(
      screen.getByLabelText(
        "Not fully indexed — 9 of 40 documents are in the semantic index",
      ),
    ).toBeInTheDocument();
  });

  it("leaves a root unmarked while its coverage is unknown", () => {
    useSemanticStore.setState({
      coverage: {},
      refreshCoverage: vi.fn().mockResolvedValue(undefined),
    });
    renderWithToasts();
    expect(document.querySelectorAll("[data-root-coverage]")).toHaveLength(0);
  });

  it("describes a root the index is behind on by what it is missing", () => {
    expect(
      describeCoverage({ root: "/a", indexable: 40, covered: 0, complete: false }),
    ).toContain("0 of 40");
    expect(
      describeCoverage({ root: "/a", indexable: 40, covered: 40, complete: true }),
    ).toContain("all 40");
  });

});
