import { beforeEach, describe, expect, it, vi } from "vitest";
import { buildFileContextMenuItems, type ContextMenuTarget } from "./fileActions";
import { useWorkspaceStore } from "../stores/useWorkspaceStore";

vi.mock("../services", () => ({ isTauri: true }));

const target: ContextMenuTarget = { kind: "file", path: "/library/paper.md", open: () => {} };

function menuIds(): string[] {
  return buildFileContextMenuItems({
    target,
    api: { writeClipboard: vi.fn(), openPath: vi.fn(), revealPath: vi.fn() } as never,
    capabilities: { canOpenInFileManager: true },
    onToast: () => {},
    onRenameRequest: () => {},
    availableRoots: ["/other-library"],
    deletionKind: "trash",
    onDeleteRequest: async () => {},
  }).map((item) => item.id);
}

describe("buildFileContextMenuItems", () => {
  beforeEach(() => {
    useWorkspaceStore.setState({
      workspaces: [{ id: "workspace-a", name: "Default" }],
      activeWorkspaceId: "workspace-a",
    });
  });

  it("offers the entries that change a document in a writable workspace", () => {
    const ids = menuIds();
    expect(ids).toContain("rename");
    expect(ids).toContain("move-to");
    expect(ids).toContain("delete");
  });

  it("withholds them in a read-only workspace but keeps the reads", () => {
    useWorkspaceStore.setState({
      workspaces: [{
        id: "corpus",
        name: "Underdog semantic corpus",
        read_only: true,
        managed_by: "underdog",
      }],
      activeWorkspaceId: "corpus",
    });

    const ids = menuIds();
    expect(ids).not.toContain("rename");
    expect(ids).not.toContain("move-to");
    expect(ids).not.toContain("delete");
    // The point of listing the workspace at all: its documents stay reachable.
    expect(ids).toContain("open");
    expect(ids).toContain("copy-path");
    expect(ids).toContain("open-in-file-manager");
  });
});
