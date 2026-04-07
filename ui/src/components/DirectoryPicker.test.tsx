import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import DirectoryPicker from "./DirectoryPicker";

vi.mock("../lib/utils/dialog", () => ({
  confirmDialog: vi.fn().mockResolvedValue(true),
}));

describe("DirectoryPicker", () => {
  const defaultProps = {
    directory: "/home/user/project",
    bookmarks: ["/home/user/other"],
    recentDirs: ["/home/user/recent"],
    onChange: vi.fn(),
    onPickDirectory: vi.fn(),
    onBookmarkAdd: vi.fn(),
    onBookmarkRemove: vi.fn(),
  };

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders with folders list", () => {
    render(<DirectoryPicker {...defaultProps} />);
    expect(screen.getByText("Open folder")).toBeInTheDocument();
    expect(screen.getByText("other")).toBeInTheDocument();
    expect(screen.getByText("recent")).toBeInTheDocument();
    expect(screen.getByText("project")).toBeInTheDocument();
  });

  it("calls onChange when a directory is clicked", () => {
    render(<DirectoryPicker {...defaultProps} />);
    const otherDir = screen.getByText("other");
    fireEvent.click(otherDir);
    expect(defaultProps.onChange).toHaveBeenCalledWith("/home/user/other");
  });

  it("calls onPickDirectory when Open folder is clicked", () => {
    render(<DirectoryPicker {...defaultProps} />);
    const openFolder = screen.getByText("Open folder");
    fireEvent.click(openFolder);
    expect(defaultProps.onPickDirectory).toHaveBeenCalled();
  });

  it("calls onBookmarkAdd/Remove when bookmark button is clicked", () => {
    render(<DirectoryPicker {...defaultProps} />);
    
    // "other" is already bookmarked
    const otherBookmarkBtn = screen.getByTitle("Remove bookmark");
    fireEvent.click(otherBookmarkBtn);
    expect(defaultProps.onBookmarkRemove).toHaveBeenCalledWith("/home/user/other");

    // "recent" is not bookmarked
    const bookmarkBtns = screen.getAllByTitle("Bookmark this directory");
    fireEvent.click(bookmarkBtns[0]); // Click the first one
    expect(defaultProps.onBookmarkAdd).toHaveBeenCalledWith("/home/user/recent");
  });

  it("calls onForgetDirectory when remove from history button is clicked", async () => {
    const onForgetDirectory = vi.fn();
    const { confirmDialog } = await import("../lib/utils/dialog");

    render(<DirectoryPicker {...defaultProps} onForgetDirectory={onForgetDirectory} />);

    const removeBtns = screen.getAllByTitle("Remove from history");
    expect(removeBtns).toHaveLength(3); // one for each directory

    fireEvent.click(removeBtns[1]); // Click the second one (recent)
    await Promise.resolve();

    expect(confirmDialog).toHaveBeenCalledWith('Remove "~/recent" from your history?');
    expect(onForgetDirectory).toHaveBeenCalledWith("/home/user/recent");
  });
});
