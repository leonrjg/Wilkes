import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createFakeTransport, type FakeTransport } from "@leonrjg/wilkes-chat/testing";

const transport: FakeTransport = createFakeTransport();
vi.mock("../services/chat", () => ({ chatTransport: transport }));

const { default: ChatPane, contextFileMatchRef } = await import("./ChatPane");
const { useChatStore } = await import("../stores/useChatStore");
const { useViewerStore } = await import("../stores/useViewerStore");

/** Mount and wait for the session the pane opens on mount. */
async function mount() {
  const rendered = render(<ChatPane onClose={vi.fn()} />);
  await waitFor(() => expect(screen.getByLabelText("Message")).toBeInTheDocument());
  return rendered;
}

beforeEach(() => {
  useChatStore.setState({
    paneOpen: true,
    paneOpening: false,
    contextFiles: [{ path: "/tmp/notes.md", pages: null }],
    activeDoc: { path: "/tmp/paper.pdf", page: 7 },
  });
  useViewerStore.setState({ openMatch: vi.fn() });
});

describe("where a context chip opens to", () => {
  it("is a page in a PDF and the start of anything else", () => {
    // A chip carries a path and nothing else -- unlike a search result, it was
    // never a hit at a position -- so the page has to come from the reader's
    // own place in the document, or from the beginning.
    expect(contextFileMatchRef("/tmp/paper.pdf", 7)).toEqual({
      path: "/tmp/paper.pdf",
      origin: { PdfPage: { page: 7, bbox: null } },
    });
    expect(contextFileMatchRef("/tmp/paper.pdf")).toEqual({
      path: "/tmp/paper.pdf",
      origin: { PdfPage: { page: 1, bbox: null } },
    });
    expect(contextFileMatchRef("/tmp/notes.md")).toEqual({
      path: "/tmp/notes.md",
      origin: { TextFile: { line: 0, col: 0 } },
    });
  });
});

describe("the documents the next question will be answered from", () => {
  it("open in the viewer, whether pinned or merely current", async () => {
    await mount();

    fireEvent.click(screen.getByText("paper.pdf"));
    fireEvent.click(screen.getByText("notes.md"));

    expect(useViewerStore.getState().openMatch).toHaveBeenNthCalledWith(1, {
      path: "/tmp/paper.pdf",
      origin: { PdfPage: { page: 7, bbox: null } },
    });
    expect(useViewerStore.getState().openMatch).toHaveBeenNthCalledWith(2, {
      path: "/tmp/notes.md",
      origin: { TextFile: { line: 0, col: 0 } },
    });
  });

  it("are deselected in the pane and nowhere else", async () => {
    // No command goes out: the pane owns what the chat is about, and the next
    // call to the session carries the new answer.
    await mount();
    const before = transport.hosts.length;

    fireEvent.click(screen.getByRole("button", { name: "Deselect current document" }));

    expect(useChatStore.getState().activeDoc).toBeNull();
    expect(transport.hosts).toHaveLength(before);
  });

  it("can be pinned so they stay after the reader moves on", async () => {
    await mount();

    fireEvent.click(screen.getByRole("button", { name: "Pin current document to context" }));

    expect(useChatStore.getState().contextFiles.map((f) => f.path)).toContain("/tmp/paper.pdf");
  });

  it("are counted once when the open document is also pinned", async () => {
    useChatStore.setState({
      contextFiles: [{ path: "/tmp/paper.pdf", pages: null }],
      activeDoc: { path: "/tmp/paper.pdf", page: 2 },
    });
    await mount();

    expect(screen.getByText("Answering about 1 document")).toBeInTheDocument();
  });

  it("say so when there are none", async () => {
    useChatStore.setState({ contextFiles: [], activeDoc: null });
    await mount();

    expect(screen.getByText("No documents in context yet")).toBeInTheDocument();
    expect(screen.getByLabelText("Message")).toHaveAttribute(
      "placeholder",
      "Ask about your documents…",
    );
  });
});
