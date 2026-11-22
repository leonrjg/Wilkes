import { beforeEach, describe, expect, it, vi } from "vitest";
import { useWorkspaceStore } from "./useWorkspaceStore";

const { deleteWorkspace, openWorkspaceWindow, switchWorkspace } = vi.hoisted(() => ({
  deleteWorkspace: vi.fn(),
  openWorkspaceWindow: vi.fn(),
  switchWorkspace: vi.fn(),
}));

vi.mock("../services", () => ({
  api: { deleteWorkspace, openWorkspaceWindow, switchWorkspace },
}));

// Some tests below replace `switchTo` with a mock; the real one is put back
// for the tests that exercise it.
const realSwitchTo = useWorkspaceStore.getState().switchTo;

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
      openElsewhere: [],
      loading: false,
      switching: false,
    });
  });

  it("deletes a workspace that is not active and forgets what it left in the browser", async () => {
    localStorage.setItem("wilkes.viewer-session.b", "{}");
    localStorage.setItem("wilkes.completion-scopes.b", "{}");
    localStorage.setItem("wilkes.reader-positions.b", "{}");
    localStorage.setItem("wilkes.file-tree.collapsed.b", "[]");
    deleteWorkspace.mockResolvedValue({
      active_workspace_id: "a",
      workspaces: [workspace("a", "First")],
      open_elsewhere: [],
    });

    await expect(useWorkspaceStore.getState().remove("b")).resolves.toBe(true);

    expect(deleteWorkspace).toHaveBeenCalledWith("b");
    expect(useWorkspaceStore.getState().workspaces.map((item) => item.id)).toEqual(["a"]);
    // A deleted workspace's session, completion scopes and reader positions are keyed by its id
    // and would otherwise outlive it in this browser forever.
    expect(localStorage.getItem("wilkes.viewer-session.b")).toBeNull();
    expect(localStorage.getItem("wilkes.completion-scopes.b")).toBeNull();
    expect(localStorage.getItem("wilkes.reader-positions.b")).toBeNull();
    expect(localStorage.getItem("wilkes.file-tree.collapsed.b")).toBeNull();
  });

  it("activates another workspace before deleting the active one", async () => {
    const switchTo = vi.fn(async (id: string) => {
      useWorkspaceStore.setState({ activeWorkspaceId: id });
    });
    useWorkspaceStore.setState({ switchTo });
    deleteWorkspace.mockResolvedValue({
      active_workspace_id: "b",
      workspaces: [workspace("b", "Second")],
      open_elsewhere: [],
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

describe("workspaces shown in another window", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useWorkspaceStore.setState({
      workspaces: [workspace("a", "First"), workspace("b", "Second"), workspace("c", "Third")],
      activeWorkspaceId: "a",
      openElsewhere: ["b"],
      loading: false,
      switching: false,
      switchTo: realSwitchTo,
    });
  });

  // A workspace is shown by one window at most. Choosing one another window
  // shows brings that window forward; this window keeps its own workspace.
  it("brings the other window forward instead of switching this one", async () => {
    await useWorkspaceStore.getState().switchTo("b");

    expect(openWorkspaceWindow).toHaveBeenCalledWith("b");
    expect(switchWorkspace).not.toHaveBeenCalled();
    expect(useWorkspaceStore.getState().activeWorkspaceId).toBe("a");
  });

  it("does not move to a workspace another window shows to delete this one", async () => {
    const switchTo = vi.fn(async (id: string) => {
      useWorkspaceStore.setState({ activeWorkspaceId: id });
    });
    useWorkspaceStore.setState({ switchTo });
    deleteWorkspace.mockResolvedValue({
      active_workspace_id: "c",
      workspaces: [workspace("b", "Second"), workspace("c", "Third")],
      open_elsewhere: ["b"],
    });

    await expect(useWorkspaceStore.getState().remove("a")).resolves.toBe(true);

    expect(switchTo).toHaveBeenCalledWith("c");
  });

  it("refuses to delete when every other workspace is in another window", async () => {
    useWorkspaceStore.setState({ openElsewhere: ["b", "c"] });

    await expect(useWorkspaceStore.getState().remove("a")).rejects.toThrow(
      "Every other workspace is open in another window",
    );
    expect(deleteWorkspace).not.toHaveBeenCalled();
  });
});
