import { ChatPane as AcpChatPane } from "@leonrjg/wilkes-chat";
import { Tooltip } from "@leonrjg/wilkes-reader";
import { FileText, MapPin, X } from "react-feather";

import { useChatSession, useChatStore } from "../stores/useChatStore";
import { useViewerStore } from "../stores/useViewerStore";
import { confirmDialog } from "../lib/utils/dialog";
import type { MatchRef } from "../lib/types";

function fileName(path: string) {
  return path.split(/[/\\]/).pop() || path;
}

/** Where in a document to open, from a path alone.
 *
 *  A PDF opens at a page and everything else at its start, because a path is
 *  all a context chip has: unlike a search result, it was never a hit at a
 *  position.
 */
export function contextFileMatchRef(path: string, page: number | null = null): MatchRef {
  if (path.toLowerCase().endsWith(".pdf")) {
    return { path, origin: { PdfPage: { page: page ?? 1, bbox: null } } };
  }
  return { path, origin: { TextFile: { line: 0, col: 0 } } };
}

/** The documents this chat is answering about.
 *
 *  Rendered into the pane's `contextBar` slot, above the transcript and
 *  outside its scroll: it describes the *next* question, not the ones already
 *  asked, and reading back through the thread must not take it off screen.
 */
function ContextBar() {
  const contextFiles = useChatStore((s) => s.contextFiles);
  const activeDoc = useChatStore((s) => s.activeDoc);
  const addContext = useChatStore((s) => s.addContext);
  const removeContext = useChatStore((s) => s.removeContext);
  const setActiveDoc = useChatStore((s) => s.setActiveDoc);
  const openMatch = useViewerStore((s) => s.openMatch);

  // The open document is in context anyway — it is pushed into every prompt.
  // Pinning is what keeps it there after the reader moves on.
  const pinned = activeDoc != null && contextFiles.some((f) => f.path === activeDoc.path);

  if (!activeDoc && contextFiles.length === 0) {
    return <span className="text-[var(--text-dim)]">No documents in context yet</span>;
  }

  return (
    <>
      {activeDoc && (
        <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-[var(--accent-blue-muted)] text-[var(--text-main)] border border-[var(--border-main)]">
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
          <Tooltip content={pinned ? "Unpin current document" : "Pin current document to context"}>
            <button
              type="button"
              onClick={() => (pinned ? removeContext(activeDoc.path) : addContext(activeDoc.path))}
              aria-label={pinned ? "Unpin current document" : "Pin current document to context"}
              className={`ml-0.5 inline-flex items-center justify-center rounded p-0.5 transition-colors ${
                pinned
                  ? "text-[var(--accent-blue)] hover:text-[var(--text-error)]"
                  : "text-[var(--text-dim)] hover:text-[var(--accent-blue)]"
              }`}
            >
              <MapPin size={10} fill={pinned ? "currentColor" : "none"} />
            </button>
          </Tooltip>
          <Tooltip content="Deselect current document">
            <button
              type="button"
              onClick={() => setActiveDoc(null)}
              aria-label="Deselect current document"
              className="inline-flex items-center justify-center rounded p-0.5 text-[var(--text-dim)] transition-colors hover:text-[var(--text-error)]"
            >
              <X size={10} />
            </button>
          </Tooltip>
        </span>
      )}
      {contextFiles
        .filter((file) => file.path !== activeDoc?.path)
        .map((file) => (
          <span
            key={file.path}
            className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-[var(--bg-active)] text-[var(--text-muted)] border border-[var(--border-main)]"
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
                aria-label={`Remove ${fileName(file.path)} from context`}
                className="hover:text-[var(--text-error)]"
              >
                <X size={10} />
              </button>
            </Tooltip>
          </span>
        ))}
    </>
  );
}

/** Count the documents the next question will be answered from.
 *
 *  A set, because the open document is very often also pinned, and saying
 *  "2 documents" about one file reads as a bug. */
function documentCount(paths: string[]) {
  return new Set(paths).size;
}

/** Wilkes's "Ask the documents" pane.
 *
 *  The chat is `@leonrjg/wilkes-chat`'s, whole: the agent selector, the
 *  history, the transcript with its branching, the composer, the permission
 *  prompts. What Wilkes adds is what it is *about* — the strip of documents
 *  above the thread, and the two lines of copy that name them.
 */
export default function ChatPane({ onClose }: { onClose?: () => void }) {
  const contextFiles = useChatStore((s) => s.contextFiles);
  const activeDoc = useChatStore((s) => s.activeDoc);
  const openMatch = useViewerStore((s) => s.openMatch);

  const count = documentCount([
    ...contextFiles.map((file) => file.path),
    ...(activeDoc ? [activeDoc.path] : []),
  ]);

  return (
    <AcpChatPane
      store={useChatSession}
      onClose={onClose}
      // A tool call that names a file is a place in the library, and Wilkes
      // has a reader for it. A general chat has nowhere to send one.
      onOpenLocation={(location) =>
        openMatch(contextFileMatchRef(location.path, location.line ?? null))
      }
      confirmDelete={(title) => confirmDialog(`Delete "${title}"? This cannot be undone.`)}
      contextBar={<ContextBar />}
      placeholder={count > 0 ? `Ask about these ${count} documents…` : "Ask about your documents…"}
      hint={
        count > 0
          ? `Answering about ${count} document${count === 1 ? "" : "s"}`
          : "Enter to send · Shift+Enter for a new line"
      }
      emptyState="Ask a question about the documents in context."
    />
  );
}
