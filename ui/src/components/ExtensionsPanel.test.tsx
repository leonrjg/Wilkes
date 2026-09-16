import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import ExtensionsPanel from "./ExtensionsPanel";

describe("ExtensionsPanel", () => {
  const mockSettings = {
    supported_extensions: ["ts", "js"],
  } as any;

  const mockOnUpdate = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders extensions list", () => {
    render(<ExtensionsPanel settings={mockSettings} onUpdate={mockOnUpdate} />);
    expect(screen.getByText(".ts")).toBeInTheDocument();
    expect(screen.getByText(".js")).toBeInTheDocument();
  });

  it("adds a new extension", () => {
    render(<ExtensionsPanel settings={mockSettings} onUpdate={mockOnUpdate} />);
    const input = screen.getByPlaceholderText("e.g. rs, py, txt");
    const addButton = screen.getByText("Add");

    fireEvent.change(input, { target: { value: "rs" } });
    fireEvent.click(addButton);

    expect(mockOnUpdate).toHaveBeenCalledWith({
      supported_extensions: ["js", "rs", "ts"],
    });
  });

  it("removes an extension", () => {
    render(<ExtensionsPanel settings={mockSettings} onUpdate={mockOnUpdate} />);
    const removeButtons = screen.getAllByRole("button", { name: "Remove" });
    
    fireEvent.click(removeButtons[0]); // Remove "ts"

    expect(mockOnUpdate).toHaveBeenCalledWith({
      supported_extensions: ["js"],
    });
  });

  it("shows every document format as always enabled, with nothing to remove", () => {
    render(<ExtensionsPanel settings={mockSettings} onUpdate={mockOnUpdate} />);

    for (const ext of ["pdf", "epub", "mobi", "azw3", "cbz", "fb2", "prc", "pdb", "azw", "cbt"]) {
      expect(screen.getByText(`.${ext}`)).toBeInTheDocument();
    }
    // Only the two text extensions are removable.
    expect(screen.getAllByRole("button", { name: "Remove" })).toHaveLength(2);
  });

  // A document extension a stored settings file still names decides nothing,
  // so it must not appear as an entry a user can remove.
  it("does not list a stored document extension as removable", () => {
    render(
      <ExtensionsPanel
        settings={{ supported_extensions: ["ts", "pdf", "epub"] } as any}
        onUpdate={mockOnUpdate}
      />,
    );

    expect(screen.getAllByRole("button", { name: "Remove" })).toHaveLength(1);
  });

  it("refuses to add a document extension and says why", () => {
    render(<ExtensionsPanel settings={mockSettings} onUpdate={mockOnUpdate} />);
    const input = screen.getByPlaceholderText("e.g. rs, py, txt");

    fireEvent.change(input, { target: { value: ".EPUB" } });
    fireEvent.click(screen.getByText("Add"));

    expect(mockOnUpdate).not.toHaveBeenCalled();
    expect(screen.getByRole("status")).toHaveTextContent("always read");
  });

  it("does not add duplicate extensions", () => {
    render(<ExtensionsPanel settings={mockSettings} onUpdate={mockOnUpdate} />);
    const input = screen.getByPlaceholderText("e.g. rs, py, txt");
    const addButton = screen.getByText("Add");

    fireEvent.change(input, { target: { value: "ts" } });
    fireEvent.click(addButton);

    expect(mockOnUpdate).not.toHaveBeenCalled();
  });
});
