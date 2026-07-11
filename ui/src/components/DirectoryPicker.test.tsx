import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import DirectoryPicker from "./DirectoryPicker";
import { ToastProvider } from "./Toast";

const { mockOpenPath, mockIsTauri } = vi.hoisted(() => ({
  mockOpenPath: vi.fn(),
  mockIsTauri: { value: false },
}));

vi.mock("../services", () => ({
  api: {
    openPath: mockOpenPath,
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

  it("shows the desktop file-manager action for directory chips", () => {
    mockIsTauri.value = true;
    renderWithToasts();

    fireEvent.contextMenu(screen.getByText("project"));
    fireEvent.click(screen.getByRole("menuitem", { name: "Open in file manager" }));

    expect(mockOpenPath).toHaveBeenCalledWith("/home/user/project");
  });
});
