import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import ChatPane, {
  contextFileMatchRef,
  isTranscriptNearBottom,
  isTranscriptScrollUpKey,
  MessageBubble,
  runTranscriptProgrammaticScroll,
  shouldAdjustTranscriptScrollForItemSizeChange,
  shouldStickToTranscriptBottom,
} from "./ChatPane";
import type { ChatMessage } from "../stores/useChatStore";
import { useChatStore } from "../stores/useChatStore";
import { useViewerStore } from "../stores/useViewerStore";
import { chatApi } from "../services/chat";

vi.mock("../services/chat", () => ({
  chatApi: {
    listBackends: vi.fn().mockResolvedValue([]),
    listConversations: vi.fn().mockResolvedValue([]),
    installBackend: vi.fn(),
    start: vi.fn(),
    openConversation: vi.fn(),
    forkConversation: vi.fn(),
    forgetConversation: vi.fn(),
    setConfigOption: vi.fn(),
    onConfigOptionsUpdated: vi.fn().mockResolvedValue(() => {}),
    addContext: vi.fn(),
    removeContext: vi.fn(),
    setActiveDoc: vi.fn().mockResolvedValue(undefined),
    newTurnId: vi.fn(),
    send: vi.fn(),
    cancel: vi.fn(),
    close: vi.fn(),
    onSessionError: vi.fn().mockResolvedValue(() => {}),
  },
}));

function message(overrides: Partial<ChatMessage>): ChatMessage {
  return {
    id: "message-1",
    role: "assistant",
    content: [],
    thought: "",
    streaming: false,
    error: null,
    permissions: [],
    startedAtMs: null,
    endedAtMs: null,
    ...overrides,
  };
}

describe("MessageBubble", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders assistant replies as GitHub-flavored Markdown", () => {
    render(
      <MessageBubble
        message={message({
          content: [{ kind: "text", text: [
            "**Result**",
            "",
            "| Threshold | Precision |",
            "| --- | --- |",
            "| 50 | 100% |",
          ].join("\n") }],
        })}
        nowMs={0}
        onNavigate={vi.fn()}
      />,
    );

    expect(screen.getByText("Result").tagName).toBe("STRONG");
    expect(screen.getByRole("table")).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Threshold" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "100%" })).toBeInTheDocument();
  });

  it("keeps user messages as literal plain text", () => {
    render(
      <MessageBubble
        message={message({
          role: "user",
          content: [{ kind: "text", text: "**literal**\n| not | a table |" }],
        })}
        nowMs={0}
        onNavigate={vi.fn()}
      />,
    );

    expect(screen.getByText(/\*\*literal\*\*/)).toBeInTheDocument();
    expect(screen.queryByText("literal")).not.toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  it("copies the raw assistant message text", () => {
    render(
      <MessageBubble
        message={message({
          content: [{ kind: "text", text: "**Result**\n\nCopied as Markdown." }],
        })}
        nowMs={0}
        onNavigate={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Copy assistant message" }));

    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(
      "**Result**\n\nCopied as Markdown.",
    );
  });

  it("copies user message text", () => {
    render(
      <MessageBubble
        message={message({
          role: "user",
          content: [{ kind: "text", text: "plain user query" }],
        })}
        nowMs={0}
        onNavigate={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Copy your message" }));

    expect(navigator.clipboard.writeText).toHaveBeenCalledWith("plain user query");
  });

  it("edits a user message into a new fork", async () => {
    const onEdit = vi.fn().mockResolvedValue(undefined);
    render(
      <MessageBubble
        message={message({
          role: "user",
          content: [{ kind: "text", text: "Original question" }],
        })}
        nowMs={0}
        onNavigate={vi.fn()}
        onEdit={onEdit}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Edit your message" }));
    const editor = screen.getByRole("textbox", { name: "Edit message text" });
    fireEvent.change(editor, { target: { value: "Revised question" } });
    fireEvent.click(screen.getByRole("button", { name: "Save in fork" }));

    await waitFor(() => expect(onEdit).toHaveBeenCalledWith("message-1", "Revised question"));
  });

  it("forks from an assistant message", () => {
    const onFork = vi.fn();
    render(
      <MessageBubble
        message={message({ content: [{ kind: "text", text: "Answer" }] })}
        nowMs={0}
        onNavigate={vi.fn()}
        onFork={onFork}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Fork from assistant message" }));
    expect(onFork).toHaveBeenCalledWith("message-1");
  });

  it("renders copy and fork actions below the message", () => {
    render(
      <MessageBubble
        message={message({ content: [{ kind: "text", text: "Answer" }] })}
        nowMs={0}
        onNavigate={vi.fn()}
        onFork={vi.fn()}
      />,
    );

    const messageText = screen.getByText("Answer");
    const copyButton = screen.getByRole("button", { name: "Copy assistant message" });
    const forkButton = screen.getByRole("button", { name: "Fork from assistant message" });
    expect(messageText.compareDocumentPosition(copyButton) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(messageText.compareDocumentPosition(forkButton) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("renders and copies text blocks on opposite sides of a tool in order", () => {
    render(
      <MessageBubble
        message={message({
          content: [
            { kind: "text", text: "Before tool." },
            {
              kind: "tool",
              tool: {
                toolCallId: "tool-1",
                title: "Literature search",
                status: "completed",
                locations: [],
                content: [],
                rawInput: null,
                rawOutput: null,
              },
            },
            { kind: "text", text: "After tool." },
          ],
        })}
        nowMs={0}
        onNavigate={vi.fn()}
      />,
    );

    const before = screen.getByText("Before tool.");
    const tool = screen.getByRole("button", { name: /Literature search/ });
    const after = screen.getByText("After tool.");
    expect(before.compareDocumentPosition(tool) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(tool.compareDocumentPosition(after) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Copy assistant message" }));
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith("Before tool.\n\nAfter tool.");
  });
});

describe("isTranscriptNearBottom", () => {
  it("treats the transcript as stuck when it is within the bottom threshold", () => {
    expect(
      isTranscriptNearBottom({ scrollHeight: 1000, scrollTop: 452, clientHeight: 500 }),
    ).toBe(true);
  });

  it("stops sticking once the user scrolls away from the bottom", () => {
    expect(
      isTranscriptNearBottom({ scrollHeight: 1000, scrollTop: 300, clientHeight: 500 }),
    ).toBe(false);
  });
});

describe("isTranscriptScrollUpKey", () => {
  it.each(["ArrowUp", "PageUp", "Home"])("recognizes %s as upward scroll intent", (key) => {
    expect(isTranscriptScrollUpKey(key)).toBe(true);
  });

  it.each(["ArrowDown", "PageDown", "End", "Enter"])("does not intercept %s", (key) => {
    expect(isTranscriptScrollUpKey(key)).toBe(false);
  });
});

describe("shouldStickToTranscriptBottom", () => {
  it("does not reattach after an upward gesture that remains near the bottom", () => {
    expect(
      shouldStickToTranscriptBottom(
        { scrollHeight: 1000, scrollTop: 470, clientHeight: 500 },
        480,
        false,
      ),
    ).toBe(false);
  });

  it("reattaches when the user scrolls downward into the bottom zone", () => {
    expect(
      shouldStickToTranscriptBottom(
        { scrollHeight: 1000, scrollTop: 480, clientHeight: 500 },
        450,
        false,
      ),
    ).toBe(true);
  });

  it("remains detached outside the bottom zone", () => {
    expect(
      shouldStickToTranscriptBottom(
        { scrollHeight: 1000, scrollTop: 400, clientHeight: 500 },
        350,
        false,
      ),
    ).toBe(false);
  });
});

describe("streaming transcript scroll corrections", () => {
  it("blocks a pending programmatic scroll after the user detaches", () => {
    const scroll = vi.fn();

    runTranscriptProgrammaticScroll(false, scroll);

    expect(scroll).not.toHaveBeenCalled();
  });

  it("allows programmatic scrolling while following the bottom", () => {
    const scroll = vi.fn();

    runTranscriptProgrammaticScroll(true, scroll);

    expect(scroll).toHaveBeenCalledOnce();
  });

  it("keeps the offset fixed when a streaming item above it grows while detached", () => {
    expect(
      shouldAdjustTranscriptScrollForItemSizeChange(false, 300, 500),
    ).toBe(false);
  });

  it("retains the virtualizer's normal correction while following the bottom", () => {
    expect(
      shouldAdjustTranscriptScrollForItemSizeChange(true, 300, 500),
    ).toBe(true);
    expect(
      shouldAdjustTranscriptScrollForItemSizeChange(true, 600, 500),
    ).toBe(false);
  });
});

describe("contextFileMatchRef", () => {
  it("opens PDFs on the requested page", () => {
    expect(contextFileMatchRef("/tmp/paper.pdf", 7)).toEqual({
      path: "/tmp/paper.pdf",
      origin: { PdfPage: { page: 7, bbox: null } },
    });
  });

  it("opens non-PDF files without a highlighted line", () => {
    expect(contextFileMatchRef("/tmp/notes.md")).toEqual({
      path: "/tmp/notes.md",
      origin: { TextFile: { line: 0, col: 0 } },
    });
  });
});

describe("ChatPane context badges", () => {
  beforeEach(() => {
    useChatStore.setState({
      paneOpen: true,
      paneOpening: false,
      backends: [{ backend: "ClaudeCode", label: "Claude Code", available: true, auth_note: "", unavailable_reason: null, installable: false }],
      backendsLoaded: true,
      backendsLoading: false,
      installingBackend: null,
      hasAvailableBackend: true,
      sessionId: "session-1",
      conversationId: null,
      backendSessionId: null,
      backend: "ClaudeCode",
      conversations: [],
      conversationsLoading: false,
      messages: [],
      contextFiles: [{ path: "/tmp/notes.md", pages: null }],
      activeDoc: { path: "/tmp/paper.pdf", page: 7 },
      streaming: false,
      currentTurnId: null,
      sessionError: null,
      configOptions: [],
    });
    useViewerStore.setState({ openMatch: vi.fn() });
  });

  it("opens active and pinned context files through the viewer", () => {
    render(<ChatPane onClose={vi.fn()} />);

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

  it("deselects the currently open document from chat context", () => {
    render(<ChatPane onClose={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "Deselect current document" }));

    expect(useChatStore.getState().activeDoc).toBeNull();
    expect(chatApi.setActiveDoc).toHaveBeenCalledWith("session-1", null, null);
  });
});
