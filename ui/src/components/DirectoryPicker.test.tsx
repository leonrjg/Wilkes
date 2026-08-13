import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import DirectoryPicker from "./DirectoryPicker";
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
  });

  const renderWithToasts = (props = defaultProps) =>
    render(
      <ToastProvider>
        <DirectoryPicker {...props} />
      </ToastProvider>,
    );

  it("renders with folders list", () => {
    renderWithToasts();
    expect(screen.getByText("Open folder")).toBeInTheDocument();
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

  it("calls onPickDirectory when Open folder is clicked", () => {
    renderWithToasts();
    const openFolder = screen.getByText("Open folder");
    fireEvent.click(openFolder);
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
});
