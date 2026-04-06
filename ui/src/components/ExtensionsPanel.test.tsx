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
    const removeButtons = screen.getAllByTitle("Remove");
    
    fireEvent.click(removeButtons[0]); // Remove "ts"

    expect(mockOnUpdate).toHaveBeenCalledWith({
      supported_extensions: ["js"],
    });
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
