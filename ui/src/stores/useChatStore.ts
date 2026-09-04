// The chat, in two halves that own different things.
//
// `useChatSession` is `@leonrjg/wilkes-chat`'s store: backends, the session,
// the transcript, branching. It knows nothing about documents.
//
// `useChatStore` is Wilkes's half: where the pane is, and what the chat is
// about. That second one is the whole of what Wilkes tells a session, and it
// is deliberately the *only* thing that knows it. It used to be two — this
// store and the session, with `chat_add_context` and friends pushing into a
// live subprocess and `switchBackend` replaying every document by hand into a
// fresh one — and anything that missed a replay left a session answering about
// a file the pane had stopped showing. Now the answer rides every call, and
// the shell mirrors it.

import { create } from "zustand";
import { createChatStore } from "@leonrjg/wilkes-chat";
import type { AgentBackend } from "@leonrjg/wilkes-chat";

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

interface ChatPaneStore {
  paneOpen: boolean;
  /** A session is being started or reopened behind the pane. Distinct from a
   *  turn streaming: this is the subprocess handshake, which can fail alone. */
  paneOpening: boolean;
  contextFiles: ChatContextFile[];
  activeDoc: ChatActiveDoc | null;

  togglePane: () => void;
  /** Open the pane, starting a session if there is not one already.
   *
   *  With a `backend`, switches to it — which is transient, and deliberately
   *  does not write `chat_backend`: Settings is the only writer of that
   *  preference, so switching agents for one conversation must not silently
   *  change what the next window opens on. */
  openPane: (backend?: AgentBackend) => Promise<void>;
  openPaneAndSend: (text: string) => Promise<void>;
  addContext: (path: string, pages?: number | null) => void;
  removeContext: (path: string) => void;
  setActiveDoc: (path: string | null, page?: number | null) => void;
  /** Everything the chat was about belonged to the workspace that just closed.
   *  Ends the session too: its MCP server answers out of that workspace. */
  resetForWorkspace: () => void;
}

const emptyContext = (): Pick<ChatPaneStore, "contextFiles" | "activeDoc"> => ({
  contextFiles: [],
  activeDoc: null,
});

export const useChatStore = create<ChatPaneStore>((set, get) => ({
  paneOpen: false,
  paneOpening: false,
  ...emptyContext(),

  togglePane: () => set((s) => ({ paneOpen: !s.paneOpen })),

  openPane: async (backend) => {
    set({ paneOpen: true, paneOpening: true });
    try {
      const session = useChatSession.getState();
      if (backend) {
        if (backend !== session.backend || !session.sessionId) {
          await useChatSession.getState().switchBackend(backend, { remember: false });
        }
        return;
      }
      // `initialize` is idempotent: it loads backends and history and opens a
      // session only if there is not one, which is exactly what "open the
      // pane" means when no agent was named.
      await useChatSession.getState().initialize();
    } finally {
      set({ paneOpening: false });
    }
  },

  openPaneAndSend: async (text) => {
    await get().openPane();
    if (!useChatSession.getState().sessionId) return;
    await useChatSession.getState().sendMessage(text);
  },

  addContext: (path, pages = null) =>
    set((s) =>
      s.contextFiles.some((file) => file.path === path)
        ? s
        : { contextFiles: [...s.contextFiles, { path, pages }] },
    ),

  removeContext: (path) =>
    set((s) => ({ contextFiles: s.contextFiles.filter((file) => file.path !== path) })),

  setActiveDoc: (path, page = null) => set({ activeDoc: path ? { path, page } : null }),

  resetForWorkspace: () => {
    useChatSession.getState().reset();
    set({ paneOpen: false, paneOpening: false, ...emptyContext() });
  },
}));

/** Everything the agent is being asked about, right now.
 *
 *  Asked for afresh on every call that opens a session or a turn, which is
 *  what lets a five-minute-old session and one starting now be told the same
 *  thing by the same code path. */
function hostContext(): ChatHostContext {
  const { contextFiles, activeDoc } = useChatStore.getState();
  return {
    search_root: useSettingsStore.getState().directory || null,
    active_doc: activeDoc,
    context_files: contextFiles,
  };
}

export const useChatSession = createChatStore({
  transport: chatTransport,
  preferredBackend: () => useSettingsStore.getState().chatBackend,
  // No `onBackendChosen`: the in-pane selector switches *this* session, and
  // Settings stays the only writer of the persisted preference.
  hostPayload: hostContext,
  // Reopening or branching a conversation puts the pane back into the state
  // that turn was asked in, so a branch is answered from the documents the
  // question was about rather than from whatever is open today.
  onHostRestore: (host) => {
    const restored = host as Partial<ChatHostContext> | null;
    if (!restored) return;
    useChatStore.setState({
      contextFiles: restored.context_files ?? [],
      activeDoc: restored.active_doc ?? null,
    });
  },
  onBackgroundError: (context, error) => console.error(`chat: ${context}`, error),
});
