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
        id: "feature.breaking-change",
        feature: "Semantic index",
        title: "Index rebuild required",
        message: "This build reads an index this installation does not have yet.",
        actions: [{
          label: "Rebuild the index",
          description: "Quit Wilkes first.",
          command: "wilkes index rebuild",
        }],
      }],
    });

    render(<StartupGate><div>Full application</div></StartupGate>);

    expect(await screen.findByText("Index rebuild required")).toBeInTheDocument();
    expect(screen.getByText("Semantic index")).toBeInTheDocument();
    expect(screen.queryByText("Full application")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    await waitFor(() => expect(api.writeClipboard).toHaveBeenCalledWith(
      "wilkes index rebuild",
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
