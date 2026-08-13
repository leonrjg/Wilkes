import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ToastProvider } from "./Toast";
import WorkspacePicker from "./WorkspacePicker";
import { useWorkspaceStore } from "../stores/useWorkspaceStore";

const { renameWorkspace } = vi.hoisted(() => ({
  renameWorkspace: vi.fn(),
}));

vi.mock("../services", () => ({
  api: { renameWorkspace },
}));

describe("WorkspacePicker", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useWorkspaceStore.setState({
      workspaces: [{ id: "workspace-a", name: "Default" }],
      activeWorkspaceId: "workspace-a",
      loading: false,
      switching: false,
    });
  });

  it("renames the active workspace through an in-app dialog", async () => {
    renameWorkspace.mockResolvedValue({ id: "workspace-a", name: "Research" });

    render(<ToastProvider><WorkspacePicker /></ToastProvider>);
    fireEvent.click(screen.getByRole("button", { name: "Rename workspace" }));

    const input = screen.getByRole("textbox", { name: "Workspace name" });
    expect(input).toHaveValue("Default");
    fireEvent.change(input, { target: { value: "  Research  " } });
    fireEvent.click(screen.getByRole("button", { name: "Rename" }));

    await waitFor(() => expect(renameWorkspace).toHaveBeenCalledWith("workspace-a", "Research"));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(screen.getByRole("option", { name: "Research" })).toBeInTheDocument();
  });

  it("does not send a request when the name is unchanged", () => {
    render(<ToastProvider><WorkspacePicker /></ToastProvider>);
    fireEvent.click(screen.getByRole("button", { name: "Rename workspace" }));
    fireEvent.click(screen.getByRole("button", { name: "Rename" }));

    expect(renameWorkspace).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });
});
