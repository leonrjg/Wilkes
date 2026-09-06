import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import DataPanel from "./DataPanel";
import { useSemanticStore } from "../stores/useSemanticStore";
import { useWorkspaceStore } from "../stores/useWorkspaceStore";
import { confirmDialog } from "../lib/utils/dialog";

const mockApi = {
  getDataPaths: vi.fn(),
  getIndexStatus: vi.fn(),
  openPath: vi.fn(),
  deleteIndex: vi.fn(),
};

vi.mock("../lib/utils/dialog", () => ({
  confirmDialog: vi.fn(),
}));

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
    workspace: "/app/data/workspaces/w1",
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
    useWorkspaceStore.setState({ refreshList: vi.fn().mockResolvedValue(undefined) } as any);
    mockApi.getDataPaths.mockResolvedValue(mockPaths);
    mockApi.getIndexStatus.mockResolvedValue(mockIndexStatus);
    mockApi.openPath.mockResolvedValue(undefined);
    mockApi.deleteIndex.mockResolvedValue(undefined);
    vi.mocked(confirmDialog).mockResolvedValue(true);
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
    // Both paths are named, and neither stands in for the other: the index
    // lives in the workspace, the installation directory contains it.
    expect(screen.getByText("/app/data/workspaces/w1")).toBeInTheDocument();
    expect(
      screen.getByText("/app/data/workspaces/w1/semantic_index.db"),
    ).toBeInTheDocument();
    expect(screen.getByText("Ready (10 files)")).toBeInTheDocument();
  });

  it("calls openPath when Open in File Manager is clicked", async () => {
    await act(async () => {
      render(<DataPanel api={mockApi as any} isActive={true} />);
    });
    
    const openButtons = screen.getAllByText("Open in File Manager");
    fireEvent.click(openButtons[0]);
    
    expect(mockApi.openPath).toHaveBeenCalledWith("/app/data/workspaces/w1");

    fireEvent.click(screen.getByText("Open Installation Folder"));
    expect(mockApi.openPath).toHaveBeenCalledWith("/app/data");

    fireEvent.click(screen.getByText("Open Workspace Folder"));
    expect(mockApi.openPath).toHaveBeenCalledWith("/app/data/workspaces/w1");
  });

  it("calls deleteIndex when Delete Database is clicked", async () => {
    await act(async () => {
      render(<DataPanel api={mockApi as any} isActive={true} />);
    });
    
    const deleteButton = screen.getByText("Delete current index");
    await act(async () => {
      fireEvent.click(deleteButton);
    });
    
    expect(confirmDialog).toHaveBeenCalled();
    expect(mockApi.deleteIndex).toHaveBeenCalled();
    expect(useSemanticStore.getState().handleCurrentRootIndexRemoved).toHaveBeenCalled();
  });

  it("does not call deleteIndex if confirm is refused", async () => {
    await act(async () => {
      render(<DataPanel api={mockApi as any} isActive={true} />);
    });
    
    vi.mocked(confirmDialog).mockResolvedValue(false);
    const deleteButton = screen.getByText("Delete current index");
    await act(async () => {
      fireEvent.click(deleteButton);
    });
    
    expect(mockApi.deleteIndex).not.toHaveBeenCalled();
  });

  describe("workspaces", () => {
    const remove = vi.fn().mockResolvedValue(true);

    beforeEach(() => {
      remove.mockClear();
      useWorkspaceStore.setState({
        workspaces: [
          { id: "w1", name: "Thesis", roots: ["/library"], active_root: "/library", read_only: false, managed_by: null },
          {
            id: "corpus",
            name: "Underdog semantic corpus",
            roots: ["/app/data/workspaces/corpus/managed_sources"],
            active_root: "/app/data/workspaces/corpus/managed_sources",
            read_only: true,
            managed_by: "underdog",
          },
        ],
        activeWorkspaceId: "w1",
        remove,
      } as any);
    });

    it("re-reads the registry when the page becomes active", async () => {
      const refreshList = vi.fn().mockResolvedValue(undefined);
      useWorkspaceStore.setState({ refreshList } as any);

      const { rerender } = render(<DataPanel api={mockApi as any} isActive={false} />);
      await act(async () => {
        rerender(<DataPanel api={mockApi as any} isActive={true} />);
      });

      expect(refreshList).toHaveBeenCalled();
    });

    it("lists every workspace, managed ones included, and marks the active one", async () => {
      await act(async () => {
        render(<DataPanel api={mockApi as any} isActive={true} />);
      });

      expect(screen.getByText("Thesis")).toBeInTheDocument();
      expect(screen.getByText("Underdog semantic corpus")).toBeInTheDocument();
      expect(screen.getByText("Active")).toBeInTheDocument();
      expect(screen.getByText("underdog")).toBeInTheDocument();
      expect(screen.getByText("/library")).toBeInTheDocument();
    });

    // Read-only protects a managed corpus's content, not its existence: the
    // bytes are on the user's disk and reclaiming them is theirs to decide.
    it("deletes a managed workspace after confirmation", async () => {
      await act(async () => {
        render(<DataPanel api={mockApi as any} isActive={true} />);
      });

      const button = screen.getByRole("button", { name: "Delete workspace Underdog semantic corpus" });
      expect(button).toBeEnabled();
      await act(async () => {
        fireEvent.click(button);
      });

      expect(confirmDialog).toHaveBeenCalled();
      expect(remove).toHaveBeenCalledWith("corpus");
    });

    it("deletes nothing when the confirmation is refused", async () => {
      await act(async () => {
        render(<DataPanel api={mockApi as any} isActive={true} />);
      });

      vi.mocked(confirmDialog).mockResolvedValue(false);
      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Delete workspace Thesis" }));
      });

      expect(remove).not.toHaveBeenCalled();
    });

    it("withholds deletion of the last remaining workspace", async () => {
      useWorkspaceStore.setState({
        workspaces: [
          { id: "w1", name: "Thesis", roots: [], active_root: null, read_only: false, managed_by: null },
        ],
      } as any);

      await act(async () => {
        render(<DataPanel api={mockApi as any} isActive={true} />);
      });

      expect(screen.getByRole("button", { name: "Delete workspace Thesis" })).toBeDisabled();
      // And a workspace with no folder open still says so rather than showing
      // an empty line.
      expect(screen.getByText("No folder open")).toBeInTheDocument();
    });

    it("surfaces a failed deletion instead of leaving the row as it was", async () => {
      remove.mockRejectedValueOnce(new Error("Workspace is busy"));

      await act(async () => {
        render(<DataPanel api={mockApi as any} isActive={true} />);
      });
      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Delete workspace Thesis" }));
      });

      expect(screen.getByText("Workspace is busy")).toBeInTheDocument();
    });
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
