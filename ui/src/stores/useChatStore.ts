import { create } from "zustand";
import { chatApi } from "../services/chat";
import { useSettingsStore } from "./useSettingsStore";
import type {
  AgentBackend,
  BackendStatus,
  ChatConversationRecord,
  ChatConfigOption,
  ChatPermissionOption,
  ChatMessageRecord,
  ChatStartResult,
  ChatToolContentBlock,
  ChatToolLocation,
  ChatUpdate,
} from "../lib/types";
import { randomId } from "../lib/types";

export interface ChatToolChip {
  toolCallId: string;
  title: string;
  status: string;
  locations: ChatToolLocation[];
  content: ChatToolContentBlock[];
  rawInput: unknown;
  rawOutput: unknown;
}

/** A permission request surfaced for the user to approve/deny. While
 *  `decision` is null the buttons are live; once answered it holds the chosen
 *  option's label (or "Dismissed" if the turn ended first). */
export interface ChatPermissionPrompt {
  requestId: string;
  toolCallId: string;
  title: string | null;
  options: ChatPermissionOption[];
  decision: string | null;
}

export type ChatMessageContentBlock =
  | { kind: "text"; text: string }
  | { kind: "tool"; tool: ChatToolChip };

export interface ChatMessage {
  id: string;
  role: "user" | "assistant";
  content: ChatMessageContentBlock[];
  thought: string;
  streaming: boolean;
  error: string | null;
  permissions: ChatPermissionPrompt[];
  startedAtMs: number | null;
  endedAtMs: number | null;
}

export interface ChatContextFile {
  path: string;
  pages: number | null;
}

interface ChatStore {
  paneOpen: boolean;
  paneOpening: boolean;
  backends: BackendStatus[];
  backendsLoaded: boolean;
  backendsLoading: boolean;
  installingBackend: AgentBackend | null;
  hasAvailableBackend: boolean;
  sessionId: string | null;
  conversationId: string | null;
  backendSessionId: string | null;
  backend: AgentBackend | null;
  conversations: ChatConversationRecord[];
  conversationsLoading: boolean;
  messages: ChatMessage[];
  contextFiles: ChatContextFile[];
  activeDoc: { path: string; page: number | null } | null;
  streaming: boolean;
  currentTurnId: string | null;
  sessionError: string | null;
  /** ACP session config (model, thought level, ...) for the current session,
   *  if the agent supports it. Empty for agents that don't. */
  configOptions: ChatConfigOption[];

  togglePane: () => void;
  /** Opens the pane. With no `backend`, uses the current session if one is
   *  open, else the preferred backend (falling back to the first available
   *  one), mirroring spec §7.3. */
  openPane: (backend?: AgentBackend) => Promise<void>;
  openPaneAndSend: (text: string) => Promise<void>;
  loadBackends: (opts?: { force?: boolean }) => Promise<void>;
  loadConversations: () => Promise<void>;
  installBackend: (backend: AgentBackend) => Promise<void>;
  /** Transient: switches *this session's* agent without touching the
   *  persisted `chat_backend` preference (spec §7.1, §7.3 -- Settings is the
   *  only writer of that preference). Starts a new subprocess/session; the
   *  message thread resets, but context awareness carries over. */
  switchBackend: (backend: AgentBackend) => Promise<void>;
  openConversation: (conversationId: string) => Promise<void>;
  forgetConversation: (conversationId: string) => Promise<void>;
  newChat: () => Promise<void>;
  addContext: (path: string, pages?: number | null) => void;
  removeContext: (path: string) => void;
  setActiveDoc: (path: string | null, page?: number | null) => void;
  setConfigOption: (configId: string, value: string) => Promise<void>;
  sendMessage: (text: string) => Promise<void>;
  forkFromMessage: (messageId: string) => Promise<void>;
  editMessage: (messageId: string, text: string) => Promise<void>;
  answerPermission: (requestId: string, option: ChatPermissionOption | null) => Promise<void>;
  cancel: () => Promise<void>;
}

function upsertTool(
  content: ChatMessageContentBlock[],
  update: Extract<ChatUpdate, { kind: "tool" }>,
): ChatMessageContentBlock[] {
  const idx = content.findIndex(
    (block) => block.kind === "tool" && block.tool.toolCallId === update.tool_call_id,
  );
  if (idx === -1) {
    return [
      ...content,
      { kind: "tool", tool: {
        toolCallId: update.tool_call_id,
        title: update.title ?? "Tool call",
        status: update.status ?? "pending",
        locations: update.locations ?? [],
        content: update.content ?? [],
        rawInput: update.raw_input ?? null,
        rawOutput: update.raw_output ?? null,
      } },
    ];
  }
  const block = content[idx];
  if (block.kind !== "tool") return content;
  const prev = block.tool;
  const next = [...content];
  next[idx] = { kind: "tool", tool: {
    ...prev,
    title: update.title ?? prev.title,
    status: update.status ?? prev.status,
    locations: update.locations ?? prev.locations,
    content: update.content ?? prev.content,
    rawInput: update.raw_input !== undefined ? update.raw_input : prev.rawInput,
    rawOutput: update.raw_output !== undefined ? update.raw_output : prev.rawOutput,
  } };
  return next;
}

function appendText(content: ChatMessageContentBlock[], delta: string): ChatMessageContentBlock[] {
  const last = content[content.length - 1];
  if (last?.kind === "text") {
    return [...content.slice(0, -1), { kind: "text", text: last.text + delta }];
  }
  return [...content, { kind: "text", text: delta }];
}

function upsertPermission(
  permissions: ChatPermissionPrompt[],
  update: Extract<ChatUpdate, { kind: "permission" }>,
): ChatPermissionPrompt[] {
  if (permissions.some((p) => p.requestId === update.request_id)) return permissions;
  return [
    ...permissions,
    {
      requestId: update.request_id,
      toolCallId: update.tool_call_id,
      title: update.title ?? null,
      options: update.options,
      decision: null,
    },
  ];
}

/** When a turn ends, the backend cancels any permission request the user
 *  never answered -- reflect that so stale prompts stop offering live buttons. */
function dismissUndecided(permissions: ChatPermissionPrompt[]): ChatPermissionPrompt[] {
  if (!permissions.some((p) => p.decision === null)) return permissions;
  return permissions.map((p) => (p.decision === null ? { ...p, decision: "Dismissed" } : p));
}

function applyUpdate(message: ChatMessage, update: ChatUpdate): ChatMessage {
  switch (update.kind) {
    case "text":
      return { ...message, content: appendText(message.content, update.delta) };
    case "thought":
      return { ...message, thought: message.thought + update.delta };
    case "tool":
      return { ...message, content: upsertTool(message.content, update) };
    case "permission":
      return { ...message, permissions: upsertPermission(message.permissions, update) };
    case "error":
      return { ...message, error: update.message };
  }
}

function pickDefaultBackend(backends: BackendStatus[], preferred: AgentBackend): AgentBackend {
  if (backends.some((b) => b.backend === preferred && b.available)) return preferred;
  return backends.find((b) => b.available)?.backend ?? preferred;
}

function hasAvailableBackend(backends: BackendStatus[]) {
  return backends.some((b) => b.available);
}

function setBackendsState(backends: BackendStatus[]) {
  return {
    backends,
    backendsLoaded: true,
    hasAvailableBackend: hasAvailableBackend(backends),
  };
}

function recordToChatMessage(message: ChatMessageRecord): ChatMessage {
  return {
    id: message.message_id,
    role: message.role,
    content: message.content.map((block) => block.kind === "text" ? block : ({
      kind: "tool" as const,
      tool: {
        toolCallId: block.tool.tool_call_id,
        title: block.tool.title,
        status: block.tool.status,
        locations: block.tool.locations,
        content: block.tool.content,
        rawInput: block.tool.raw_input ?? null,
        rawOutput: block.tool.raw_output ?? null,
      },
    })),
    thought: message.thought,
    streaming: false,
    error: message.error,
    permissions: [],
    startedAtMs: null,
    endedAtMs: null,
  };
}

function messageTextContent(message: ChatMessage): string {
  return message.content
    .filter((block): block is Extract<ChatMessageContentBlock, { kind: "text" }> => block.kind === "text")
    .map((block) => block.text)
    .join(message.role === "assistant" ? "\n\n" : "");
}

async function subscribeToSession(sessionId: string) {
  await Promise.all([
    chatApi.onSessionError(sessionId, (message) => {
      if (useChatStore.getState().sessionId === sessionId) {
        useChatStore.setState({ sessionError: message });
      }
    }),
    chatApi.onConfigOptionsUpdated(sessionId, (options) => {
      if (useChatStore.getState().sessionId === sessionId) {
        useChatStore.setState({ configOptions: options });
      }
    }),
  ]);
}

function startedState(started: ChatStartResult, backend: AgentBackend | null) {
  return {
    sessionId: started.session_id,
    conversationId: started.conversation_id,
    backendSessionId: started.backend_session_id,
    backend,
    messages: started.messages.map(recordToChatMessage),
    contextFiles: started.context_files,
    activeDoc: started.active_doc,
    streaming: false,
    currentTurnId: null,
    sessionError: null,
    configOptions: started.config_options,
    paneOpen: true,
    paneOpening: false,
  };
}

export const useChatStore = create<ChatStore>((set, get) => ({
  paneOpen: false,
  paneOpening: false,
  backends: [],
  backendsLoaded: false,
  backendsLoading: false,
  installingBackend: null,
  hasAvailableBackend: false,
  sessionId: null,
  conversationId: null,
  backendSessionId: null,
  backend: null,
  conversations: [],
  conversationsLoading: false,
  messages: [],
  contextFiles: [],
  activeDoc: null,
  streaming: false,
  currentTurnId: null,
  sessionError: null,
  configOptions: [],

  togglePane: () => set((s) => ({ paneOpen: !s.paneOpen })),

  loadBackends: async (opts = {}) => {
    if (get().backendsLoaded && !opts.force) return;
    set({ backendsLoading: true });
    try {
      const backends = await chatApi.listBackends(Boolean(opts.force));
      set({ ...setBackendsState(backends), backendsLoading: false });
    } catch (error) {
      set({ backendsLoading: false });
      throw error;
    }
  },

  installBackend: async (backend) => {
    set({ installingBackend: backend, sessionError: null });
    try {
      const status = await chatApi.installBackend(backend);
      const backends = get().backends;
      const next = backends.some((b) => b.backend === status.backend)
        ? backends.map((b) => (b.backend === status.backend ? status : b))
        : [...backends, status];
      set({ ...setBackendsState(next), installingBackend: null });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set({ installingBackend: null, sessionError: message });
      throw error;
    }
  },

  loadConversations: async () => {
    set({ conversationsLoading: true });
    try {
      const conversations = await chatApi.listConversations();
      set({ conversations, conversationsLoading: false });
    } catch (error) {
      set({ conversationsLoading: false });
      throw error;
    }
  },

  openPane: async (backend) => {
    set({ paneOpen: true, paneOpening: true, sessionError: null });
    try {
      let state = get();
      if (state.backends.length === 0) {
        await state.loadBackends();
        state = get();
      }
      if (backend) {
        if (backend !== state.backend || !state.sessionId) await get().switchBackend(backend);
      } else if (!state.sessionId) {
        if (!state.hasAvailableBackend) return;
        const preferred = useSettingsStore.getState().chatBackend;
        await get().switchBackend(pickDefaultBackend(state.backends, preferred));
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set({ sessionError: message });
      throw error;
    } finally {
      set({ paneOpening: false });
    }
  },

  openPaneAndSend: async (text) => {
    await get().openPane();
    if (!get().sessionId) return;
    await get().sendMessage(text);
  },

  switchBackend: async (backend) => {
    const previousSessionId = get().sessionId;
    if (previousSessionId) {
      chatApi.close(previousSessionId).catch((e) => console.error("chat: close session failed", e));
    }

    set({
      sessionId: null,
      conversationId: null,
      backendSessionId: null,
      backend,
      messages: [],
      streaming: false,
      currentTurnId: null,
      sessionError: null,
      configOptions: [],
    });

    let sessionId: string;
    let conversationId: string | null;
    let backendSessionId: string | null;
    let configOptions: ChatConfigOption[];
    try {
      const started = await chatApi.start(backend, useSettingsStore.getState().directory || null);
      sessionId = started.session_id;
      conversationId = started.conversation_id;
      backendSessionId = started.backend_session_id;
      configOptions = started.config_options;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (get().backend === backend) {
        set({
          sessionId: null,
          conversationId: null,
          backendSessionId: null,
          streaming: false,
          currentTurnId: null,
          sessionError: message,
          configOptions: [],
        });
      }
      throw error;
    }

    set({
      sessionId,
      conversationId,
      backendSessionId,
      backend,
      streaming: false,
      currentTurnId: null,
      sessionError: null,
      configOptions,
    });

    subscribeToSession(sessionId).catch((e) =>
      console.error("chat: failed to subscribe to session", e),
    );

    // The conversation resets on a backend switch, but "what am I asking
    // about" is a Wilkes-owned fact independent of which CLI answers it --
    // replay it into the fresh session.
    const { contextFiles, activeDoc } = get();
    for (const file of contextFiles) {
      await chatApi.addContext(sessionId, file.path, file.pages);
    }
    if (activeDoc) {
      await chatApi.setActiveDoc(sessionId, activeDoc.path, activeDoc.page);
    }
    await get().loadConversations().catch((e) => console.error("chat: failed to load history", e));
  },

  openConversation: async (conversationId) => {
    const previousSessionId = get().sessionId;
    if (previousSessionId) {
      chatApi.close(previousSessionId).catch((e) => console.error("chat: close session failed", e));
    }

    const conversation = get().conversations.find((c) => c.conversation_id === conversationId);
    set({
      sessionId: null,
      conversationId,
      backendSessionId: null,
      backend: conversation?.backend ?? get().backend,
      messages: [],
      streaming: false,
      currentTurnId: null,
      sessionError: null,
      configOptions: [],
      paneOpen: true,
      paneOpening: true,
    });

    try {
      const started = await chatApi.openConversation(
        conversationId,
        useSettingsStore.getState().directory || null,
      );
      const opened = conversation ?? (await chatApi.listConversations()).find((c) => c.conversation_id === conversationId);
      set(startedState(started, opened?.backend ?? get().backend));

      subscribeToSession(started.session_id).catch((e) =>
        console.error("chat: failed to subscribe to session", e),
      );

      await get().loadConversations().catch((e) => console.error("chat: failed to load history", e));
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set({
        sessionId: null,
        backendSessionId: null,
        streaming: false,
        currentTurnId: null,
        sessionError: message,
        configOptions: [],
        paneOpening: false,
      });
      throw error;
    }
  },

  forgetConversation: async (conversationId) => {
    await chatApi.forgetConversation(conversationId);
    set((s) => ({
      conversations: s.conversations.filter((c) => c.conversation_id !== conversationId),
    }));
    if (get().conversationId === conversationId) {
      set({ conversationId: null, backendSessionId: null });
    }
  },

  newChat: async () => {
    const backend = get().backend;
    if (backend) await get().switchBackend(backend);
  },

  addContext: (path, pages = null) => {
    set((s) =>
      s.contextFiles.some((f) => f.path === path)
        ? s
        : { contextFiles: [...s.contextFiles, { path, pages }] },
    );
    const sessionId = get().sessionId;
    if (sessionId) chatApi.addContext(sessionId, path, pages).catch(console.error);
  },

  removeContext: (path) => {
    set((s) => ({ contextFiles: s.contextFiles.filter((f) => f.path !== path) }));
    const sessionId = get().sessionId;
    if (sessionId) chatApi.removeContext(sessionId, path).catch(console.error);
  },

  setActiveDoc: (path, page = null) => {
    set({ activeDoc: path ? { path, page } : null });
    const sessionId = get().sessionId;
    if (sessionId) chatApi.setActiveDoc(sessionId, path, page).catch(console.error);
  },

  setConfigOption: async (configId, value) => {
    const sessionId = get().sessionId;
    if (!sessionId) return;
    // Optimistic: flip the selected value immediately, then reconcile with
    // whatever the agent actually reports back (it may also change other
    // options' current values as a side effect, e.g. clamping thought level
    // to what the newly selected model supports).
    set((s) => ({
      configOptions: s.configOptions.map((o) =>
        o.id === configId ? { ...o, current_value: value } : o,
      ),
    }));
    try {
      const options = await chatApi.setConfigOption(sessionId, configId, value);
      if (get().sessionId === sessionId) set({ configOptions: options });
    } catch (error) {
      console.error("chat: set config option failed", error);
    }
  },

  sendMessage: async (text) => {
    const { sessionId, streaming, conversationId } = get();
    if (!sessionId || streaming) return;

    const turnId = chatApi.newTurnId();
    const startedAtMs = performance.now();
    const userMessage: ChatMessage = {
      id: randomId(),
      role: "user",
      content: [{ kind: "text", text }],
      thought: "",
      streaming: false,
      error: null,
      permissions: [],
      startedAtMs: null,
      endedAtMs: null,
    };
    const assistantMessage: ChatMessage = {
      id: turnId,
      role: "assistant",
      content: [],
      thought: "",
      streaming: true,
      error: null,
      permissions: [],
      startedAtMs,
      endedAtMs: null,
    };
    set((s) => ({
      messages: [...s.messages, userMessage, assistantMessage],
      streaming: true,
      currentTurnId: turnId,
    }));

    const patchAssistant = (patch: (m: ChatMessage) => ChatMessage) =>
      set((s) => ({ messages: s.messages.map((m) => (m.id === turnId ? patch(m) : m)) }));

    try {
      const result = await chatApi.send(
        sessionId,
        turnId,
        userMessage.id,
        text,
        useSettingsStore.getState().directory || null,
        (update) => patchAssistant((m) => applyUpdate(m, update)),
        () => {
          const endedAtMs = performance.now();
          patchAssistant((m) => ({
            ...m,
            streaming: false,
            endedAtMs,
            permissions: dismissUndecided(m.permissions),
          }));
          set({ streaming: false, currentTurnId: null });
        },
      );
      if (result.conversation_id && get().sessionId === sessionId) {
        if (get().conversationId !== result.conversation_id) {
          set({ conversationId: result.conversation_id });
        }
        if (!conversationId) {
          await get()
            .loadConversations()
            .catch((e) => console.error("chat: failed to load history", e));
        }
      }
    } catch (error) {
      console.error("chat: send failed", error);
      const message = error instanceof Error ? error.message : String(error);
      const endedAtMs = performance.now();
      patchAssistant((m) => ({
        ...m,
        streaming: false,
        error: message,
        endedAtMs,
        permissions: dismissUndecided(m.permissions),
      }));
      set({ streaming: false, currentTurnId: null });
    }
  },

  forkFromMessage: async (messageId) => {
    const state = get();
    const sourceConversationId = state.conversationId;
    const message = state.messages.find((candidate) => candidate.id === messageId);
    if (!sourceConversationId || !message || state.streaming || !state.backend) return;

    const started = await chatApi.forkConversation(
      sourceConversationId,
      messageId,
      message.role === "assistant",
    );
    const previousSessionId = state.sessionId;
    set(startedState(started, state.backend));
    if (previousSessionId) {
      chatApi.close(previousSessionId).catch((e) => console.error("chat: close session failed", e));
    }
    subscribeToSession(started.session_id).catch((e) =>
      console.error("chat: failed to subscribe to forked session", e),
    );
    await get().loadConversations().catch((e) =>
      console.error("chat: failed to refresh fork history", e),
    );
    if (message.role === "user") await get().sendMessage(messageTextContent(message));
  },

  editMessage: async (messageId, text) => {
    const state = get();
    const sourceConversationId = state.conversationId;
    const message = state.messages.find((candidate) => candidate.id === messageId);
    if (
      !sourceConversationId
      || !message
      || message.role !== "user"
      || state.streaming
      || !state.backend
      || !text.trim()
    ) return;

    const started = await chatApi.forkConversation(
      sourceConversationId,
      messageId,
      false,
    );
    const previousSessionId = state.sessionId;
    set(startedState(started, state.backend));
    if (previousSessionId) {
      chatApi.close(previousSessionId).catch((e) => console.error("chat: close session failed", e));
    }
    subscribeToSession(started.session_id).catch((e) =>
      console.error("chat: failed to subscribe to edited session", e),
    );
    await get().loadConversations().catch((e) =>
      console.error("chat: failed to refresh fork history", e),
    );
    await get().sendMessage(text.trim());
  },

  answerPermission: async (requestId, option) => {
    const sessionId = get().sessionId;
    if (!sessionId) return;
    // Reflect the decision immediately so the buttons resolve to a label; the
    // backend resolves the parked ACP request from this same call.
    const decision = option ? option.name : "Dismissed";
    set((s) => ({
      messages: s.messages.map((m) =>
        m.permissions.some((p) => p.requestId === requestId && p.decision === null)
          ? {
              ...m,
              permissions: m.permissions.map((p) =>
                p.requestId === requestId ? { ...p, decision } : p,
              ),
            }
          : m,
      ),
    }));
    try {
      await chatApi.answerPermission(sessionId, requestId, option?.option_id ?? null);
    } catch (error) {
      console.error("chat: answer permission failed", error);
    }
  },

  cancel: async () => {
    const { sessionId, currentTurnId } = get();
    if (sessionId && currentTurnId) {
      await chatApi.cancel(sessionId, currentTurnId).catch(console.error);
    }
  },
}));
