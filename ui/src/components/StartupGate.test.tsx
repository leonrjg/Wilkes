import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import StartupGate from "./StartupGate";
import { api } from "../services";

vi.mock("../services", () => ({
  api: {
    getStartupStatus: vi.fn(),
    writeClipboard: vi.fn(() => Promise.resolve()),
  },
}));

describe("StartupGate", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("mounts the application only after startup is ready", async () => {
    vi.mocked(api.getStartupStatus).mockResolvedValue({ blockers: [] });

    render(<StartupGate><div>Full application</div></StartupGate>);

    expect(screen.queryByText("Full application")).not.toBeInTheDocument();
    expect(screen.getByText("Checking this installation…")).toBeInTheDocument();
    expect(await screen.findByText("Full application")).toBeInTheDocument();
  });

  it("renders feature-owned remediation without mounting the application", async () => {
    vi.mocked(api.getStartupStatus).mockResolvedValue({
      blockers: [{
        id: "workspaces.migration",
        feature: "Workspaces",
        title: "Migration required",
        message: "Move the existing library into a workspace.",
        actions: [{
          label: "Run migration",
          description: "Quit Wilkes first.",
          command: "python3 scripts/migrate_workspace.py",
        }],
      }],
    });

    render(<StartupGate><div>Full application</div></StartupGate>);

    expect(await screen.findByText("Migration required")).toBeInTheDocument();
    expect(screen.getByText("Workspaces")).toBeInTheDocument();
    expect(screen.queryByText("Full application")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    await waitFor(() => expect(api.writeClipboard).toHaveBeenCalledWith(
      "python3 scripts/migrate_workspace.py",
    ));
  });

  it("uses the same splash when startup status itself fails", async () => {
    vi.mocked(api.getStartupStatus).mockRejectedValue(new Error("native bridge unavailable"));

    render(<StartupGate><div>Full application</div></StartupGate>);

    expect(await screen.findByText("Wilkes could not check its startup status")).toBeInTheDocument();
    expect(screen.getByText("native bridge unavailable")).toBeInTheDocument();
    expect(screen.queryByText("Full application")).not.toBeInTheDocument();
  });
});
