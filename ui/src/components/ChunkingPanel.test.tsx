import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import ChunkingPanel from "./ChunkingPanel";

describe("ChunkingPanel", () => {
  const mockApi = {
    updateSettings: vi.fn().mockResolvedValue({}),
  } as any;

  const mockSettings = {
    semantic: {
      chunk_size: 500,
      chunk_overlap: 50,
    },
  } as any;

  const mockOnUpdate = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders correctly", () => {
    render(<ChunkingPanel api={mockApi} settings={mockSettings} onUpdate={mockOnUpdate} />);
    expect(screen.getByText("500 characters")).toBeInTheDocument();
    expect(screen.getByText("50 characters")).toBeInTheDocument();
  });

  it("updates chunk size", async () => {
    render(<ChunkingPanel api={mockApi} settings={mockSettings} onUpdate={mockOnUpdate} />);
    const slider = screen.getAllByRole("slider")[0];
    
    fireEvent.change(slider, { target: { value: "1000" } });
    
    expect(mockOnUpdate).toHaveBeenCalledWith(expect.objectContaining({
      semantic: expect.objectContaining({ chunk_size: 1000 })
    }));
    expect(mockApi.updateSettings).toHaveBeenCalledWith({
      semantic: expect.objectContaining({ chunk_size: 1000 })
    });
  });
});
