// The chat, in two halves that own different things.
//
// Each pane's session is a `@leonrjg/wilkes-chat` store: backends, the
// session, the transcript, branching. It knows nothing about documents.
//
// `useChatStore` is Wilkes's half: which panes there are, and what each chat is
// about. That second one is the whole of what Wilkes tells a session, and it
// is deliberately the *only* thing that knows it. It used to be two — this
// store and the session, with `chat_add_context` and friends pushing into a
// live subprocess and `switchBackend` replaying every document by hand into a
// fresh one — and anything that missed a replay left a session answering about
// a file the pane had stopped showing. Now the answer rides every call, and
// the shell mirrors it.
//
// Several panes can be open, each its own session on its own agent with its
// own documents in context. The shell holds them all at once; what a pane is
// about is keyed by the pane, so a question asked in one is never answered
// from another's documents.

import { create } from "zustand";
import { createChatStore } from "@leonrjg/wilkes-chat";
import type { AgentBackend, BackendStatus, ChatStore } from "@leonrjg/wilkes-chat";

import { chatTransport } from "../services/chat";
import { useSettingsStore } from "./useSettingsStore";

export interface ChatContextFile {
  path: string;
  pages: number | null;
}

export interface ChatActiveDoc {
  path: string;
  page: number | null;
}

/** What the window says the chat is about, in the shape `WilkesChatHost`
 *  deserializes. snake_case because it is Rust's vocabulary, not this file's. */
export interface ChatHostContext {
  search_root: string | null;
  active_doc: ChatActiveDoc | null;
  context_files: ChatContextFile[];
}

/** One open chat pane: its session, and what it is answering about. */
export interface ChatPaneEntry {
  id: string;
  session: ChatStore;
  /** A session is being started or reopened behind the pane. Distinct from a
   *  turn streaming: this is the subprocess handshake, which can fail alone. */
  opening: boolean;
  contextFiles: ChatContextFile[];
  activeDoc: ChatActiveDoc | null;
}

interface ChatPanesStore {
  /** Whether the panes are shown. Hiding them keeps every session open, as
   *  the toolbar button always has. */
  panesVisible: boolean;
  /** The open panes, left to right. */
  panes: ChatPaneEntry[];
  /** The pane a request from elsewhere — "Ask about this file", a selection
   *  in the reader — goes to: the one last used. */
  focusedPane: string | null;
  /** What the reader is showing. Every pane follows it; a new one starts on
   *  it, so opening a second chat does not forget what is being read. */
  readerDoc: ChatActiveDoc | null;
  /** Which agents there are, as last read by anything in the window — the
   *  toolbar's menu or any pane. Asked of the shell, so it is one answer
   *  however many panes are open. */
  backends: BackendStatus[];
  backendsLoaded: boolean;
  hasAvailableBackend: boolean;

  loadBackends: (opts?: { force?: boolean }) => Promise<void>;
  /** Shows the panes, opening one if there are none, and starting the session
   *  of the one in use if it has none yet.
   *
   *  With a `backend`, switches that pane to it — which is transient, and
   *  deliberately does not write `chat_backend`: Settings is the only writer
   *  of that preference, so switching agents for one conversation must not
   *  silently change what the next window opens on. */
  showPanes: (backend?: AgentBackend) => Promise<void>;
  hidePanes: () => void;
  /** Opens another pane beside the others, on `backend` or the preferred one,
   *  and makes it the one in use. */
  newPane: (backend?: AgentBackend) => Promise<void>;
  /** Closes one pane and ends its session. The last one is hidden instead,
   *  with its conversation, so its close button means what the toolbar button
   *  does. */
  closePane: (id: string) => void;
  focusPane: (id: string) => void;
  /** Sends `text` from the pane in use, showing one if none is. */
  openPaneAndSend: (text: string) => Promise<void>;
  /** Puts a document in the context of the pane in use, showing one if none
   *  is. */
  askAbout: (path: string) => Promise<void>;
  addContext: (paneId: string, path: string, pages?: number | null) => void;
  removeContext: (paneId: string, path: string) => void;
  /** The document is gone from the library, so it leaves every pane. */
  forgetDocument: (path: string) => void;
  /** Takes the current document out of one pane, until the reader moves on. */
  deselectActiveDoc: (paneId: string) => void;
  /** The reader moved: every pane is now about what it shows. */
  setReaderDoc: (path: string | null, page?: number | null) => void;
  /** Everything the chat was about belonged to the workspace that just closed.
   *  Ends every session too: their MCP server answers out of that workspace. */
  resetForWorkspace: () => void;
}

let paneCount = 0;

function updatePane(
  panes: ChatPaneEntry[],
  id: string,
  patch: (pane: ChatPaneEntry) => Partial<ChatPaneEntry>,
): ChatPaneEntry[] {
  return panes.map((pane) => (pane.id === id ? { ...pane, ...patch(pane) } : pane));
}

/** Everything one pane's agent is being asked about, right now.
 *
 *  Asked for afresh on every call that opens a session or a turn, which is
 *  what lets a five-minute-old session and one starting now be told the same
 *  thing by the same code path. */
function hostContext(paneId: string): ChatHostContext {
  const pane = useChatStore.getState().panes.find((candidate) => candidate.id === paneId);
  return {
    search_root: useSettingsStore.getState().directory || null,
    active_doc: pane?.activeDoc ?? null,
    context_files: pane?.contextFiles ?? [],
  };
}

function sessionFor(paneId: string): ChatStore {
  return createChatStore({
    transport: chatTransport,
    preferredBackend: () => useSettingsStore.getState().chatBackend,
    // No `onBackendChosen`: the in-pane selector switches *this* session, and
    // Settings stays the only writer of the persisted preference.
    hostPayload: () => hostContext(paneId),
    // Reopening or branching a conversation puts the pane back into the state
    // that turn was asked in, so a branch is answered from the documents the
    // question was about rather than from whatever is open today.
    onHostRestore: (host) => {
      const restored = host as Partial<ChatHostContext> | null;
      if (!restored) return;
      useChatStore.setState((state) => ({
        panes: updatePane(state.panes, paneId, () => ({
          contextFiles: restored.context_files ?? [],
          activeDoc: restored.active_doc ?? null,
        })),
      }));
    },
    onBackgroundError: (context, error) => console.error(`chat: ${context}`, error),
  });
}

/** Starts the pane's session, or switches it to `backend`. */
async function startSession(paneId: string, backend: AgentBackend | undefined): Promise<void> {
  const pane = useChatStore.getState().panes.find((candidate) => candidate.id === paneId);
  if (pane === undefined) return;
  const setOpening = (opening: boolean) =>
    useChatStore.setState((state) => ({
      panes: updatePane(state.panes, paneId, () => ({ opening })),
    }));
  setOpening(true);
  try {
    const session = pane.session.getState();
    if (backend) {
      if (backend !== session.backend || !session.sessionId) {
        await session.switchBackend(backend, { remember: false });
      }
      return;
    }
    // `initialize` is idempotent: it loads backends and history and opens a
    // session only if there is not one, which is exactly what "open the
    // pane" means when no agent was named.
    await session.initialize();
  } finally {
    setOpening(false);
  }
}

/** The pane in use, or null when none is open. */
function focused(state: ChatPanesStore): ChatPaneEntry | null {
  return (
    state.panes.find((pane) => pane.id === state.focusedPane) ??
    state.panes[state.panes.length - 1] ??
    null
  );
}

export const useChatStore = create<ChatPanesStore>((set, get) => ({
  panesVisible: false,
  panes: [],
  focusedPane: null,
  readerDoc: null,
  backends: [],
  backendsLoaded: false,
  hasAvailableBackend: false,

  loadBackends: async (opts = {}) => {
    if (get().backendsLoaded && !opts.force) return;
    const backends = await chatTransport.listBackends(Boolean(opts.force));
    set({
      backends,
      backendsLoaded: true,
      hasAvailableBackend: backends.some((b) => b.available),
    });
  },

  showPanes: async (backend) => {
    const pane = focused(get());
    if (pane === null) {
      await get().newPane(backend);
      return;
    }
    set({ panesVisible: true });
    await startSession(pane.id, backend);
  },

  hidePanes: () => set({ panesVisible: false }),

  newPane: async (backend) => {
    paneCount += 1;
    const id = `chat-${paneCount}`;
    const session = sessionFor(id);
    // A pane's own reading of the agents is the newest there is — an install
    // from its setup panel lands there first — so the window's follows it.
    session.subscribe((state, previous) => {
      if (state.backends === previous.backends || !state.backendsLoaded) return;
      set({
        backends: state.backends,
        backendsLoaded: true,
        hasAvailableBackend: state.hasAvailableBackend,
      });
    });
    const pane: ChatPaneEntry = {
      id,
      session,
      opening: false,
      contextFiles: [],
      activeDoc: get().readerDoc,
    };
    set((state) => ({ panesVisible: true, panes: [...state.panes, pane], focusedPane: id }));
    await startSession(id, backend);
  },

  closePane: (id) => {
    const { panes, focusedPane } = get();
    const closing = panes.find((pane) => pane.id === id);
    if (closing === undefined) return;
    if (panes.length === 1) {
      set({ panesVisible: false });
      return;
    }
    closing.session.getState().reset();
    const remaining = panes.filter((pane) => pane.id !== id);
    set({
      panes: remaining,
      focusedPane: focusedPane === id ? remaining[remaining.length - 1].id : focusedPane,
    });
  },

  focusPane: (id) => {
    if (get().focusedPane !== id) set({ focusedPane: id });
  },

  openPaneAndSend: async (text) => {
    await get().showPanes();
    const pane = focused(get());
    if (pane === null || !pane.session.getState().sessionId) return;
    await pane.session.getState().sendMessage(text);
  },

  askAbout: async (path) => {
    await get().showPanes();
    const pane = focused(get());
    if (pane !== null) get().addContext(pane.id, path);
  },

  addContext: (paneId, path, pages = null) =>
    set((state) => ({
      panes: updatePane(state.panes, paneId, (pane) =>
        pane.contextFiles.some((file) => file.path === path)
          ? {}
          : { contextFiles: [...pane.contextFiles, { path, pages }] },
      ),
    })),

  removeContext: (paneId, path) =>
    set((state) => ({
      panes: updatePane(state.panes, paneId, (pane) => ({
        contextFiles: pane.contextFiles.filter((file) => file.path !== path),
      })),
    })),

  forgetDocument: (path) =>
    set((state) => ({
      panes: state.panes.map((pane) => ({
        ...pane,
        contextFiles: pane.contextFiles.filter((file) => file.path !== path),
      })),
    })),

  deselectActiveDoc: (paneId) =>
    set((state) => ({ panes: updatePane(state.panes, paneId, () => ({ activeDoc: null })) })),

  setReaderDoc: (path, page = null) => {
    const doc = path ? { path, page } : null;
    set((state) => ({
      readerDoc: doc,
      panes: state.panes.map((pane) => ({ ...pane, activeDoc: doc })),
    }));
  },

  resetForWorkspace: () => {
    for (const pane of get().panes) pane.session.getState().reset();
    set({ panesVisible: false, panes: [], focusedPane: null, readerDoc: null });
  },
}));
