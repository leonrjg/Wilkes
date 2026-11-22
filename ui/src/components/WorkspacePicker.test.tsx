import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ToastProvider } from "./Toast";
import WorkspacePicker from "./WorkspacePicker";
import { useWorkspaceStore } from "../stores/useWorkspaceStore";

const { renameWorkspace, switchWorkspace, openWorkspaceWindow } = vi.hoisted(() => ({
  renameWorkspace: vi.fn(),
  switchWorkspace: vi.fn(),
  openWorkspaceWindow: vi.fn(),
}));

vi.mock("../services", () => ({
  api: { renameWorkspace, switchWorkspace, openWorkspaceWindow },
}));

const realSwitchTo = useWorkspaceStore.getState().switchTo;

describe("WorkspacePicker", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useWorkspaceStore.setState({
      workspaces: [{ id: "workspace-a", name: "Default" }],
      activeWorkspaceId: "workspace-a",
      openElsewhere: [],
      loading: false,
      switching: false,
      switchTo: realSwitchTo,
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
    expect(screen.getByRole("button", { name: "Workspace: Research" })).toBeInTheDocument();
  });

  it("lists a read-only workspace, marks it, and withholds rename", () => {
    useWorkspaceStore.setState({
      workspaces: [
        { id: "workspace-a", name: "Default" },
        { id: "corpus", name: "Underdog semantic corpus", read_only: true, managed_by: "underdog" },
      ],
      activeWorkspaceId: "corpus",
    });

    render(<ToastProvider><WorkspacePicker /></ToastProvider>);
    fireEvent.click(screen.getByRole("button", { name: "Workspace: Underdog semantic corpus" }));

    expect(
      screen.getByRole("menuitemradio", { name: "Underdog semantic corpus (read-only)" }),
    ).toHaveAttribute("aria-checked", "true");
    expect(screen.getByLabelText("Read-only workspace")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Rename workspace" })).toBeDisabled();
    // Creating a workspace of one's own is unaffected by the active one being
    // read-only.
    expect(screen.getByRole("button", { name: "New workspace" })).toBeEnabled();
  });

  it("does not send a request when the name is unchanged", () => {
    render(<ToastProvider><WorkspacePicker /></ToastProvider>);
    fireEvent.click(screen.getByRole("button", { name: "Rename workspace" }));
    fireEvent.click(screen.getByRole("button", { name: "Rename" }));

    expect(renameWorkspace).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  describe("one list for switching and for opening windows", () => {
    beforeEach(() => {
      useWorkspaceStore.setState({
        workspaces: [
          { id: "a", name: "Default" },
          { id: "b", name: "Books" },
          { id: "c", name: "Corpus" },
        ] as never,
        activeWorkspaceId: "a",
        openElsewhere: ["c"],
      });
    });

    it("switches this window to the workspace a row names", async () => {
      const switchTo = vi.fn().mockResolvedValue(undefined);
      useWorkspaceStore.setState({ switchTo });
      render(<ToastProvider><WorkspacePicker /></ToastProvider>);

      fireEvent.click(screen.getByRole("button", { name: "Workspace: Default" }));
      fireEvent.click(screen.getByRole("menuitemradio", { name: "Books" }));

      await waitFor(() => expect(switchTo).toHaveBeenCalledWith("b"));
    });

    it("opens a workspace no window shows in a new window from its own row", async () => {
      openWorkspaceWindow.mockResolvedValue(undefined);
      render(<ToastProvider><WorkspacePicker /></ToastProvider>);

      fireEvent.click(screen.getByRole("button", { name: "Workspace: Default" }));
      fireEvent.click(screen.getByRole("menuitem", { name: "Open Books in a new window" }));

      await waitFor(() => expect(openWorkspaceWindow).toHaveBeenCalledWith("b"));
      expect(switchWorkspace).not.toHaveBeenCalled();
    });

    // The active workspace is this window's already, and one another window
    // shows is reached by choosing its row: neither gets a second button.
    it("offers a new window only where it would be a new one", () => {
      render(<ToastProvider><WorkspacePicker /></ToastProvider>);
      fireEvent.click(screen.getByRole("button", { name: "Workspace: Default" }));

      expect(screen.queryByRole("menuitem", { name: "Open Default in a new window" })).toBeNull();
      expect(screen.queryByRole("menuitem", { name: "Open Corpus in a new window" })).toBeNull();
      expect(
        screen.getByRole("menuitemradio", { name: "Corpus (open in another window)" }),
      ).toBeInTheDocument();
    });

    it("brings forward the window already showing a workspace", async () => {
      openWorkspaceWindow.mockResolvedValue(undefined);
      render(<ToastProvider><WorkspacePicker /></ToastProvider>);

      fireEvent.click(screen.getByRole("button", { name: "Workspace: Default" }));
      fireEvent.click(screen.getByRole("menuitemradio", { name: "Corpus (open in another window)" }));

      await waitFor(() => expect(openWorkspaceWindow).toHaveBeenCalledWith("c"));
      expect(switchWorkspace).not.toHaveBeenCalled();
    });
  });
});
