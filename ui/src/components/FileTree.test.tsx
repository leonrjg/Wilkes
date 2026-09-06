import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { FileEntry } from "../lib/types";
import FileTree, { buildFileTree, FILE_TREE_DRAG_TYPE } from "./FileTree";

const file = (path: string): FileEntry => ({
  path,
  size_bytes: 1,
  file_type: "PlainText",
  extension: "txt",
});

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

  it("starts expanded, collapses folders, and moves a dragged file into the dropped-on folder", () => {
    const onMove = vi.fn().mockResolvedValue(undefined);
    render(
      <FileTree
        root="/library"
        files={[file("/library/root.txt"), file("/library/articles/paper.txt")]}
        movable
        onMove={onMove}
        renderFile={(entry, drag) => (
          <button {...drag}>{entry.path.split("/").pop()}</button>
        )}
      />,
    );

    expect(screen.getByText("root.txt")).toBeInTheDocument();
    expect(screen.getByText("paper.txt")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Collapse folder articles" }));
    expect(screen.queryByText("paper.txt")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Expand folder articles" }));
    expect(screen.getByText("paper.txt")).toBeInTheDocument();

    const values = new Map<string, string>();
    const dataTransfer = {
      effectAllowed: "none",
      dropEffect: "none",
      setData: (type: string, value: string) => values.set(type, value),
      getData: (type: string) => values.get(type) ?? "",
    };
    fireEvent.dragStart(screen.getByText("paper.txt"), { dataTransfer });
    expect(values.get(FILE_TREE_DRAG_TYPE)).toBe("/library/articles/paper.txt");
    // The root has no row of its own, so its drop target is the tree container.
    const root = screen.getByRole("tree");
    fireEvent.dragEnter(root, { dataTransfer });
    expect(root.className).toContain("ring-[var(--accent-blue)]");
    fireEvent.drop(root, {
      dataTransfer,
    });

    expect(onMove).toHaveBeenCalledWith("/library/articles/paper.txt", "/library");
  });

  it("makes an empty folder's whole tree item a clear drop target", () => {
    const onMove = vi.fn().mockResolvedValue(undefined);
    render(
      <FileTree
        root="/library"
        files={[file("/library/paper.txt")]}
        directories={["/library/empty"]}
        movable
        onMove={onMove}
        renderFile={(entry, drag) => <button {...drag}>{entry.path.split("/").pop()}</button>}
      />,
    );

    const dataTransfer = {
      effectAllowed: "none",
      dropEffect: "none",
      setData: vi.fn(),
      getData: vi.fn(() => ""),
    };
    fireEvent.dragStart(screen.getByText("paper.txt"), { dataTransfer });
    const emptyFolder = screen
      .getByRole("button", { name: "Collapse folder empty" })
      .closest("li")!;
    fireEvent.dragEnter(emptyFolder, { dataTransfer });
    expect(screen.getByText("Drop here")).toBeInTheDocument();
    fireEvent.drop(emptyFolder, { dataTransfer });

    expect(onMove).toHaveBeenCalledWith("/library/paper.txt", "/library/empty");
  });

  it("moves to the visibly highlighted folder when the webview swallows drop", () => {
    const onMove = vi.fn().mockResolvedValue(undefined);
    render(
      <FileTree
        root="/library"
        files={[file("/library/paper.txt")]}
        directories={["/library/empty"]}
        movable
        onMove={onMove}
        renderFile={(entry, drag) => <button {...drag}>{entry.path.split("/").pop()}</button>}
      />,
    );

    const dataTransfer = {
      effectAllowed: "none",
      dropEffect: "none",
      setData: vi.fn(),
      getData: vi.fn(() => ""),
    };
    const draggedFile = screen.getByText("paper.txt");
    fireEvent.dragStart(draggedFile, { dataTransfer });
    fireEvent.dragEnter(screen.getByRole("button", { name: "Collapse folder empty" }), {
      dataTransfer,
    });
    fireEvent.dragEnd(draggedFile, { clientX: 0, clientY: 0, dataTransfer });

    expect(onMove).toHaveBeenCalledWith("/library/paper.txt", "/library/empty");
  });

  it("does not use the drag-end fallback after the user cancels with Escape", () => {
    const onMove = vi.fn().mockResolvedValue(undefined);
    render(
      <FileTree
        root="/library"
        files={[file("/library/paper.txt")]}
        directories={["/library/empty"]}
        movable
        onMove={onMove}
        renderFile={(entry, drag) => <button {...drag}>{entry.path.split("/").pop()}</button>}
      />,
    );

    const dataTransfer = {
      effectAllowed: "none",
      dropEffect: "none",
      setData: vi.fn(),
      getData: vi.fn(() => ""),
    };
    const draggedFile = screen.getByText("paper.txt");
    fireEvent.dragStart(draggedFile, { dataTransfer });
    fireEvent.dragEnter(screen.getByRole("button", { name: "Collapse folder empty" }), {
      dataTransfer,
    });
    fireEvent.keyDown(window, { key: "Escape" });
    fireEvent.dragEnd(draggedFile, { clientX: 0, clientY: 0, dataTransfer });

    expect(onMove).not.toHaveBeenCalled();
  });
});
