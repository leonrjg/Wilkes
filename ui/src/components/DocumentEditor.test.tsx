import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../services";
import { useEditorStore } from "../stores/useEditorStore";
import DocumentEditor from "./DocumentEditor";

vi.mock("../services", () => ({
  api: {
    cancelCompletion: vi.fn().mockResolvedValue(undefined),
    completionFeedback: vi.fn().mockResolvedValue(undefined),
    getSessionSteering: vi.fn().mockResolvedValue({ documents: [], suppressions: [] }),
    onCompletion: vi.fn().mockResolvedValue(vi.fn()),
    requestCompletion: vi.fn().mockResolvedValue(undefined),
    resetSessionSteering: vi.fn().mockResolvedValue(undefined),
    saveDocument: vi.fn().mockResolvedValue(undefined),
  },
}));

describe("DocumentEditor grounded completion", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useEditorStore.setState({ buffers: {}, activeEditorPath: null });
  });

  it("regenerates with every suggestion already shown at the cursor", async () => {
    const path = "/library/draft.md";
    const store = useEditorStore.getState();
    store.ensureBuffer(path, "Existing claim.");
    store.beginCompletion(path, "first");
    store.applyCompletionEvent(path, "first", {
      kind: "shown",
      text: "A repetitive continuation.",
      mode: "append",
    });

    render(
      <DocumentEditor
        content="Existing claim."
        language="markdown"
        documentPath={path}
        semanticReady
        generationReady
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Regenerate completion" }));

    await waitFor(() => expect(api.requestCompletion).toHaveBeenCalledOnce());
    expect(api.requestCompletion).toHaveBeenCalledWith(
      expect.stringMatching(/^completion-/),
      expect.objectContaining({
        path,
        text: "Existing claim.",
        avoid_suggestions: ["A repetitive continuation."],
      }),
    );
  });

  it("shows estimated context-window usage in the inspector", async () => {
    const path = "/library/draft.md";
    const store = useEditorStore.getState();
    store.ensureBuffer(path, "Existing claim.");
    store.beginCompletion(path, "first");
    store.applyCompletionEvent(path, "first", {
      kind: "context",
      composition: {
        windowTokens: 32_768,
        usedTokens: 8_192,
        docCoverage: { kind: "full" },
        retrievalTokens: 4_000,
        docTokens: 3_500,
        scopeMode: "library",
      },
    });
    store.applyCompletionEvent(path, "first", {
      kind: "shown",
      text: "A grounded continuation.",
      mode: "append",
    });

    render(
      <DocumentEditor
        content="Existing claim."
        language="markdown"
        documentPath={path}
        semanticReady
        generationReady
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Inspect" }));

    expect(screen.getByText(/Estimated context use: 8,192 \/ 32,768 tokens \(25%\)/))
      .toBeInTheDocument();
    expect(screen.getByRole("progressbar", { name: "Estimated context window usage" }))
      .toHaveAttribute("aria-valuenow", "25");
  });

  it("excludes and restores an inspector source and regenerates with each scope change", async () => {
    const path = "/library/draft.md";
    const sourcePath = "/library/source.pdf";
    const store = useEditorStore.getState();
    store.ensureBuffer(path, "Existing claim.");
    store.beginCompletion(path, "first");
    store.applyCompletionEvent(path, "first", {
      kind: "retrieval",
      hyde_query: "Likely continuation",
      sources: [{
        path: sourcePath,
        title: "Source",
        page: 3,
        chunkIds: ["7"],
        score: 0.81,
        pinned: false,
      }],
    });
    store.applyCompletionEvent(path, "first", {
      kind: "shown",
      text: "A grounded continuation.",
      mode: "append",
    });

    render(
      <DocumentEditor
        content="Existing claim."
        language="markdown"
        documentPath={path}
        semanticReady
        generationReady
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Inspect" }));
    fireEvent.click(screen.getByRole("button", {
      name: "Remove Source from completion context",
    }));

    await waitFor(() => expect(api.requestCompletion).toHaveBeenCalledTimes(1));
    expect(useEditorStore.getState().buffers[path].scope.excluded).toEqual([sourcePath]);
    expect(api.requestCompletion).toHaveBeenLastCalledWith(
      expect.stringMatching(/^completion-/),
      expect.objectContaining({
        scope: { mode: "library", pinned: [], excluded: [sourcePath] },
      }),
    );

    fireEvent.click(screen.getByRole("button", {
      name: "Restore source.pdf to completion context",
    }));

    await waitFor(() => expect(api.requestCompletion).toHaveBeenCalledTimes(2));
    expect(useEditorStore.getState().buffers[path].scope.excluded).toEqual([]);
    expect(api.requestCompletion).toHaveBeenLastCalledWith(
      expect.stringMatching(/^completion-/),
      expect.objectContaining({
        scope: { mode: "library", pinned: [], excluded: [] },
      }),
    );
  });
});
