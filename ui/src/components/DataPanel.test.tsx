import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import DataPanel from "./DataPanel";
import { useSemanticStore } from "../stores/useSemanticStore";

const mockApi = {
  getDataPaths: vi.fn(),
  getIndexStatus: vi.fn(),
  openPath: vi.fn(),
  deleteIndex: vi.fn(),
};

vi.mock("../services", () => ({
  isTauri: true,
  api: {
    getDataPaths: vi.fn(),
    getIndexStatus: vi.fn(),
    openPath: vi.fn(),
    deleteIndex: vi.fn(),
  },
}));

describe("DataPanel", () => {
  const mockPaths = {
    app_data: "/app/data",
  };

  const mockIndexStatus = {
    indexed_files: 10,
    db_size_bytes: 1024 * 1024,
    total_chunks: 100,
    model_id: "model/test",
  };

  beforeEach(() => {
    vi.clearAllMocks();
    vi.stubGlobal("isTauri", true);
    mockApi.getDataPaths.mockResolvedValue(mockPaths);
    mockApi.getIndexStatus.mockResolvedValue(mockIndexStatus);
    mockApi.openPath.mockResolvedValue(undefined);
    mockApi.deleteIndex.mockResolvedValue(undefined);
    vi.stubGlobal("confirm", vi.fn(() => true));
    useSemanticStore.setState({
      indexStatus: mockIndexStatus as any,
      readyForCurrentRoot: true,
      status: "ready",
      buildRoot: null,
      blockedRoot: null,
      error: null,
      refreshCurrentRootStatus: vi.fn().mockResolvedValue(true),
      ensureCurrentRootIndexed: vi.fn().mockResolvedValue(true),
      handleIndexUpdated: vi.fn().mockResolvedValue(undefined),
      handleCurrentRootIndexRemoved: vi.fn().mockResolvedValue(undefined),
    });
  });

  it("renders data paths and index status", async () => {
    await act(async () => {
      render(<DataPanel api={mockApi as any} isActive={true} />);
    });
    
    expect(screen.getByText("/app/data")).toBeInTheDocument();
    expect(screen.getByText("Ready (10 files)")).toBeInTheDocument();
  });

  it("calls openPath when Open in File Manager is clicked", async () => {
    await act(async () => {
      render(<DataPanel api={mockApi as any} isActive={true} />);
    });
    
    const openButtons = screen.getAllByText("Open in File Manager");
    fireEvent.click(openButtons[0]);
    
    expect(mockApi.openPath).toHaveBeenCalledWith("/app/data");
  });

  it("calls deleteIndex when Delete Database is clicked", async () => {
    await act(async () => {
      render(<DataPanel api={mockApi as any} isActive={true} />);
    });
    
    const deleteButton = screen.getByText("Delete current index");
    await act(async () => {
      fireEvent.click(deleteButton);
    });
    
    expect(window.confirm).toHaveBeenCalled();
    expect(mockApi.deleteIndex).toHaveBeenCalled();
    expect(useSemanticStore.getState().handleCurrentRootIndexRemoved).toHaveBeenCalled();
  });

  it("does not call deleteIndex if confirm is refused", async () => {
    await act(async () => {
      render(<DataPanel api={mockApi as any} isActive={true} />);
    });
    
    vi.stubGlobal("confirm", vi.fn(() => false));
    const deleteButton = screen.getByText("Delete current index");
    await act(async () => {
      fireEvent.click(deleteButton);
    });
    
    expect(mockApi.deleteIndex).not.toHaveBeenCalled();
  });

  it("renders empty state when no index status", async () => {
    useSemanticStore.setState({ indexStatus: null });
    await act(async () => {
      render(<DataPanel api={mockApi as any} isActive={true} />);
    });
    
    expect(screen.getByText(/No semantic index built yet/i)).toBeInTheDocument();
  });

  it("refreshes shared semantic status when the tab becomes active", async () => {
    const refreshCurrentRootStatus = vi.fn().mockResolvedValue(true);
    useSemanticStore.setState({ refreshCurrentRootStatus } as any);

    const { rerender } = render(<DataPanel api={mockApi as any} isActive={false} />);

    await act(async () => {
      rerender(<DataPanel api={mockApi as any} isActive={true} />);
    });

    expect(refreshCurrentRootStatus).toHaveBeenCalled();
  });
});
