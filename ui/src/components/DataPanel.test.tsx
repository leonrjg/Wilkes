import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import DataPanel from "./DataPanel";

describe("DataPanel", () => {
  const mockApi = {
    getDataPaths: vi.fn(),
    getIndexStatus: vi.fn(),
    openPath: vi.fn(),
    deleteIndex: vi.fn(),
  } as any;

  const mockPaths = {
    app_data: "/app/data",
    hf_cache: "/hf/cache",
  };

  const mockIndexStatus = {
    indexed_files: 10,
    db_size_bytes: 1024 * 1024,
    total_chunks: 100,
    model_id: "model/test",
  };

  beforeEach(() => {
    vi.clearAllMocks();
    mockApi.getDataPaths.mockResolvedValue(mockPaths);
    mockApi.getIndexStatus.mockResolvedValue(mockIndexStatus);
    mockApi.openPath.mockResolvedValue(undefined);
    vi.stubGlobal("confirm", vi.fn(() => true));
  });

  it("renders data paths and index status", async () => {
    await act(async () => {
      render(<DataPanel api={mockApi} />);
    });
    
    expect(screen.getByText("/app/data")).toBeInTheDocument();
    expect(screen.getByText("/hf/cache")).toBeInTheDocument();
    expect(screen.getByText("Ready (10 files)")).toBeInTheDocument();
  });

  it("calls openPath when Open in File Manager is clicked", async () => {
    await act(async () => {
      render(<DataPanel api={mockApi} />);
    });
    
    const openButtons = screen.getAllByText("Open in File Manager");
    fireEvent.click(openButtons[0]);
    
    expect(mockApi.openPath).toHaveBeenCalledWith("/app/data");
  });

  it("calls deleteIndex when Delete Database is clicked", async () => {
    await act(async () => {
      render(<DataPanel api={mockApi} />);
    });
    
    const deleteButton = screen.getByText("Delete Database");
    await act(async () => {
      fireEvent.click(deleteButton);
    });
    
    expect(window.confirm).toHaveBeenCalled();
    expect(mockApi.deleteIndex).toHaveBeenCalled();
  });

  it("does not call deleteIndex if confirm is refused", async () => {
    await act(async () => {
      render(<DataPanel api={mockApi} />);
    });
    
    vi.stubGlobal("confirm", vi.fn(() => false));
    const deleteButton = screen.getByText("Delete Database");
    await act(async () => {
      fireEvent.click(deleteButton);
    });
    
    expect(mockApi.deleteIndex).not.toHaveBeenCalled();
  });

  it("renders empty state when no index status", async () => {
    mockApi.getIndexStatus.mockResolvedValue(null);
    await act(async () => {
      render(<DataPanel api={mockApi} />);
    });
    
    expect(screen.getByText(/No semantic index built yet/i)).toBeInTheDocument();
  });

  it("handles errors when loading data paths", async () => {
    mockApi.getDataPaths.mockRejectedValue(new Error("Failed to load paths"));
    await act(async () => {
      render(<DataPanel api={mockApi} />);
    });
  });
});
