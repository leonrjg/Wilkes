import { beforeEach, describe, expect, it, vi } from "vitest";
import { createFakeTransport, type FakeTransport } from "@leonrjg/wilkes-chat/testing";

/** A conversation asked about a paper, saved with what it was asked about. */
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

const transport: FakeTransport = createFakeTransport({ conversations: [asked] });
vi.mock("../services/chat", () => ({ chatTransport: transport }));

const { useChatStore } = await import("./useChatStore");
const { useSettingsStore } = await import("./useSettingsStore");

const panes = () => useChatStore.getState().panes;
const first = () => panes()[0];

beforeEach(() => {
  transport.hosts.length = 0;
  useChatStore.getState().resetForWorkspace();
  useSettingsStore.setState({ directory: "/library" });
});

describe("what Wilkes tells a chat session", () => {
  it("is the pane's own state, read at the moment of the call", async () => {
    // The window is the only thing that knows which documents are in context.
    // Reading it late is what lets a document added between the handshake and
    // the question reach the question.
    await useChatStore.getState().showPanes();
    useChatStore.getState().addContext(first().id, "/library/paper.pdf", 12);
    useChatStore.getState().setReaderDoc("/library/notes.md", null);

    const sent = first().session.getState().sendMessage("What does it say?");
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
    await useChatStore.getState().showPanes();
    const id = first().id;
    useChatStore.getState().addContext(id, "/library/a.pdf");
    useChatStore.getState().removeContext(id, "/library/a.pdf");
    useChatStore.getState().addContext(id, "/library/b.pdf");

    const sent = first().session.getState().sendMessage("And now?");
    transport.lastTurn().finish();
    await sent;

    expect((transport.hosts.at(-1)?.host as { context_files: unknown[] }).context_files).toEqual([
      { path: "/library/b.pdf", pages: null },
    ]);
  });

  it("does not add the same document twice", async () => {
    await useChatStore.getState().showPanes();
    useChatStore.getState().addContext(first().id, "/library/a.pdf", 3);
    useChatStore.getState().addContext(first().id, "/library/a.pdf", 3);

    expect(first().contextFiles).toHaveLength(1);
  });
});

describe("the pane", () => {
  it("opens on a session and reports while it is starting", async () => {
    const opening = useChatStore.getState().showPanes();
    expect(useChatStore.getState().panesVisible).toBe(true);
    expect(first().opening).toBe(true);

    await opening;
    expect(first().opening).toBe(false);
    expect(first().session.getState().sessionId).toBeTruthy();
  });

  it("switching agents for one conversation does not rewrite the preference", async () => {
    // Settings is the only writer of `chat_backend`: an agent chosen because
    // the preferred one was busy is a fact about now, not a preference.
    useSettingsStore.setState({ chatBackend: "ClaudeCode" });
    await useChatStore.getState().showPanes("Codex");

    expect(first().session.getState().backend).toBe("Codex");
    expect(useSettingsStore.getState().chatBackend).toBe("ClaudeCode");
  });

  it("lets go of the documents when the workspace does", async () => {
    // Every path in context belonged to the workspace that just closed, and
    // so does the MCP server answering out of it.
    await useChatStore.getState().showPanes();
    const session = first().session;
    useChatStore.getState().addContext(first().id, "/library/a.pdf");

    useChatStore.getState().resetForWorkspace();

    expect(panes()).toEqual([]);
    expect(useChatStore.getState().panesVisible).toBe(false);
    expect(session.getState().sessionId).toBeNull();
  });

  it("knows which agents there are once a pane has asked", async () => {
    await useChatStore.getState().showPanes();

    expect(useChatStore.getState().backendsLoaded).toBe(true);
    expect(useChatStore.getState().hasAvailableBackend).toBe(true);
  });
});

describe("several panes", () => {
  it("are separate sessions, each answering from its own documents", async () => {
    await useChatStore.getState().showPanes();
    await useChatStore.getState().newPane();
    const [a, b] = panes();
    expect(a.session.getState().sessionId).not.toBe(b.session.getState().sessionId);

    useChatStore.getState().addContext(a.id, "/library/a.pdf");
    useChatStore.getState().addContext(b.id, "/library/b.pdf");
    const sent = panes()[1].session.getState().sendMessage("Which one?");
    transport.lastTurn().finish();
    await sent;

    expect((transport.hosts.at(-1)?.host as { context_files: unknown[] }).context_files).toEqual([
      { path: "/library/b.pdf", pages: null },
    ]);
  });

  it("all follow the reader, and a new one starts on what it shows", async () => {
    await useChatStore.getState().showPanes();
    useChatStore.getState().setReaderDoc("/library/open.pdf", 3);
    await useChatStore.getState().newPane();

    expect(panes().map((pane) => pane.activeDoc)).toEqual([
      { path: "/library/open.pdf", page: 3 },
      { path: "/library/open.pdf", page: 3 },
    ]);

    useChatStore.getState().deselectActiveDoc(panes()[0].id);
    expect(panes().map((pane) => pane.activeDoc?.path ?? null)).toEqual([
      null,
      "/library/open.pdf",
    ]);
  });

  it("send a request from elsewhere to the pane last used", async () => {
    await useChatStore.getState().showPanes();
    await useChatStore.getState().newPane();
    const [a, b] = panes();
    useChatStore.getState().focusPane(a.id);

    await useChatStore.getState().askAbout("/library/asked.pdf");

    expect(panes()[0].contextFiles).toEqual([{ path: "/library/asked.pdf", pages: null }]);
    expect(panes()[1].contextFiles).toEqual([]);
    expect(b.id).toBe(panes()[1].id);
  });

  it("all let go of a document that left the library", async () => {
    await useChatStore.getState().showPanes();
    await useChatStore.getState().newPane();
    for (const pane of panes()) useChatStore.getState().addContext(pane.id, "/library/gone.pdf");

    useChatStore.getState().forgetDocument("/library/gone.pdf");

    expect(panes().map((pane) => pane.contextFiles)).toEqual([[], []]);
  });

  it("close one at a time, and the last one only hides", async () => {
    await useChatStore.getState().showPanes();
    await useChatStore.getState().newPane();
    const [a, b] = panes();

    useChatStore.getState().closePane(b.id);
    expect(panes().map((pane) => pane.id)).toEqual([a.id]);
    expect(b.session.getState().sessionId).toBeNull();
    expect(useChatStore.getState().focusedPane).toBe(a.id);

    useChatStore.getState().closePane(a.id);
    expect(panes().map((pane) => pane.id)).toEqual([a.id]);
    expect(useChatStore.getState().panesVisible).toBe(false);
    expect(a.session.getState().sessionId).toBeTruthy();
  });
});

describe("reopening a conversation", () => {
  it("puts the pane back on the documents the question was asked about", async () => {
    // Restored into Wilkes's own state rather than handed to the shell, so
    // what the pane shows and what the session is told stay the same thing.
    // Only into the pane that reopened it: the others are other questions.
    await useChatStore.getState().showPanes();
    await useChatStore.getState().newPane();
    useChatStore.getState().setReaderDoc("/library/today.md");

    await panes()[1].session.getState().openConversation("c1");

    expect(panes()[1].activeDoc).toEqual({ path: "/library/paper.pdf", page: 4 });
    expect(panes()[1].contextFiles).toEqual([{ path: "/library/paper.pdf", pages: 12 }]);
    expect(panes()[0].activeDoc).toEqual({ path: "/library/today.md", page: null });
    expect(panes()[0].contextFiles).toEqual([]);
  });
});
