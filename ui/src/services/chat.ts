import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  AgentBackend,
  BackendStatus,
  ChatConversationRecord,
  ChatConfigOption,
  ChatDone,
  ChatSendResult,
  ChatStartResult,
  ChatUpdate,
} from "../lib/types";
import { randomId } from "../lib/types";

/** Desktop-only IPC wrapper for the chat pane (spec §7.8, §11 -- no server
 *  build for v1). Mirrors the per-id request/stream pattern in `tauri.ts`:
 *  the frontend generates the turn id and registers listeners *before*
 *  invoking, eliminating the race where `chat/done-*` could fire before the
 *  listener exists. */
export const chatApi = {
  listBackends(refresh = false): Promise<BackendStatus[]> {
    return invoke<BackendStatus[]>("chat_list_backends", { refresh });
  },

  installBackend(backend: AgentBackend): Promise<BackendStatus> {
    return invoke<BackendStatus>("chat_install_backend", { backend });
  },

  listConversations(): Promise<ChatConversationRecord[]> {
    return invoke<ChatConversationRecord[]>("chat_list_conversations");
  },

  openConversation(conversationId: string, searchRoot?: string | null): Promise<ChatStartResult> {
    return invoke<ChatStartResult>("chat_open_conversation", { conversationId, searchRoot: searchRoot || null });
  },

  forkConversation(
    conversationId: string,
    messageId: string,
    includeMessage: boolean,
  ): Promise<ChatStartResult> {
    return invoke<ChatStartResult>("chat_fork_conversation", {
      conversationId,
      messageId,
      includeMessage,
    });
  },

  forgetConversation(conversationId: string): Promise<void> {
    return invoke("chat_forget_conversation", { conversationId });
  },

  start(backend: AgentBackend, searchRoot?: string | null): Promise<ChatStartResult> {
    return invoke<ChatStartResult>("chat_start", { backend, searchRoot: searchRoot || null });
  },

  setConfigOption(sessionId: string, configId: string, value: string): Promise<ChatConfigOption[]> {
    return invoke<ChatConfigOption[]>("chat_set_config_option", { sessionId, configId, value });
  },

  /** Fires when the agent's own session config changes (e.g. it reports a
   *  new current model after `setConfigOption`, or switches on its own). */
  onConfigOptionsUpdated(
    sessionId: string,
    handler: (options: ChatConfigOption[]) => void,
  ): Promise<() => void> {
    return listen<ChatConfigOption[]>(`chat/config-${sessionId}`, (event) => handler(event.payload));
  },

  addContext(sessionId: string, path: string, pages?: number | null): Promise<void> {
    return invoke("chat_add_context", { sessionId, path, pages: pages ?? null });
  },

  removeContext(sessionId: string, path: string): Promise<void> {
    return invoke("chat_remove_context", { sessionId, path });
  },

  setActiveDoc(sessionId: string, path: string | null, page?: number | null): Promise<void> {
    return invoke("chat_set_active_doc", { sessionId, path, page: page ?? null });
  },

  /** Generates a turn id synchronously so the caller can key a placeholder
   *  message/Stop button to it *before* `send` resolves -- the alternative
   *  (generating it inside `send`) would race: `onUpdate` events can start
   *  arriving before an `await`ed `send()` call returns the id it used. */
  newTurnId(): string {
    return randomId();
  },

  async send(
    sessionId: string,
    turnId: string,
    userMessageId: string,
    text: string,
    searchRoot: string | null,
    onUpdate: (update: ChatUpdate) => void,
    onDone: (done: ChatDone) => void,
  ): Promise<ChatSendResult> {
    const unlistenUpdate = await listen<ChatUpdate>(`chat/update-${turnId}`, (event) =>
      onUpdate(event.payload),
    );
    const unlistenDone = await listen<ChatDone>(`chat/done-${turnId}`, (event) => {
      unlistenUpdate();
      unlistenDone();
      onDone(event.payload);
    });

    return invoke<ChatSendResult>("chat_send", {
      sessionId,
      turnId,
      userMessageId,
      text,
      searchRoot,
    });
  },

  cancel(sessionId: string, turnId: string): Promise<void> {
    return invoke("chat_cancel", { sessionId, turnId });
  },

  /** Answer a surfaced permission prompt. `optionId` is null when the user
   *  dismisses/denies without choosing one of the agent's offered options. */
  answerPermission(
    sessionId: string,
    requestId: string,
    optionId: string | null,
  ): Promise<void> {
    return invoke("chat_answer_permission", { sessionId, requestId, optionId });
  },

  close(sessionId: string): Promise<void> {
    return invoke("chat_close", { sessionId });
  },

  /** Fires if the subprocess dies outside of any turn's request/response
   *  cycle (spawn failure, crash). Register once per session. */
  onSessionError(sessionId: string, handler: (message: string) => void): Promise<() => void> {
    return listen<{ message: string }>(`chat/session-error-${sessionId}`, (event) =>
      handler(event.payload.message),
    );
  },
};
