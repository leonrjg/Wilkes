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
  });

  it("starts timing when the assistant turn is created", async () => {
    vi.mocked(chatApi.send).mockResolvedValue({ conversation_id: null });

    await useChatStore.getState().sendMessage("Summarize this");

    const message = assistantMessage();
    expect(message.startedAtMs).toBe(1000);
    expect(message.endedAtMs).toBeNull();
    expect(message.streaming).toBe(true);
  });

  it("stops timing when the turn reports done", async () => {
    let doneHandler: ((done: { stop_reason: string }) => void) | null = null;
    vi.mocked(chatApi.send).mockImplementation(async (_sessionId, _turnId, _text, _searchRoot, _onUpdate, onDone) => {
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
    vi.mocked(chatApi.send).mockImplementation(async (_sessionId, _turnId, _text, _searchRoot, _onUpdate, onDone) => {
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
      },
    ]);

    await useChatStore.getState().sendMessage("Summarize this");

    expect(useChatStore.getState().conversationId).toBe("conversation-1");
    expect(useChatStore.getState().conversations).toHaveLength(1);
  });
});
