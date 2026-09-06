import { beforeEach, describe, expect, it, vi } from "vitest";
import { useWorkspaceStore } from "./useWorkspaceStore";

const { deleteWorkspace } = vi.hoisted(() => ({ deleteWorkspace: vi.fn() }));

vi.mock("../services", () => ({
  api: { deleteWorkspace },
}));

function workspace(id: string, name: string) {
  return { id, name, roots: [], active_root: null, read_only: false, managed_by: null };
}

describe("useWorkspaceStore.remove", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    useWorkspaceStore.setState({
      workspaces: [workspace("a", "First"), workspace("b", "Second")],
      activeWorkspaceId: "a",
      loading: false,
      switching: false,
    });
  });

  it("deletes a workspace that is not active and forgets what it left in the browser", async () => {
    localStorage.setItem("wilkes.viewer-session.b", "{}");
    localStorage.setItem("wilkes.completion-scopes.b", "{}");
    deleteWorkspace.mockResolvedValue({
      active_workspace_id: "a",
      workspaces: [workspace("a", "First")],
    });

    await expect(useWorkspaceStore.getState().remove("b")).resolves.toBe(true);

    expect(deleteWorkspace).toHaveBeenCalledWith("b");
    expect(useWorkspaceStore.getState().workspaces.map((item) => item.id)).toEqual(["a"]);
    // A deleted workspace's session and completion scopes are keyed by its id
    // and would otherwise outlive it in this browser forever.
    expect(localStorage.getItem("wilkes.viewer-session.b")).toBeNull();
    expect(localStorage.getItem("wilkes.completion-scopes.b")).toBeNull();
  });

  it("activates another workspace before deleting the active one", async () => {
    const switchTo = vi.fn(async (id: string) => {
      useWorkspaceStore.setState({ activeWorkspaceId: id });
    });
    useWorkspaceStore.setState({ switchTo });
    deleteWorkspace.mockResolvedValue({
      active_workspace_id: "b",
      workspaces: [workspace("b", "Second")],
    });

    await expect(useWorkspaceStore.getState().remove("a")).resolves.toBe(true);

    expect(switchTo).toHaveBeenCalledWith("b");
    expect(deleteWorkspace).toHaveBeenCalledWith("a");
    expect(useWorkspaceStore.getState().activeWorkspaceId).toBe("b");
  });

  // `switchTo` returns without switching when the user declines to discard
  // unsaved editor changes. Deleting anyway would delete the workspace they
  // are still looking at.
  it("deletes nothing when the switch it needed was declined", async () => {
    useWorkspaceStore.setState({ switchTo: vi.fn(async () => {}) });

    await expect(useWorkspaceStore.getState().remove("a")).resolves.toBe(false);

    expect(deleteWorkspace).not.toHaveBeenCalled();
  });

  it("refuses the last workspace rather than emptying the registry", async () => {
    useWorkspaceStore.setState({
      workspaces: [workspace("a", "First")],
      activeWorkspaceId: "a",
    });

    await expect(useWorkspaceStore.getState().remove("a")).rejects.toThrow(
      "The last workspace cannot be deleted.",
    );
    expect(deleteWorkspace).not.toHaveBeenCalled();
  });
});
