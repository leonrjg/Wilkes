import { act, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import DocumentApp from "./DocumentApp";
import { ToastProvider } from "./components/Toast";
import { api } from "./services";
import { useSettingsStore } from "./stores/useSettingsStore";
import { useViewerStore } from "./stores/useViewerStore";
import type { NativeOpenRequest } from "./lib/types";

vi.mock("./services", () => ({
  api: {
    getGlobalSettings: vi.fn(),
    onNativeOpen: vi.fn(),
    documentWindowReady: vi.fn(),
  },
}));

vi.mock("./components/PreviewPane", () => ({
  default: ({ standalone }: { standalone?: boolean }) => (
    <div data-testid="preview-mode">{standalone ? "standalone" : "workspace"}</div>
  ),
}));

describe("DocumentApp", () => {
  let liveHandler: ((request: NativeOpenRequest) => void) | undefined;
  let openFile: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    vi.clearAllMocks();
    liveHandler = undefined;
    openFile = vi.fn();
    useViewerStore.setState({ openFile });
    useSettingsStore.setState({ replaceSettings: vi.fn() });
    vi.mocked(api.getGlobalSettings!).mockResolvedValue({ theme: "System" } as never);
    vi.mocked(api.onNativeOpen!).mockImplementation(async (handler) => {
      liveHandler = handler;
      return () => {};
    });
    vi.mocked(api.documentWindowReady!).mockResolvedValue([
      {
        paths: ["/outside/one.pdf", "/outside/two.md"],
        errors: ["Cannot open /outside/missing.pdf: not found"],
      },
    ]);
  });

  it("opens queued and live OS files without mounting the workspace shell", async () => {
    render(
      <ToastProvider>
        <DocumentApp />
      </ToastProvider>,
    );

    expect(screen.getByTestId("preview-mode")).toHaveTextContent("standalone");
    await waitFor(() => {
      expect(openFile).toHaveBeenNthCalledWith(1, "/outside/one.pdf");
      expect(openFile).toHaveBeenNthCalledWith(2, "/outside/two.md");
    });
    expect(screen.getByText(/missing\.pdf: not found/)).toBeInTheDocument();

    act(() => {
      liveHandler?.({ paths: ["/later/three.txt"], errors: [] });
    });
    expect(openFile).toHaveBeenNthCalledWith(3, "/later/three.txt");
  });
});
