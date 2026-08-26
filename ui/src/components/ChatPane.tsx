import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { elementScroll, useVirtualizer } from "@tanstack/react-virtual";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  ChevronDown,
  Clock,
  Check, Copy,
  Download,
  Edit3,
  FileText,
  GitBranch,
  Loader,
  MapPin,
  Plus,
  RefreshCw,
  Send,
  Square,
  Trash2,
  X,
} from "react-feather";
import { useChatStore } from "../stores/useChatStore";
import type { ChatMessage, ChatToolChip } from "../stores/useChatStore";
import { useViewerStore } from "../stores/useViewerStore";
import { useContextMenu, ContextMenu } from "./ContextMenu";
import { confirmDialog } from "../lib/utils/dialog";
import type { AgentBackend, MatchRef } from "../lib/types";
import { Tooltip } from "./preview";
import { CopyButton } from "./CopyButton";

function fileName(path: string) {
  return path.split(/[/\\]/).pop() || path;
}

function statusDotClassName(available: boolean) {
  return `inline-block w-1.5 h-1.5 rounded-full flex-shrink-0 ${
    available ? "bg-green-500" : "bg-[var(--text-dim)]"
  }`;
}

function toolStatusIcon(status: string) {
  if (status === "completed") return "✓";
  if (status === "failed") return "✗";
  return "…";
}

function formatConversationTime(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

export function formatElapsedTime(elapsedMs: number) {
  const totalSeconds = Math.max(0, Math.floor(elapsedMs / 1000));
  const seconds = totalSeconds % 60;
  const totalMinutes = Math.floor(totalSeconds / 60);
  const minutes = totalMinutes % 60;
  const hours = Math.floor(totalMinutes / 60);

  if (hours > 0) {
    return `${hours}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
  }

  return `${minutes}:${String(seconds).padStart(2, "0")}`;
}

function messageElapsedLabel(message: ChatMessage, nowMs: number) {
  if (message.role !== "assistant" || message.startedAtMs == null) return null;
  return formatElapsedTime((message.endedAtMs ?? nowMs) - message.startedAtMs);
}

export function messageText(message: ChatMessage): string {
  return message.content
    .filter((block): block is Extract<(typeof message.content)[number], { kind: "text" }> => block.kind === "text")
    .map((block) => block.text)
    .join(message.role === "assistant" ? "\n\n" : "");
}

export function contextFileMatchRef(path: string, page: number | null = null): MatchRef {
  if (path.toLowerCase().endsWith(".pdf")) {
    return { path, origin: { PdfPage: { page: page ?? 1, bbox: null } } };
  }
  return { path, origin: { TextFile: { line: 0, col: 0 } } };
}

export function isTranscriptNearBottom(
  scroll: { scrollHeight: number; scrollTop: number; clientHeight: number },
  thresholdPx = 48,
) {
  return scroll.scrollHeight - scroll.scrollTop - scroll.clientHeight <= thresholdPx;
}

export function isTranscriptScrollUpKey(key: string) {
  return key === "ArrowUp" || key === "PageUp" || key === "Home";
}

export function shouldStickToTranscriptBottom(
  scroll: { scrollHeight: number; scrollTop: number; clientHeight: number },
  previousScrollTop: number,
  currentlyStuck: boolean,
) {
  if (!isTranscriptNearBottom(scroll)) return false;
  return currentlyStuck || scroll.scrollTop > previousScrollTop;
}

export function runTranscriptProgrammaticScroll(
  currentlyStuck: boolean,
  scroll: () => void,
) {
  if (currentlyStuck) scroll();
}

export function shouldAdjustTranscriptScrollForItemSizeChange(
  currentlyStuck: boolean,
  itemStart: number,
  scrollOffset: number,
) {
  return currentlyStuck && itemStart < scrollOffset;
}

interface Props {
  onClose: () => void;
}

export default function ChatPane({ onClose }: Props) {
  const parentRef = useRef<HTMLDivElement>(null);
  const stickToBottomRef = useRef(true);
  const lastScrollTopRef = useRef(0);
  const lastTouchYRef = useRef<number | null>(null);
  const [draft, setDraft] = useState("");

  const backends = useChatStore((s) => s.backends);
  const backendsLoaded = useChatStore((s) => s.backendsLoaded);
  const backendsLoading = useChatStore((s) => s.backendsLoading);
  const installingBackend = useChatStore((s) => s.installingBackend);
  const hasAvailableBackend = useChatStore((s) => s.hasAvailableBackend);
  const backend = useChatStore((s) => s.backend);
  const paneOpening = useChatStore((s) => s.paneOpening);
  const sessionId = useChatStore((s) => s.sessionId);
  const conversationId = useChatStore((s) => s.conversationId);
  const backendSessionId = useChatStore((s) => s.backendSessionId);
  const conversations = useChatStore((s) => s.conversations);
  const conversationsLoading = useChatStore((s) => s.conversationsLoading);
  const messages = useChatStore((s) => s.messages);
  const contextFiles = useChatStore((s) => s.contextFiles);
  const activeDoc = useChatStore((s) => s.activeDoc);
  const streaming = useChatStore((s) => s.streaming);
  const sessionError = useChatStore((s) => s.sessionError);
  const configOptions = useChatStore((s) => s.configOptions);
  const loadBackends = useChatStore((s) => s.loadBackends);
  const loadConversations = useChatStore((s) => s.loadConversations);
  const installBackend = useChatStore((s) => s.installBackend);
  const switchBackend = useChatStore((s) => s.switchBackend);
  const openConversation = useChatStore((s) => s.openConversation);
  const forgetConversation = useChatStore((s) => s.forgetConversation);
  const newChat = useChatStore((s) => s.newChat);
  const addContext = useChatStore((s) => s.addContext);
  const removeContext = useChatStore((s) => s.removeContext);
  const setActiveDoc = useChatStore((s) => s.setActiveDoc);
  const setConfigOption = useChatStore((s) => s.setConfigOption);
  const sendMessage = useChatStore((s) => s.sendMessage);
  const forkFromMessage = useChatStore((s) => s.forkFromMessage);
  const editMessage = useChatStore((s) => s.editMessage);
  const cancel = useChatStore((s) => s.cancel);
  const openMatch = useViewerStore((state) => state.openMatch);

  const { menu, openMenu, closeMenu } = useContextMenu<null>();

  const virtualizer = useVirtualizer({
    count: messages.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 96,
    overscan: 4,
    measureElement: (el) => el.getBoundingClientRect().height,
    // scrollToIndex starts a short reconciliation loop. Guard the underlying
    // writes too, so a reconciliation already in flight cannot pull the user
    // back down after an upward gesture detaches the transcript.
    scrollToFn: (offset, options, instance) => {
      runTranscriptProgrammaticScroll(stickToBottomRef.current, () => {
        elementScroll(offset, options, instance);
      });
    },
  });

  // The virtualizer normally compensates when an item that starts above the
  // viewport changes height. A streaming reply can itself start above the
  // viewport, so that compensation moves the transcript down on every chunk.
  // Once detached, keeping scrollTop unchanged is the intended behavior.
  virtualizer.shouldAdjustScrollPositionOnItemSizeChange = (item, _delta, instance) =>
    shouldAdjustTranscriptScrollForItemSizeChange(
      stickToBottomRef.current,
      item.start,
      instance.scrollOffset ?? 0,
    );

  useEffect(() => {
    stickToBottomRef.current = true;
    lastScrollTopRef.current = 0;
  }, [conversationId, sessionId]);

  useLayoutEffect(() => {
    if (messages.length === 0 || !stickToBottomRef.current) return;
    virtualizer.scrollToIndex(messages.length - 1, { align: "end" });
  }, [messages, virtualizer]);

  useEffect(() => {
    loadConversations().catch((e) => console.error("chat: failed to load history", e));
  }, [loadConversations]);

  const activeBackendStatus = backends.find((b) => b.backend === backend);
  const currentDocInContext =
    activeDoc != null && contextFiles.some((f) => f.path === activeDoc.path);
  const [nowMs, setNowMs] = useState(() => performance.now());
  const hasStreamingAssistant = messages.some((m) => m.role === "assistant" && m.streaming);

  useEffect(() => {
    if (!hasStreamingAssistant) return;
    setNowMs(performance.now());
    const interval = window.setInterval(() => setNowMs(performance.now()), 1000);
    return () => window.clearInterval(interval);
  }, [hasStreamingAssistant]);

  const handleSend = () => {
    const text = draft.trim();
    if (!text || streaming) return;
    setDraft("");
    sendMessage(text).catch((e) => console.error("chat: send failed", e));
  };

  const backendMenuItems = useMemo(
    () =>
      backends.map((b) => {
        // An unavailable-but-installable backend (npx present, adapter not yet
        // fetched) offers an inline pre-warm; the menu keeps itself open with a
        // spinner until install settles, then availability refreshes.
        if (!b.available && b.installable) {
          return {
            id: b.backend,
            label: `Install ${b.label}`,
            icon: Download,
            run: () => installBackend(b.backend as AgentBackend),
          };
        }
        return {
          id: b.backend,
          label: `${b.backend === backend ? "● " : "○ "}${b.label}${
            !b.available ? ` — ${b.unavailable_reason ?? b.auth_note}` : ""
          }`,
          disabled: !b.available,
          run: () => switchBackend(b.backend as AgentBackend),
        };
      }),
    [backends, backend, switchBackend, installBackend],
  );

  const historyMenuItems = useMemo(() => {
    if (conversations.length === 0) {
      return [
        {
          id: "empty",
          label: conversationsLoading ? "Loading saved chats..." : "No saved chats",
          disabled: true,
          run: () => {},
        },
      ];
    }
    return conversations.slice(0, 12).map((conversation) => ({
      id: conversation.conversation_id,
      label: `${conversation.conversation_id === conversationId ? "● " : ""}${conversation.parent_conversation_id ? "↳ " : ""}${conversation.title} · ${formatConversationTime(conversation.updated_at)}`,
      run: () => openConversation(conversation.conversation_id),
    }));
  }, [conversations, conversationsLoading, conversationId, openConversation]);

  return (
    <div className="h-full flex flex-col bg-[var(--bg-sidebar)] border-l border-[var(--border-main)]">
      {/* Header */}
      <div className="px-2 py-1.5 border-b border-[var(--border-main)] bg-[var(--bg-header)] flex flex-col gap-1">
        <div className="h-7 flex items-center gap-1.5 min-w-0">
          <Tooltip content="Switch agent">
            <button
              type="button"
              onClick={(e) => openMenu({ event: e, target: null, items: backendMenuItems, size: "content" })}
              className="h-7 max-w-[170px] flex items-center gap-1.5 px-1.5 text-xs rounded border border-transparent text-[var(--text-main)] hover:bg-[var(--bg-active)] hover:border-[var(--border-main)] min-w-0"
            >
              <span className={statusDotClassName(activeBackendStatus?.available ?? false)} />
              <span className="truncate font-medium">{activeBackendStatus?.label ?? "Select agent"}</span>
              {paneOpening ? (
                <Loader size={12} className="flex-shrink-0 text-[var(--accent-blue)] animate-spin" />
              ) : (
                <ChevronDown size={12} className="flex-shrink-0 text-[var(--text-dim)]" />
              )}
            </button>
          </Tooltip>
          <Tooltip content="New chat">
            <button
              type="button"
              onClick={() => newChat().catch((e) => console.error("chat: new chat failed", e))}
              disabled={!sessionId || paneOpening}
              className="w-7 h-7 ml-auto flex items-center justify-center rounded border border-transparent text-[var(--text-muted)] hover:text-[var(--text-main)] hover:bg-[var(--bg-active)] disabled:opacity-40 flex-shrink-0"
            >
              <Plus size={13} />
            </button>
          </Tooltip>
          <Tooltip content="Chat history">
            <button
              type="button"
              onClick={(e) => openMenu({ event: e, target: null, items: historyMenuItems })}
              disabled={paneOpening}
              className="w-7 h-7 flex items-center justify-center rounded border border-transparent text-[var(--text-muted)] hover:text-[var(--text-main)] hover:bg-[var(--bg-active)] disabled:opacity-40 flex-shrink-0"
            >
              <Clock size={13} />
            </button>
          </Tooltip>
          <Tooltip content="Copy backend session id">
            <CopyButton
              copy={() => backendSessionId ? navigator.clipboard.writeText(backendSessionId) : Promise.resolve()}
              disabled={!backendSessionId}
              copiedChildren={<Check size={13} />}
              className="w-7 h-7 flex items-center justify-center rounded border border-transparent text-[var(--text-muted)] hover:text-[var(--text-main)] hover:bg-[var(--bg-active)] disabled:opacity-40 flex-shrink-0"
            >
              <Copy size={13} />
            </CopyButton>
          </Tooltip>
          <Tooltip content="Forget this chat from Wilkes">
            <button
              type="button"
              onClick={async () => {
                if (!conversationId) return;
                const title = conversations.find(
                  (c) => c.conversation_id === conversationId,
                )?.title;
                const confirmed = await confirmDialog(
                  `Delete ${title ? `"${title}"` : "this chat"}? This cannot be undone.`,
                );
                if (!confirmed) return;
                forgetConversation(conversationId).catch((e) =>
                  console.error("chat: forget failed", e),
                );
              }}
              disabled={!conversationId}
              className="w-7 h-7 flex items-center justify-center rounded border border-transparent text-[var(--text-muted)] hover:text-[var(--text-error)] hover:bg-[var(--bg-active)] disabled:opacity-40 flex-shrink-0"
            >
              <Trash2 size={13} />
            </button>
          </Tooltip>
          <Tooltip content="Close chat">
            <button
              type="button"
              onClick={onClose}
              className="w-7 h-7 flex items-center justify-center rounded border border-transparent text-[var(--text-muted)] hover:text-[var(--text-main)] hover:bg-[var(--bg-active)] flex-shrink-0"
            >
              <X size={14} />
            </button>
          </Tooltip>
        </div>

        {configOptions.length > 0 && (
          <div
            className="grid gap-1"
            style={{ gridTemplateColumns: "repeat(auto-fit, minmax(92px, 1fr))" }}
          >
            {configOptions.map((option) => (
              <label
                key={option.id}
                className="h-6 min-w-0 flex items-center rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-1.5 text-[10px] text-[var(--text-dim)]"
              >
                <Tooltip content={option.name}>
                  <select
                    value={option.current_value}
                    onChange={(e) => setConfigOption(option.id, e.target.value)}
                    className="min-w-0 w-full bg-transparent text-[11px] font-medium text-[var(--text-main)] outline-none focus:text-[var(--accent-blue)]"
                  >
                    {option.choices.map((choice) => (
                      <option key={choice.value} value={choice.value}>
                        {choice.name}
                      </option>
                    ))}
                  </select>
                </Tooltip>
              </label>
            ))}
          </div>
        )}
      </div>

      {paneOpening && (
        <div className="px-2 py-1 border-b border-[var(--border-main)] bg-[var(--accent-blue-muted)]/40 flex items-center gap-1.5 text-[10px] text-[var(--text-muted)]">
          <Loader size={11} className="text-[var(--accent-blue)] animate-spin" />
          <span className="truncate">Starting chat session…</span>
        </div>
      )}

      {/* Context strip */}
      <div className="p-2 border-b border-[var(--border-main)] flex flex-wrap gap-1.5">
        {activeDoc && (
          <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] bg-[var(--accent-blue-muted)] text-[var(--text-main)] border border-[var(--border-main)]">
            <Tooltip content={`Open ${activeDoc.path}`} className="font-mono break-all">
              <button
                type="button"
                onClick={() => openMatch(contextFileMatchRef(activeDoc.path, activeDoc.page))}
                className="min-w-0 inline-flex items-center gap-1 rounded text-left hover:text-[var(--accent-blue)]"
              >
                <FileText size={10} className="flex-shrink-0" />
                <span className="truncate max-w-[140px]">{fileName(activeDoc.path)}</span>
                {activeDoc.page != null && (
                  <span className="text-[var(--text-dim)] flex-shrink-0">· p.{activeDoc.page}</span>
                )}
              </button>
            </Tooltip>
            <Tooltip content={currentDocInContext ? "Unpin current document" : "Pin current document to context"}>
              <button
                type="button"
                onClick={() =>
                  currentDocInContext ? removeContext(activeDoc.path) : addContext(activeDoc.path)
                }
                className={`ml-0.5 inline-flex items-center justify-center rounded p-0.5 transition-colors ${
                  currentDocInContext
                    ? "text-[var(--accent-blue)] hover:text-[var(--text-error)]"
                    : "text-[var(--text-dim)] hover:text-[var(--accent-blue)]"
                }`}
                aria-label={
                  currentDocInContext ? "Unpin current document" : "Pin current document to context"
                }
              >
                <MapPin size={10} fill={currentDocInContext ? "currentColor" : "none"} />
              </button>
            </Tooltip>
            <Tooltip content="Deselect current document">
              <button
                type="button"
                onClick={() => setActiveDoc(null)}
                className="inline-flex items-center justify-center rounded p-0.5 text-[var(--text-dim)] transition-colors hover:text-[var(--text-error)]"
                aria-label="Deselect current document"
              >
                <X size={10} />
              </button>
            </Tooltip>
          </span>
        )}
        {contextFiles
          .filter((f) => f.path !== activeDoc?.path)
          .map((file) => (
            <span
              key={file.path}
              className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] bg-[var(--bg-active)] text-[var(--text-muted)] border border-[var(--border-main)]"
            >
              <Tooltip content={`Open ${file.path}`} className="font-mono break-all">
                <button
                  type="button"
                  onClick={() => openMatch(contextFileMatchRef(file.path))}
                  className="min-w-0 inline-flex items-center gap-1 rounded text-left hover:text-[var(--accent-blue)]"
                >
                  <FileText size={10} className="flex-shrink-0" />
                  <span className="truncate max-w-[140px]">{fileName(file.path)}</span>
                </button>
              </Tooltip>
              <Tooltip content="Remove from context">
                <button
                  type="button"
                  onClick={() => removeContext(file.path)}
                  className="hover:text-[var(--text-error)]"
                >
                  <X size={10} />
                </button>
              </Tooltip>
            </span>
          ))}
        {!activeDoc && contextFiles.length === 0 && (
          <span className="text-[10px] text-[var(--text-dim)]">No documents in context yet</span>
        )}
      </div>

      {sessionError && (
        <div className="px-2 py-1.5 border-b border-[var(--border-main)] bg-[var(--text-error)]/10 flex items-center gap-2 text-[11px] text-[var(--text-error)]">
          <Tooltip content={sessionError}>
            <span className="flex-1 truncate">
              {activeBackendStatus?.label ?? "Agent"} error — {sessionError}
            </span>
          </Tooltip>
          <button
            type="button"
            onClick={() => backend && switchBackend(backend).catch((e) => console.error(e))}
            className="flex items-center gap-1 flex-shrink-0 hover:underline"
          >
            <RefreshCw size={11} /> Retry
          </button>
        </div>
      )}

      {/* Transcript */}
      {backendsLoading && !backendsLoaded ? (
        <div className="flex-1 p-3 flex items-center gap-2 text-xs text-[var(--text-muted)]">
          <Loader size={13} className="animate-spin text-[var(--accent-blue)]" />
          <span>Checking chat agents…</span>
        </div>
      ) : !hasAvailableBackend && backends.length > 0 ? (
        <div className="flex-1 overflow-auto p-3 space-y-2">
          <p className="text-xs text-[var(--text-muted)]">No agent is set up yet.</p>
          {backends.map((b) => (
            <div
              key={b.backend}
              className="text-[11px] text-[var(--text-dim)] border border-[var(--border-main)] rounded p-2 space-y-1.5"
            >
              <div className="flex items-center justify-between gap-2">
                <div className="font-medium text-[var(--text-main)]">{b.label}</div>
                {b.installable && (
                  <button
                    type="button"
                    onClick={() => installBackend(b.backend as AgentBackend).catch((e) => console.error(e))}
                    disabled={installingBackend !== null}
                    className="inline-flex items-center gap-1 text-[11px] text-[var(--accent-blue)] hover:underline disabled:opacity-50 flex-shrink-0"
                  >
                    {installingBackend === b.backend && <Loader size={10} className="animate-spin" />}
                    Install
                  </button>
                )}
              </div>
              <div>{b.unavailable_reason ?? b.auth_note}</div>
            </div>
          ))}
          <button
            type="button"
            onClick={() => loadBackends({ force: true }).catch((e) => console.error(e))}
            disabled={backendsLoading || installingBackend !== null}
            className="text-[11px] text-[var(--accent-blue)] hover:underline"
          >
            {backendsLoading ? "Checking…" : "Recheck"}
          </button>
        </div>
      ) : (
        <div
          ref={parentRef}
          className="flex-1 overflow-auto custom-scrollbar"
          style={{ overflowAnchor: "none" }}
          onScroll={(event) => {
            const scroll = event.currentTarget;
            stickToBottomRef.current = shouldStickToTranscriptBottom(
              scroll,
              lastScrollTopRef.current,
              stickToBottomRef.current,
            );
            lastScrollTopRef.current = scroll.scrollTop;
          }}
          onWheelCapture={(event) => {
            if (event.deltaY < 0) stickToBottomRef.current = false;
          }}
          onTouchStart={(event) => {
            lastTouchYRef.current = event.touches[0]?.clientY ?? null;
          }}
          onTouchMove={(event) => {
            const touchY = event.touches[0]?.clientY;
            if (touchY == null) return;
            if (lastTouchYRef.current != null && touchY > lastTouchYRef.current) {
              stickToBottomRef.current = false;
            }
            lastTouchYRef.current = touchY;
          }}
          onTouchEnd={() => {
            lastTouchYRef.current = null;
          }}
          onKeyDownCapture={(event) => {
            if (isTranscriptScrollUpKey(event.key)) stickToBottomRef.current = false;
          }}
          tabIndex={0}
        >
          <div style={{ height: `${virtualizer.getTotalSize()}px`, position: "relative" }}>
            {virtualizer.getVirtualItems().map((item) => {
              const message = messages[item.index];
              return (
                <div
                  key={message.id}
                  data-index={item.index}
                  ref={virtualizer.measureElement}
                  style={{
                    position: "absolute",
                    top: 0,
                    left: 0,
                    width: "100%",
                    transform: `translateY(${item.start}px)`,
                  }}
                  className="px-3 py-2"
                >
                  <MessageBubble
                    message={message}
                    nowMs={nowMs}
                    onNavigate={openMatch}
                    actionsDisabled={streaming || !conversationId}
                    onFork={(messageId) =>
                      forkFromMessage(messageId).catch((error) =>
                        console.error("chat: fork failed", error),
                      )
                    }
                    onEdit={(messageId, text) =>
                      editMessage(messageId, text).catch((error) =>
                        console.error("chat: edit failed", error),
                      )
                    }
                  />
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* Composer */}
      <div className="p-2 border-t border-[var(--border-main)] flex flex-col gap-1.5">
        <textarea
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              handleSend();
            } else if (e.key === "Escape") {
              (e.target as HTMLTextAreaElement).blur();
            }
          }}
          placeholder={
            contextFiles.length + (activeDoc ? 1 : 0) > 0
              ? `Ask about these ${new Set([...contextFiles.map((f) => f.path), ...(activeDoc ? [activeDoc.path] : [])]).size} documents…`
              : "Ask a question..."
          }
          rows={2}
          disabled={!hasAvailableBackend}
          className="w-full resize-none bg-[var(--bg-app)] border border-[var(--border-main)] rounded px-2 py-1.5 text-xs text-[var(--text-main)] outline-none focus:border-[var(--accent-blue)] disabled:opacity-50"
        />
        <div className="flex items-center justify-between gap-2">
          <span className="text-[10px] text-[var(--text-dim)] truncate">
            {contextFiles.length + (activeDoc && !currentDocInContext ? 1 : 0) > 0
              ? `Answering about ${new Set([...contextFiles.map((f) => f.path), ...(activeDoc ? [activeDoc.path] : [])]).size} document(s)`
              : "No documents in context"}
          </span>
          {streaming ? (
            <button
              type="button"
              onClick={() => cancel().catch((e) => console.error(e))}
              className="flex items-center gap-1 px-2.5 py-1 text-[11px] rounded bg-[var(--bg-active)] text-[var(--text-main)] border border-[var(--border-main)] hover:border-[var(--border-strong)] flex-shrink-0"
            >
              <Square size={10} /> Stop
            </button>
          ) : (
            <button
              type="button"
              onClick={handleSend}
              disabled={!draft.trim() || !hasAvailableBackend || !sessionId}
              className="flex items-center gap-1 px-2.5 py-1 text-[11px] rounded bg-[var(--accent-blue)] text-white hover:opacity-90 disabled:opacity-40 flex-shrink-0"
            >
              <Send size={10} /> Send
            </button>
          )}
        </div>
      </div>

      <ContextMenu menu={menu} onClose={closeMenu} />
    </div>
  );
}

export function MessageBubble({
  message,
  nowMs,
  onNavigate,
  onFork,
  onEdit,
  actionsDisabled = false,
}: {
  message: ChatMessage;
  nowMs: number;
  onNavigate: (matchRef: MatchRef) => void;
  onFork?: (messageId: string) => void | Promise<void>;
  onEdit?: (messageId: string, text: string) => void | Promise<void>;
  actionsDisabled?: boolean;
}) {
  const isUser = message.role === "user";
  const answerPermission = useChatStore((s) => s.answerPermission);
  const [expandedToolId, setExpandedToolId] = useState<string | null>(null);
  const [thinkingExpanded, setThinkingExpanded] = useState(false);
  const [editing, setEditing] = useState(false);
  const [editText, setEditText] = useState("");
  const [savingEdit, setSavingEdit] = useState(false);
  const hasThought = !isUser && message.thought.trim().length > 0;
  const copyText = messageText(message);
  const elapsedLabel = messageElapsedLabel(message, nowMs);
  return (
    <div className={isUser ? "text-right" : "text-left"}>
      <div
        className={`mb-0.5 flex items-center gap-1 ${
          isUser ? "justify-end" : "justify-start"
        }`}
      >
        <span className="text-[10px] text-[var(--text-dim)]">
          {isUser ? "You" : "Assistant"}
          {elapsedLabel && <span> · {elapsedLabel}</span>}
        </span>
        {isUser && onEdit && (
          <Tooltip content="Edit message in a new fork">
            <button
              type="button"
              aria-label="Edit your message"
              disabled={actionsDisabled}
              onClick={() => {
                setEditText(copyText);
                setEditing(true);
              }}
              className="inline-flex h-4 w-4 items-center justify-center rounded text-[var(--text-dim)] hover:bg-[var(--bg-active)] hover:text-[var(--text-main)] disabled:opacity-30"
            >
              <Edit3 size={10} />
            </button>
          </Tooltip>
        )}
      </div>
      <div
        className={`inline-block max-w-full text-left rounded px-2.5 py-1.5 text-xs ${
          isUser
            ? "bg-[var(--bg-card)] text-[var(--text-main)]"
            : "bg-[var(--bg-app)] border border-[var(--border-main)] text-[var(--text-main)]"
        }`}
      >
        {hasThought && (
          <div className="mb-1.5">
            <button
              type="button"
              onClick={() => setThinkingExpanded((v) => !v)}
              className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-[var(--bg-active)] text-[10px] text-[var(--text-muted)] hover:text-[var(--text-main)]"
            >
              <ChevronDown
                size={10}
                className={`transition-transform ${thinkingExpanded ? "" : "-rotate-90"}`}
              />
              <span>{message.streaming ? "Thinking..." : "Thinking"}</span>
            </button>
            {thinkingExpanded && (
              <pre className="mt-1 whitespace-pre-wrap break-words font-mono text-[10px] text-[var(--text-muted)] bg-[var(--bg-active)] px-2 py-1 rounded max-w-[420px] max-h-48 overflow-auto">
                {message.thought}
              </pre>
            )}
          </div>
        )}
        {message.permissions.length > 0 && (
          <div className="flex flex-col gap-1 mb-1.5">
            {message.permissions.map((prompt) => (
              <div
                key={prompt.requestId}
                className="rounded border border-[var(--border-main)] bg-[var(--bg-active)] px-2 py-1.5 text-[10px]"
              >
                <div className="text-[var(--text-muted)] mb-1">
                  Permission requested{prompt.title ? `: ${prompt.title}` : ""}
                </div>
                {prompt.decision === null ? (
                  <div className="flex flex-wrap gap-1">
                    {prompt.options.map((option) => {
                      const isAllow = option.kind.startsWith("allow");
                      return (
                        <button
                          key={option.option_id}
                          type="button"
                          onClick={() => answerPermission(prompt.requestId, option)}
                          className={`px-1.5 py-0.5 rounded ${
                            isAllow
                              ? "bg-[var(--accent-blue)] text-white hover:bg-[var(--accent-blue-hover)]"
                              : "bg-[var(--bg-card)] text-[var(--text-muted)] hover:text-[var(--text-main)]"
                          }`}
                        >
                          {option.name}
                        </button>
                      );
                    })}
                  </div>
                ) : (
                  <div className="text-[var(--text-dim)]">{prompt.decision}</div>
                )}
              </div>
            ))}
          </div>
        )}
        {isUser && editing ? (
          <div className="flex min-w-[260px] flex-col gap-1.5">
            <textarea
              aria-label="Edit message text"
              value={editText}
              onChange={(event) => setEditText(event.target.value)}
              rows={Math.max(2, Math.min(8, editText.split("\n").length))}
              className="w-full resize-y rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-2 py-1.5 text-xs outline-none focus:border-[var(--accent-blue)]"
              autoFocus
            />
            <div className="flex justify-end gap-1">
              <button
                type="button"
                onClick={() => setEditing(false)}
                disabled={savingEdit}
                className="rounded px-2 py-0.5 text-[10px] text-[var(--text-muted)] hover:bg-[var(--bg-active)]"
              >
                Cancel
              </button>
              <button
                type="button"
                disabled={savingEdit || !editText.trim()}
                onClick={async () => {
                  if (!onEdit || !editText.trim()) return;
                  setSavingEdit(true);
                  try {
                    await onEdit(message.id, editText.trim());
                  } finally {
                    setSavingEdit(false);
                  }
                }}
                className="rounded bg-[var(--accent-blue)] px-2 py-0.5 text-[10px] text-white disabled:opacity-40"
              >
                Save in fork
              </button>
            </div>
          </div>
        ) : isUser ? (
          <span className="whitespace-pre-wrap">{copyText}</span>
        ) : (
          <div className="flex flex-col gap-1.5">
            {message.content.map((block, index) => {
              if (block.kind === "text") {
                return (
                  <div className="prose prose-chat" key={`text-${index}`}>
                    <ReactMarkdown
                      remarkPlugins={[remarkGfm]}
                      components={{
                        a: ({ children, href }) => (
                          <a href={href} target="_blank" rel="noreferrer">
                            {children}
                          </a>
                        ),
                      }}
                    >
                      {block.text}
                    </ReactMarkdown>
                  </div>
                );
              }
              const tool = block.tool;
              const isExpanded = expandedToolId === tool.toolCallId;
              const hasDetail =
                tool.content.length > 0 || tool.rawInput != null || tool.rawOutput != null;
              return (
                <div key={`tool-${tool.toolCallId}`} className="w-fit max-w-full">
                  <button
                    type="button"
                    onClick={() =>
                      setExpandedToolId(isExpanded ? null : tool.toolCallId)
                    }
                    className="flex items-center gap-1 px-1.5 py-0.5 rounded bg-[var(--bg-active)] text-[10px] text-[var(--text-muted)] hover:text-[var(--text-main)] w-fit"
                  >
                    <FileText size={10} />
                    <span className="truncate max-w-[180px]">{tool.title}</span>
                    <span>{toolStatusIcon(tool.status)}</span>
                  </button>
                  {isExpanded && (
                    <ToolCallDetail tool={tool} onNavigate={onNavigate} hasDetail={hasDetail} />
                  )}
                </div>
              );
            })}
            {message.content.length === 0 && message.streaming && <span>…</span>}
            {message.streaming && <span className="animate-pulse">▍</span>}
          </div>
        )}
        {message.error && (
          <div className="mt-1 text-[10px] text-[var(--text-error)]">{message.error}</div>
        )}
      </div>
      <div
        className={`mt-0.5 flex items-center gap-1 ${
          isUser ? "justify-end" : "justify-start"
        }`}
      >
        <Tooltip content="Copy message">
          <CopyButton
            copy={() => copyText ? navigator.clipboard.writeText(copyText) : Promise.resolve()}
            disabled={!copyText}
            aria-label={`Copy ${isUser ? "your" : "assistant"} message`}
            copiedAriaLabel="Copied"
            copiedChildren={<Check size={10} />}
            className="inline-flex h-4 w-4 items-center justify-center rounded text-[var(--text-dim)] hover:bg-[var(--bg-active)] hover:text-[var(--text-main)] disabled:opacity-30"
          >
            <Copy size={10} />
          </CopyButton>
        </Tooltip>
        {onFork && (
          <Tooltip content="Fork conversation from this message">
            <button
              type="button"
              aria-label={`Fork from ${isUser ? "your" : "assistant"} message`}
              disabled={actionsDisabled || message.streaming}
              onClick={() => onFork(message.id)}
              className="inline-flex h-4 w-4 items-center justify-center rounded text-[var(--text-dim)] hover:bg-[var(--bg-active)] hover:text-[var(--text-main)] disabled:opacity-30"
            >
              <GitBranch size={10} />
            </button>
          </Tooltip>
        )}
      </div>
    </div>
  );
}

/** Click-to-expand detail behind a tool chip: the tool's own content
 *  (text/diff/terminal) plus the raw input/output ACP reported for it --
 *  "what was passed to the tool, what it returned". */
function ToolCallDetail({
  tool,
  onNavigate,
  hasDetail,
}: {
  tool: ChatToolChip;
  onNavigate: (matchRef: MatchRef) => void;
  hasDetail: boolean;
}) {
  return (
    <div className="mt-1 p-2 rounded border border-[var(--border-main)] bg-[var(--bg-app)] text-[10px] text-[var(--text-muted)] max-w-[420px] space-y-1.5">
      {tool.locations.length > 0 && (
        <div className="flex flex-col gap-0.5">
          {tool.locations.map((loc, i) => (
            <button
              key={i}
              type="button"
              onClick={() =>
                onNavigate({ path: loc.path, origin: { TextFile: { line: loc.line ?? 1, col: 1 } } })
              }
              className="text-left text-[var(--accent-blue)] hover:underline truncate"
            >
              {loc.path}
              {loc.line != null && `:${loc.line}`}
            </button>
          ))}
        </div>
      )}
      {tool.content.map((block, i) => {
        if (block.kind === "text") {
          return (
            <pre key={i} className="whitespace-pre-wrap break-words font-mono text-[var(--text-main)]">
              {block.text}
            </pre>
          );
        }
        if (block.kind === "diff") {
          return (
            <div key={i} className="space-y-0.5">
              <div className="text-[var(--text-dim)] truncate">{block.path}</div>
              {block.old_text != null && (
                <pre className="whitespace-pre-wrap break-words font-mono text-red-400/80 bg-red-500/10 px-1 rounded">
                  - {block.old_text}
                </pre>
              )}
              <pre className="whitespace-pre-wrap break-words font-mono text-green-500/80 bg-green-500/10 px-1 rounded">
                + {block.new_text}
              </pre>
            </div>
          );
        }
        return (
          <div key={i} className="text-[var(--text-dim)]">
            Terminal output ({block.terminal_id})
          </div>
        );
      })}
      {tool.rawInput != null && <RawJsonBlock label="Input" value={tool.rawInput} />}
      {tool.rawOutput != null && <RawJsonBlock label="Output" value={tool.rawOutput} />}
      {!hasDetail && <div className="italic">No further detail reported for this tool call.</div>}
    </div>
  );
}

function RawJsonBlock({ label, value }: { label: string; value: unknown }) {
  return (
    <div>
      <div className="text-[var(--text-dim)] uppercase tracking-wider text-[9px] mb-0.5">{label}</div>
      <pre className="whitespace-pre-wrap break-words font-mono text-[var(--text-main)] bg-[var(--bg-active)] px-1 py-0.5 rounded max-h-40 overflow-auto">
        {JSON.stringify(value, null, 2)}
      </pre>
    </div>
  );
}
