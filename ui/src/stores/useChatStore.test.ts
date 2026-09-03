import { beforeEach, describe, expect, it, vi } from "vitest";
import { createFakeTransport, type FakeTransport } from "@leonrjg/wilkes-chat/testing";

const transport: FakeTransport = createFakeTransport();
vi.mock("../services/chat", () => ({ chatTransport: transport }));

const { useChatSession, useChatStore } = await import("./useChatStore");
const { useSettingsStore } = await import("./useSettingsStore");

beforeEach(() => {
  transport.hosts.length = 0;
  useChatStore.setState({
    paneOpen: false,
    paneOpening: false,
    contextFiles: [],
    activeDoc: null,
  });
  useChatSession.getState().reset();
  useSettingsStore.setState({ directory: "/library" });
});

describe("what Wilkes tells a chat session", () => {
  it("is the pane's own state, read at the moment of the call", async () => {
    // The window is the only thing that knows which documents are in context.
    // Reading it late is what lets a document added between the handshake and
    // the question reach the question.
    await useChatStore.getState().openPane();
    useChatStore.getState().addContext("/library/paper.pdf", 12);
    useChatStore.getState().setActiveDoc("/library/notes.md", null);

    const sent = useChatSession.getState().sendMessage("What does it say?");
    transport.lastTurn().finish();
    await sent;

    expect(transport.hosts.at(-1)).toEqual({
      call: "send",
      host: {
        search_root: "/library",
        active_doc: { path: "/library/notes.md", page: null },
        context_files: [{ path: "/library/paper.pdf", pages: 12 }],
      },
    });
  });

  it("reaches a session that was already open, with no push of its own", async () => {
    // The three commands this replaced -- add_context, remove_context,
    // set_active_doc -- existed to keep a live session in step. Nothing pushes
    // now: the next call carries the answer.
    await useChatStore.getState().openPane();
    useChatStore.getState().addContext("/library/a.pdf");
    useChatStore.getState().removeContext("/library/a.pdf");
    useChatStore.getState().addContext("/library/b.pdf");

    const sent = useChatSession.getState().sendMessage("And now?");
    transport.lastTurn().finish();
    await sent;

    expect((transport.hosts.at(-1)?.host as { context_files: unknown[] }).context_files).toEqual([
      { path: "/library/b.pdf", pages: null },
    ]);
  });

  it("does not add the same document twice", () => {
    useChatStore.getState().addContext("/library/a.pdf", 3);
    useChatStore.getState().addContext("/library/a.pdf", 3);

    expect(useChatStore.getState().contextFiles).toHaveLength(1);
  });
});

describe("the pane", () => {
  it("opens on a session and reports while it is starting", async () => {
    const opening = useChatStore.getState().openPane();
    expect(useChatStore.getState().paneOpen).toBe(true);
    expect(useChatStore.getState().paneOpening).toBe(true);

    await opening;
    expect(useChatStore.getState().paneOpening).toBe(false);
    expect(useChatSession.getState().sessionId).toBeTruthy();
  });

  it("switching agents for one conversation does not rewrite the preference", async () => {
    // Settings is the only writer of `chat_backend`: an agent chosen because
    // the preferred one was busy is a fact about now, not a preference.
    useSettingsStore.setState({ chatBackend: "ClaudeCode" });
    await useChatStore.getState().openPane("Codex");

    expect(useChatSession.getState().backend).toBe("Codex");
    expect(useSettingsStore.getState().chatBackend).toBe("ClaudeCode");
  });

  it("lets go of the documents when the workspace does", async () => {
    // Every path in context belonged to the workspace that just closed, and
    // so does the MCP server answering out of it.
    await useChatStore.getState().openPane();
    useChatStore.getState().addContext("/library/a.pdf");

    useChatStore.getState().resetForWorkspace();

    expect(useChatStore.getState().contextFiles).toEqual([]);
    expect(useChatStore.getState().paneOpen).toBe(false);
    expect(useChatSession.getState().sessionId).toBeNull();
  });
});

describe("reopening a conversation", () => {
  it("puts the pane back on the documents the question was asked about", async () => {
    // Restored into Wilkes's own state rather than handed to the shell, so
    // what the pane shows and what the session is told stay the same thing.
    const asked = {
      conversation_id: "c1",
      backend: "ClaudeCode" as const,
      backend_session_id: "b1",
      cwd: "/tmp",
      title: "About the paper",
      created_at: "2026-09-01T10:00:00Z",
      updated_at: "2026-09-01T10:00:00Z",
      last_opened_at: "2026-09-01T10:00:00Z",
      config_values: [],
      messages: [
        {
          message_id: "m1",
          turn_id: "t1",
          role: "user" as const,
          thought: "",
          content: [{ kind: "text" as const, text: "What does it say?" }],
          error: null,
          environment: {
            config_values: [],
            host: {
              search_root: "/library",
              active_doc: { path: "/library/paper.pdf", page: 4 },
              context_files: [{ path: "/library/paper.pdf", pages: 12 }],
            },
          },
        },
      ],
    };
    const local = createFakeTransport({ conversations: [asked] });
    const { createChatStore } = await import("@leonrjg/wilkes-chat");
    const session = createChatStore({
      transport: local,
      onHostRestore: (host) => {
        const restored = host as { context_files: []; active_doc: null };
        useChatStore.setState({
          contextFiles: restored.context_files,
          activeDoc: restored.active_doc,
        });
      },
      onBackgroundError: () => {},
    });

    await session.getState().initialize();
    await session.getState().openConversation("c1");

    expect(useChatStore.getState().activeDoc).toEqual({ path: "/library/paper.pdf", page: 4 });
    expect(useChatStore.getState().contextFiles).toEqual([
      { path: "/library/paper.pdf", pages: 12 },
    ]);
  });
});
