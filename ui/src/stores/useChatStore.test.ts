import { beforeEach, describe, expect, it, vi } from "vitest";
import { chatApi } from "../services/chat";
import { useChatStore } from "./useChatStore";

vi.mock("../services/chat", () => ({
  chatApi: {
    listBackends: vi.fn(),
    listConversations: vi.fn(),
    installBackend: vi.fn(),
    start: vi.fn(),
    openConversation: vi.fn(),
    forkConversation: vi.fn(),
    forgetConversation: vi.fn(),
    setConfigOption: vi.fn(),
    onConfigOptionsUpdated: vi.fn(),
    addContext: vi.fn(),
    removeContext: vi.fn(),
    setActiveDoc: vi.fn(),
    newTurnId: vi.fn(),
    send: vi.fn(),
    cancel: vi.fn(),
    close: vi.fn(),
    onSessionError: vi.fn(),
  },
}));

function resetChatStore() {
  useChatStore.setState({
    paneOpen: false,
    paneOpening: false,
    backends: [],
    backendsLoaded: false,
    backendsLoading: false,
    installingBackend: null,
    hasAvailableBackend: false,
    sessionId: "session-1",
    conversationId: null,
    backendSessionId: "backend-session-1",
    backend: "ClaudeCode",
    conversations: [],
    conversationsLoading: false,
    messages: [],
    contextFiles: [],
    activeDoc: null,
    streaming: false,
    currentTurnId: null,
    sessionError: null,
    configOptions: [],
  });
}

function assistantMessage() {
  const message = useChatStore.getState().messages.find((m) => m.role === "assistant");
  if (!message) throw new Error("assistant message was not created");
  return message;
}

describe("useChatStore chat timing", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    vi.clearAllMocks();
    resetChatStore();
    vi.spyOn(performance, "now").mockReturnValue(1000);
    vi.mocked(chatApi.newTurnId).mockReturnValue("turn-1");
    vi.mocked(chatApi.listConversations).mockResolvedValue([]);
    vi.mocked(chatApi.close).mockResolvedValue(undefined);
    vi.mocked(chatApi.onSessionError).mockResolvedValue(() => {});
    vi.mocked(chatApi.onConfigOptionsUpdated).mockResolvedValue(() => {});
  });

  it("starts timing when the assistant turn is created", async () => {
    vi.mocked(chatApi.send).mockResolvedValue({ conversation_id: null });

    await useChatStore.getState().sendMessage("Summarize this");

    const message = assistantMessage();
    expect(message.startedAtMs).toBe(1000);
    expect(message.endedAtMs).toBeNull();
    expect(message.streaming).toBe(true);
  });

  it("coalesces adjacent text deltas but starts a new text block after a tool", async () => {
    vi.mocked(chatApi.send).mockImplementation(async (_sessionId, _turnId, _userMessageId, _text, _searchRoot, onUpdate) => {
      onUpdate({ kind: "text", delta: "Before " });
      onUpdate({ kind: "text", delta: "tool." });
      onUpdate({
        kind: "tool",
        tool_call_id: "tool-1",
        title: "Search",
        status: "completed",
        locations: [],
        content: [],
        raw_input: null,
        raw_output: null,
      });
      onUpdate({ kind: "text", delta: "After tool." });
      return { conversation_id: null };
    });

    await useChatStore.getState().sendMessage("Find it");

    expect(assistantMessage().content).toEqual([
      { kind: "text", text: "Before tool." },
      expect.objectContaining({ kind: "tool", tool: expect.objectContaining({ toolCallId: "tool-1" }) }),
      { kind: "text", text: "After tool." },
    ]);
  });

  it("stops timing when the turn reports done", async () => {
    let doneHandler: ((done: { stop_reason: string }) => void) | null = null;
    vi.mocked(chatApi.send).mockImplementation(async (_sessionId, _turnId, _userMessageId, _text, _searchRoot, _onUpdate, onDone) => {
      doneHandler = onDone;
      return { conversation_id: null };
    });

    await useChatStore.getState().sendMessage("Summarize this");
    vi.mocked(performance.now).mockReturnValue(4250);
    doneHandler?.({ stop_reason: "end_turn" });

    const message = assistantMessage();
    expect(message.startedAtMs).toBe(1000);
    expect(message.endedAtMs).toBe(4250);
    expect(message.streaming).toBe(false);
    expect(useChatStore.getState().streaming).toBe(false);
    expect(useChatStore.getState().currentTurnId).toBeNull();
  });

  it("freezes the completed duration when later time passes", async () => {
    let doneHandler: ((done: { stop_reason: string }) => void) | null = null;
    vi.mocked(chatApi.send).mockImplementation(async (_sessionId, _turnId, _userMessageId, _text, _searchRoot, _onUpdate, onDone) => {
      doneHandler = onDone;
      return { conversation_id: null };
    });

    await useChatStore.getState().sendMessage("Summarize this");
    vi.mocked(performance.now).mockReturnValue(2750);
    doneHandler?.({ stop_reason: "end_turn" });
    vi.mocked(performance.now).mockReturnValue(9999);

    expect(assistantMessage().endedAtMs).toBe(2750);
  });

  it("stops timing when sending fails", async () => {
    vi.mocked(chatApi.send).mockRejectedValue(new Error("agent failed"));

    await useChatStore.getState().sendMessage("Summarize this");

    const message = assistantMessage();
    expect(message.startedAtMs).toBe(1000);
    expect(message.endedAtMs).toBe(1000);
    expect(message.streaming).toBe(false);
    expect(message.error).toBe("agent failed");
  });

  it("records the first-send conversation id after persistence", async () => {
    vi.mocked(chatApi.send).mockResolvedValue({ conversation_id: "conversation-1" });
    vi.mocked(chatApi.listConversations).mockResolvedValue([
      {
        conversation_id: "conversation-1",
        backend: "ClaudeCode",
        backend_session_id: "backend-session-1",
        cwd: "/tmp/workspace",
        title: "New Claude Code chat",
        created_at: "2026-07-05T00:00:00Z",
        updated_at: "2026-07-05T00:00:00Z",
        last_opened_at: "2026-07-05T00:00:00Z",
        context_files: [],
        active_doc: null,
        config_values: [],
        messages: [],
        parent_conversation_id: null,
        forked_from_message_id: null,
        branch_history_pending: false,
      },
    ]);

    await useChatStore.getState().sendMessage("Summarize this");

    expect(useChatStore.getState().conversationId).toBe("conversation-1");
    expect(useChatStore.getState().conversations).toHaveLength(1);
  });

  it("edits by forking before the selected user message and sending replacement text", async () => {
    useChatStore.setState({
      conversationId: "conversation-1",
      messages: [
        {
          id: "user-1",
          role: "user",
          content: [{ kind: "text", text: "Original" }],
          thought: "",
          streaming: false,
          error: null,
          permissions: [],
          startedAtMs: null,
          endedAtMs: null,
        },
      ],
    });
    vi.mocked(chatApi.forkConversation).mockResolvedValue({
      session_id: "session-fork",
      conversation_id: "conversation-fork",
      backend_session_id: "backend-fork",
      config_options: [],
      messages: [],
      context_files: [],
      active_doc: null,
    });
    vi.mocked(chatApi.send).mockResolvedValue({ conversation_id: "conversation-fork" });

    await useChatStore.getState().editMessage("user-1", "Revised");

    expect(chatApi.forkConversation).toHaveBeenCalledWith(
      "conversation-1",
      "user-1",
      false,
    );
    expect(chatApi.send).toHaveBeenCalledWith(
      "session-fork",
      "turn-1",
      expect.any(String),
      "Revised",
      null,
      expect.any(Function),
      expect.any(Function),
    );
    expect(useChatStore.getState().conversationId).toBe("conversation-fork");
  });

  it("preserves persisted message ids when reopening a conversation", async () => {
    const persistedMessage = {
      message_id: "stable-user-id",
      turn_id: "turn-1",
      role: "user" as const,
      thought: "",
      content: [{ kind: "text" as const, text: "Saved question" }],
      error: null,
      environment: {
        context_files: [],
        active_doc: null,
        search_root: null,
        config_values: [],
      },
    };
    useChatStore.setState({
      conversations: [{
        conversation_id: "conversation-1",
        backend: "ClaudeCode",
        backend_session_id: "backend-session-1",
        cwd: "/tmp/workspace",
        title: "Saved chat",
        created_at: "2026-07-18T00:00:00Z",
        updated_at: "2026-07-18T00:00:00Z",
        last_opened_at: "2026-07-18T00:00:00Z",
        context_files: [],
        active_doc: null,
        config_values: [],
        messages: [persistedMessage],
        parent_conversation_id: null,
        forked_from_message_id: null,
        branch_history_pending: false,
      }],
    });
    vi.mocked(chatApi.openConversation).mockResolvedValue({
      session_id: "session-opened",
      conversation_id: "conversation-1",
      backend_session_id: "backend-session-1",
      config_options: [],
      messages: [persistedMessage],
      context_files: [],
      active_doc: null,
    });

    await useChatStore.getState().openConversation("conversation-1");

    expect(useChatStore.getState().messages[0]?.id).toBe("stable-user-id");
  });
});
