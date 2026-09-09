import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { capturePointer, pointerEvent } from "../test/pointerDrag";
import type { FileEntry } from "../lib/types";
import { createRef } from "react";
import FileTree, { buildFileTree, relativeFolderPath, type FileTreeHandle } from "./FileTree";

const file = (path: string): FileEntry => ({
  path,
  size_bytes: 1,
  file_type: "PlainText",
  extension: "txt",
});

const originalElementFromPoint = Object.getOwnPropertyDescriptor(document, "elementFromPoint");

afterEach(() => {
  if (originalElementFromPoint) {
    Object.defineProperty(document, "elementFromPoint", originalElementFromPoint);
  } else {
    Reflect.deleteProperty(document, "elementFromPoint");
  }
});

function stubElementFromPoint(element: Element): ReturnType<typeof vi.fn> {
  const implementation = vi.fn(() => element);
  Object.defineProperty(document, "elementFromPoint", {
    configurable: true,
    value: implementation,
  });
  return implementation;
}

describe("FileTree", () => {
  it("groups the recursive listing into real containing-folder paths", () => {
    const tree = buildFileTree("/library", [
      file("/library/root.txt"),
      file("/library/articles/2026/paper.txt"),
    ]);

    expect(tree.files.map((entry) => entry.path)).toEqual(["/library/root.txt"]);
    expect(tree.folders[0]).toMatchObject({
      path: "/library/articles",
      name: "articles",
    });
    expect(tree.folders[0].folders[0]).toMatchObject({
      path: "/library/articles/2026",
      name: "2026",
    });
  });

  it("skips a listing entry outside the listed root", () => {
    // A root switch re-renders with the new root while the previous root's
    // entries are still in hand; that frame must draw, not throw.
    const tree = buildFileTree("/library", [
      file("/elsewhere/paper.txt"),
      file("/library/kept.txt"),
    ], ["/elsewhere/folder"]);

    expect(tree.files.map((entry) => entry.path)).toEqual(["/library/kept.txt"]);
    expect(tree.folders).toEqual([]);
  });

  it("keeps empty physical directories as collapsible drop targets", () => {
    const tree = buildFileTree("/library", [], ["/library/empty/nested"]);
    expect(tree.folders[0]).toMatchObject({ path: "/library/empty", files: [] });
    expect(tree.folders[0].folders[0]).toMatchObject({
      path: "/library/empty/nested",
      files: [],
    });
  });

  it("does not draw a row for the root itself", () => {
    render(
      <FileTree
        root="/library"
        files={[file("/library/articles/paper.txt")]}
        movable={false}
        onMove={vi.fn()}
        renderFile={(entry) => <span>{entry.path.split("/").pop()}</span>}
      />,
    );

    expect(screen.queryByRole("button", { name: /folder library/ })).toBeNull();
    expect(screen.getByRole("button", { name: "Collapse folder articles" })).toBeInTheDocument();
  });

  function setup(movable = true) {
    const onMove = vi.fn().mockResolvedValue(undefined);
    const onClick = vi.fn();
    const props = {
      root: "/library",
      files: [
        file("/library/source/paper.txt"),
        file("/library/parent/inside.txt"),
        file("/library/other.txt"),
      ],
      directories: ["/library/parent/child", "/library/second"],
      movable,
      onMove,
      renderFile: (entry: FileEntry, drag: import("./FileTree").FileTreeDragProps) => (
        <button {...drag} onClick={onClick}>{entry.path.split("/").pop()}</button>
      ),
    };
    const handle = createRef<FileTreeHandle>();
    const view = render(<FileTree ref={handle} {...props} />);
    const source = screen.getByText("paper.txt");
    const parent = screen.getByRole("button", { name: "Collapse folder parent" });
    const child = screen.getByRole("button", { name: "Collapse folder child" });
    const second = screen.getByRole("button", { name: "Collapse folder second" });
    capturePointer(source);
    const hit = stubElementFromPoint(parent);
    const start = () => {
      pointerEvent(source, "pointerdown");
      pointerEvent(window, "pointermove", { clientX: 40, clientY: 40 });
    };
    const release = () => pointerEvent(window, "pointerup", { clientX: 40, clientY: 40, buttons: 0 });
    return { ...view, props, handle, source, parent, child, second, hit, start, release, onMove, onClick };
  }

  it("moves once to the highlighted nested folder, using coordinates despite pointer capture", () => {
    const { source, parent, child, hit, start, release, onMove } = setup();
    start();
    expect(screen.getByText("Drop here").closest("button")).toBe(parent);
    hit.mockReturnValue(child.querySelector("span")!);
    pointerEvent(source, "pointermove", { clientX: 40, clientY: 60 });
    expect(screen.getByText("Drop here").closest("button")).toBe(child);
    release();
    release();
    expect(onMove).toHaveBeenCalledExactlyOnceWith("/library/source/paper.txt", "/library/parent/child");
    expect(source.releasePointerCapture).toHaveBeenCalledWith(1);
  });

  it("cancels when release disagrees with the last highlighted folder", () => {
    const { hit, second, start, release, onMove } = setup();
    start();
    hit.mockReturnValue(second);
    release();
    expect(onMove).not.toHaveBeenCalled();
  });

  it("moves into a folder when dropped over a file within that folder", () => {
    const { hit, parent, start, release, onMove } = setup();
    hit.mockReturnValue(screen.getByText("inside.txt"));
    start();
    expect(screen.getByText("Drop here").closest("button")).toBe(parent);
    release();
    expect(onMove).toHaveBeenCalledExactlyOnceWith("/library/source/paper.txt", "/library/parent");
  });

  it("uses the enclosing folder when dropped on its subtree padding", () => {
    const { hit, parent, start, release, onMove } = setup();
    hit.mockReturnValue(parent.parentElement!);
    start();
    expect(screen.getByText("Drop here").closest("button")).toBe(parent);
    release();
    expect(onMove).toHaveBeenCalledExactlyOnceWith("/library/source/paper.txt", "/library/parent");
  });

  it.each(["outside", "root-level file", "another tree"])("does not move on %s", (where) => {
    const { hit, start, release, onMove } = setup();
    start();
    const foreign = document.createElement("button");
    foreign.dataset.fileTreeFolderPath = "/foreign";
    hit.mockReturnValue(where === "root-level file" ? screen.getByText("other.txt") : foreign);
    pointerEvent(window, "pointermove", { clientX: 50 });
    expect(screen.queryByText("Drop here")).not.toBeInTheDocument();
    release();
    expect(onMove).not.toHaveBeenCalled();
  });

  it("moves to root only on its own empty space", () => {
    const { hit, start, release, onMove } = setup();
    hit.mockReturnValue(screen.getByRole("tree"));
    start();
    expect(screen.getByRole("tree").className).toContain("ring-[var(--accent-blue)]");
    release();
    expect(onMove).toHaveBeenCalledExactlyOnceWith("/library/source/paper.txt", "/library");
  });

  it("does not move to the file's current folder", () => {
    const { hit, start, release, onMove } = setup();
    hit.mockReturnValue(screen.getByRole("button", { name: "Collapse folder source" }));
    start();
    expect(screen.getByText("Already here")).toBeInTheDocument();
    release();
    expect(onMove).not.toHaveBeenCalled();
  });

  it.each(["Escape", "pointercancel", "lostpointercapture", "blur", "buttons released"])(
    "cancels on %s without moving on a later release", (reason) => {
      const { source, start, release, onMove } = setup();
      start();
      if (reason === "Escape") fireEvent.keyDown(window, { key: "Escape" });
      else if (reason === "blur") fireEvent(window, new Event("blur"));
      else if (reason === "buttons released") pointerEvent(window, "pointermove", { buttons: 0 });
      else pointerEvent(reason === "lostpointercapture" ? source : window, reason);
      expect(screen.queryByText("Drop here")).not.toBeInTheDocument();
      release();
      expect(onMove).not.toHaveBeenCalled();
    },
  );

  it("keeps ordinary clicks and suppresses the click following a drag", () => {
    const { source, start, release, onClick, onMove } = setup();
    pointerEvent(source, "pointerdown");
    pointerEvent(window, "pointermove", { clientX: 22 });
    release();
    fireEvent.click(source, { detail: 1 });
    expect(onClick).toHaveBeenCalledTimes(1);
    expect(onMove).not.toHaveBeenCalled();
    start();
    release();
    fireEvent.click(source, { detail: 1 });
    expect(onClick).toHaveBeenCalledTimes(1);
    pointerEvent(source, "pointerdown");
    release();
    fireEvent.click(source, { detail: 1 });
    expect(onClick).toHaveBeenCalledTimes(2);
  });

  it("ignores other pointers and secondary buttons", () => {
    const { source, start, release, onMove } = setup();
    pointerEvent(source, "pointerdown", { button: 2 });
    pointerEvent(window, "pointermove", { clientX: 50 });
    release();
    expect(onMove).not.toHaveBeenCalled();
    start();
    pointerEvent(window, "pointercancel", { pointerId: 2 });
    pointerEvent(window, "pointerup", { pointerId: 2 });
    expect(onMove).not.toHaveBeenCalled();
    release();
    expect(onMove).toHaveBeenCalledTimes(1);
  });

  it("disables movement in a read-only tree", () => {
    const { start, release, onMove } = setup(false);
    start();
    release();
    expect(onMove).not.toHaveBeenCalled();
    expect(screen.queryByText("Drop here")).not.toBeInTheDocument();
  });

  it.each(["root", "permission", "unmount"])("cancels when %s changes", (change) => {
    const { start, release, onMove, rerender, unmount, props } = setup();
    start();
    if (change === "unmount") unmount();
    else rerender(<FileTree {...props} root={change === "root" ? "/new" : props.root} movable={change !== "permission"} />);
    release();
    expect(onMove).not.toHaveBeenCalled();
  });

  it("recomputes the highlight after scrolling under a stationary pointer", () => {
    let tick: FrameRequestCallback = () => {};
    const raf = vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => { tick = callback; return 1; });
    const { hit, second, start, release, onMove } = setup();
    start();
    hit.mockReturnValue(second);
    fireEvent.scroll(screen.getByRole("tree"));
    act(() => tick(performance.now() + 16));
    expect(screen.getByText("Drop here").closest("button")).toBe(second);
    release();
    expect(onMove).toHaveBeenCalledExactlyOnceWith("/library/source/paper.txt", "/library/second");
    raf.mockRestore();
  });

  it("auto-scrolls near the sidebar edge and stops on cancellation", () => {
    let tick: FrameRequestCallback = () => {};
    const raf = vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => { tick = callback; return 123; });
    const cancel = vi.spyOn(window, "cancelAnimationFrame");
    const { start, release, onMove } = setup();
    const scroller = screen.getByRole("tree").parentElement!;
    scroller.style.overflowY = "auto";
    Object.defineProperties(scroller, { scrollHeight: { value: 800 }, clientHeight: { value: 200 } });
    vi.spyOn(scroller, "getBoundingClientRect").mockReturnValue({
      left: 0, right: 300, top: 0, bottom: 200, width: 300, height: 200, x: 0, y: 0,
      toJSON: () => ({}),
    });
    start();
    pointerEvent(window, "pointermove", { clientX: 40, clientY: 190 });
    act(() => tick(performance.now() + 16));
    expect(scroller.scrollTop).toBeGreaterThan(0);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(cancel).toHaveBeenCalledWith(123);
    release();
    expect(onMove).not.toHaveBeenCalled();
    raf.mockRestore();
    cancel.mockRestore();
  });

  it("does not use a stale target when release coordinates are invalid", () => {
    const { start, onMove } = setup();
    start();
    pointerEvent(window, "pointerup", { clientX: NaN, clientY: NaN, buttons: 0 });
    expect(onMove).not.toHaveBeenCalled();
  });

  it("expands a closed hovered folder after a delay", () => {
    let tick: FrameRequestCallback = () => {};
    const raf = vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => { tick = callback; return 1; });
    const { parent, start, release } = setup();
    fireEvent.click(parent);
    expect(screen.queryByRole("button", { name: "Collapse folder child" })).not.toBeInTheDocument();
    start();
    expect(screen.queryByRole("button", { name: "Collapse folder child" })).not.toBeInTheDocument();
    act(() => tick(performance.now() + 650));
    expect(screen.getByRole("button", { name: "Collapse folder child" })).toBeInTheDocument();
    release();
    raf.mockRestore();
  });

  /** A file dragged in from the file manager is reported by position, and must
   *  land where the highlight it drew says it will — the same folder a row
   *  dragged to that point would land in. */
  it("highlights and names the folder under a drag from outside the application", () => {
    const { handle, parent, child, hit } = setup();
    const over = (x: number, y: number) => {
      let target: string | null = null;
      act(() => { target = handle.current!.externalDragOver(x, y); });
      return target;
    };

    expect(over(40, 40)).toBe("/library/parent");
    expect(screen.getByText("Drop here").closest("button")).toBe(parent);
    hit.mockReturnValue(child);
    expect(over(40, 60)).toBe("/library/parent/child");
    expect(screen.getByText("Drop here").closest("button")).toBe(child);

    act(() => handle.current!.externalDragEnd());
    expect(screen.queryByText("Drop here")).not.toBeInTheDocument();
  });

  it("offers no destination to an outside drag over a read-only tree", () => {
    const { handle } = setup(false);
    let target: string | null = "unset";
    act(() => { target = handle.current!.externalDragOver(40, 40); });
    expect(target).toBeNull();
    expect(screen.queryByText("Drop here")).not.toBeInTheDocument();
  });

  /** The roots in the strip at the top of the window are destinations for the
   *  same drag, so a file can leave the root it is in without a dialog. */
  describe("the roots in the strip", () => {
    const chip = (path: string) => {
      const element = document.createElement("div");
      element.dataset.fileDropRootPath = path;
      document.body.appendChild(element);
      return element;
    };

    afterEach(() => {
      document.querySelectorAll("[data-file-drop-root-path]").forEach((el) => el.remove());
    });

    it("moves a file to a root outside the tree it was dragged from", () => {
      const { hit, start, release, onMove } = setup();
      hit.mockReturnValue(chip("/other-library"));
      start();
      release();
      expect(onMove).toHaveBeenCalledExactlyOnceWith(
        "/library/source/paper.txt",
        "/other-library",
      );
    });

    it("does not move a file to the root it is already in", () => {
      const { hit, start, release, onMove } = setup();
      hit.mockReturnValue(chip("/library/source"));
      start();
      release();
      expect(onMove).not.toHaveBeenCalled();
    });

    /** The strip scrolls sideways, and a root can be off the side of the
     *  window. Resting at an edge scrolls it into reach, as resting at the
     *  sidebar's bottom edge scrolls a folder into reach. */
    it("scrolls the strip when the drag rests at its edge", () => {
      let tick: FrameRequestCallback = () => {};
      const raf = vi.spyOn(window, "requestAnimationFrame")
        .mockImplementation((callback) => { tick = callback; return 1; });
      const strip = document.createElement("div");
      strip.style.overflowX = "auto";
      Object.defineProperties(strip, {
        scrollWidth: { value: 900 },
        clientWidth: { value: 300 },
        scrollLeft: { value: 0, writable: true },
      });
      vi.spyOn(strip, "getBoundingClientRect").mockReturnValue({
        left: 0, right: 300, top: 0, bottom: 24, width: 300, height: 24, x: 0, y: 0,
        toJSON: () => ({}),
      });
      strip.appendChild(chip("/other-library"));
      document.body.appendChild(strip);

      const { hit, start, release } = setup();
      hit.mockReturnValue(strip.firstElementChild!);
      start();
      pointerEvent(window, "pointermove", { clientX: 295, clientY: 10 });
      act(() => tick(performance.now() + 16));
      expect(strip.scrollLeft).toBeGreaterThan(0);
      release();
      strip.remove();
      raf.mockRestore();
    });

    /** An import is admitted only into the root that is open, so a drag from
     *  outside the application is never shown a root it cannot land in. */
    it("are not offered to a drag from outside the application", () => {
      const { handle, hit } = setup();
      hit.mockReturnValue(chip("/other-library"));
      let target: string | null = "unset";
      act(() => { target = handle.current!.externalDragOver(40, 40); });
      expect(target).toBeNull();
    });
  });

  describe("the folder menu", () => {
    function setupMenu(onCreateFolder?: (directory: string, name: string) => Promise<void>) {
      const create = onCreateFolder ?? vi.fn().mockResolvedValue(undefined);
      render(
        <FileTree
          root="/library"
          files={[file("/library/parent/inside.txt"), file("/library/other.txt")]}
          directories={["/library/parent/child"]}
          movable
          onMove={vi.fn()}
          onCreateFolder={create}
          renderFile={(entry, drag) => (
            <button
              {...drag}
              onContextMenu={(event) => event.preventDefault()}
            >
              {entry.path.split("/").pop()}
            </button>
          )}
        />,
      );
      const submit = async (name: string) => {
        fireEvent.change(screen.getByRole("textbox"), { target: { value: name } });
        await act(async () => {
          fireEvent.click(screen.getByRole("button", { name: "Create" }));
        });
      };
      return { create, submit };
    }

    it("creates in the root from the space around the tree's own entries", async () => {
      const { create, submit } = setupMenu();
      fireEvent.contextMenu(screen.getByRole("tree"));
      expect(screen.getByRole("group", { name: "New folder in library" })).toBeInTheDocument();
      await submit("Reading");
      expect(create).toHaveBeenCalledWith("/library", "Reading");
    });

    it("creates within the folder whose subtree padding was clicked", async () => {
      const { create, submit } = setupMenu();
      fireEvent.contextMenu(screen.getByText("inside.txt").closest("li")!);
      expect(screen.getByRole("group", { name: "New folder in parent" })).toBeInTheDocument();
      await submit("Notes");
      expect(create).toHaveBeenCalledWith("/library/parent", "Notes");
    });

    it("creates within a folder whose own row was clicked, and opens it", async () => {
      const { create, submit } = setupMenu();
      const parent = screen.getByRole("button", { name: "Collapse folder parent" });
      fireEvent.click(parent);
      expect(screen.queryByText("inside.txt")).toBeNull();

      fireEvent.contextMenu(screen.getByRole("button", { name: "Expand folder parent" }));
      await submit("Notes");
      expect(create).toHaveBeenCalledWith("/library/parent", "Notes");
      // A folder created into a closed folder would otherwise not be seen.
      expect(screen.getByText("inside.txt")).toBeInTheDocument();
    });

    it("leaves a click a row has already answered alone", () => {
      setupMenu();
      fireEvent.contextMenu(screen.getByText("other.txt"));
      expect(screen.queryByRole("menu")).toBeNull();
    });

    it("offers nothing where folders may not be created", () => {
      render(
        <FileTree
          root="/library"
          files={[file("/library/other.txt")]}
          movable={false}
          onMove={vi.fn()}
          renderFile={(entry) => <span>{entry.path.split("/").pop()}</span>}
        />,
      );
      fireEvent.contextMenu(screen.getByRole("tree"));
      expect(screen.queryByRole("menu")).toBeNull();
    });
  });

  it("names a folder relative to the root, and refuses one outside it", () => {
    expect(relativeFolderPath("/library", "/library")).toBeNull();
    expect(relativeFolderPath("/library", "/library/parent/child")).toBe("parent/child");
    expect(() => relativeFolderPath("/library", "/elsewhere/parent")).toThrow(/not a folder under/);
  });
});
