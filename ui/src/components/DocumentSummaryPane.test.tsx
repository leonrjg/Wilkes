import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GenerationStreamEvent } from "../lib/types";
import { api } from "../services";
import DocumentSummaryPane from "./DocumentSummaryPane";

vi.mock("../services", () => ({
  api: {
    summarizeDocument: vi.fn().mockResolvedValue(undefined),
    onGenerationStream: vi.fn(),
    writeClipboard: vi.fn().mockResolvedValue(undefined),
  },
}));

describe("DocumentSummaryPane", () => {
  let handler: (event: GenerationStreamEvent) => void;

  beforeEach(() => {
    vi.clearAllMocks();
    (api.summarizeDocument as any).mockResolvedValue(undefined);
    (api.onGenerationStream as any).mockImplementation(
      (next: (event: GenerationStreamEvent) => void) => {
        handler = next;
        return Promise.resolve(vi.fn());
      },
    );
  });

  it("streams, completes, copies, and regenerates a summary", async () => {
    render(<DocumentSummaryPane path="/docs/paper.pdf" onClose={vi.fn()} />);

    expect(screen.getByText("Thinking…")).toBeInTheDocument();
    await waitFor(() => expect(api.summarizeDocument).toHaveBeenCalledOnce());
    const requestId = (api.summarizeDocument as any).mock.calls[0][0];
    expect(api.summarizeDocument).toHaveBeenCalledWith(
      requestId,
      "/docs/paper.pdf",
    );

    act(() => {
      handler({
        phase: "delta",
        request_id: requestId,
        task: "document_summary",
        delta: "A concise ",
      });
      handler({
        phase: "completed",
        request_id: requestId,
        task: "document_summary",
        text: "A concise summary.",
      });
    });
    expect(screen.getByText("A concise summary.")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Copy summary" }));
    await waitFor(() =>
      expect(api.writeClipboard).toHaveBeenCalledWith("A concise summary."),
    );

    fireEvent.click(screen.getByRole("button", { name: "Regenerate summary" }));
    await waitFor(() =>
      expect(api.summarizeDocument).toHaveBeenCalledTimes(2),
    );
  });

  it("offers a retry after a failed terminal event", async () => {
    render(<DocumentSummaryPane path="/docs/empty.txt" onClose={vi.fn()} />);
    await waitFor(() => expect(api.summarizeDocument).toHaveBeenCalledOnce());
    const requestId = (api.summarizeDocument as any).mock.calls[0][0];

    act(() => {
      handler({
        phase: "failed",
        request_id: requestId,
        task: "document_summary",
        error: "document has no extractable text",
      });
    });
    expect(screen.getByText("Summary unavailable")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() =>
      expect(api.summarizeDocument).toHaveBeenCalledTimes(2),
    );
  });
});
