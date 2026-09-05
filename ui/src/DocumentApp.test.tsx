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
    nativeOpenReady: vi.fn(),
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
    vi.mocked(api.nativeOpenReady!).mockResolvedValue([
      {
        paths: ["/outside/one.pdf", "/outside/two.md"],
        errors: ["Cannot open /outside/missing.pdf: not found"],
        workspace: null,
        origin: null,
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
      expect(openFile).toHaveBeenNthCalledWith(1, "/outside/one.pdf", null);
      expect(openFile).toHaveBeenNthCalledWith(2, "/outside/two.md", null);
    });
    expect(screen.getByText(/missing\.pdf: not found/)).toBeInTheDocument();

    act(() => {
      liveHandler?.({
        paths: ["/later/three.txt"],
        errors: [],
        workspace: null,
        origin: null,
      });
    });
    expect(openFile).toHaveBeenNthCalledWith(3, "/later/three.txt", null);
  });

  /** A link may name a page even when it names no workspace: the standalone
   *  reader has no library behind it, but it still has a document to place. */
  it("lands a link that named a page at that page", async () => {
    vi.mocked(api.nativeOpenReady!).mockResolvedValue([]);
    render(
      <ToastProvider>
        <DocumentApp />
      </ToastProvider>,
    );
    await waitFor(() => expect(liveHandler).toBeDefined());

    const origin = { PdfPage: { page: 42, bbox: null } } as const;
    act(() => {
      liveHandler?.({ paths: ["/outside/book.pdf"], errors: [], workspace: null, origin });
    });

    expect(openFile).toHaveBeenCalledWith("/outside/book.pdf", origin);
  });
});
