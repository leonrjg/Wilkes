import { create } from "zustand";
import type {
  CompletionEvent,
  CompletionMode,
  CompletionScope,
  CompletionSource,
  ContextComposition,
} from "../lib/types";

const SCOPE_STORAGE_KEY = "wilkes.completion-scopes";
let activeWorkspaceId = "default";
let storedScopes: Record<string, CompletionScope> = {};
const scopeStorageKey = () => `${SCOPE_STORAGE_KEY}.${activeWorkspaceId}`;
const MAX_SUGGESTION_HISTORY = 8;

export type CompletionStatus = "idle" | "searching" | "nothing-relevant" | "error";

export interface ActiveCompletion {
  id: string;
  text: string;
  mode: CompletionMode;
  sources: CompletionSource[];
  hydeQuery: string;
  composition: ContextComposition | null;
}

export interface EditorBuffer {
  text: string;
  dirty: boolean;
  cursor: number;
  scope: CompletionScope;
  completion: ActiveCompletion | null;
  lastCompletion: ActiveCompletion | null;
  suggestionHistory: string[];
  status: CompletionStatus;
  error: string | null;
}

interface EditorStore {
  buffers: Record<string, EditorBuffer>;
  activeEditorPath: string | null;
  setActiveEditor(path: string | null): void;
  ensureBuffer(path: string, text: string): void;
  updateBuffer(path: string, text: string, cursor: number): void;
  setCursor(path: string, cursor: number): void;
  markSaved(path: string): void;
  beginCompletion(path: string, id: string): void;
  applyCompletionEvent(path: string, id: string, event: CompletionEvent): void;
  clearCompletion(path: string, status?: CompletionStatus): void;
  setScopeMode(path: string, mode: CompletionScope["mode"]): void;
  togglePin(path: string, pinnedPath: string): void;
  excludeFromContext(path: string, excludedPath: string): void;
  restoreToContext(path: string, restoredPath: string): void;
  switchWorkspace(workspaceId: string): void;
}

function readScopes(): Record<string, CompletionScope> {
  try {
    const parsed = JSON.parse(localStorage.getItem(scopeStorageKey()) ?? "{}") as unknown;
    return typeof parsed === "object" && parsed !== null
      ? parsed as Record<string, CompletionScope>
      : {};
  } catch {
    return {};
  }
}

storedScopes = typeof localStorage === "undefined" ? {} : readScopes();

function defaultScope(path: string): CompletionScope {
  const stored = storedScopes[path];
  if (!stored || !Array.isArray(stored.pinned)) {
    return { mode: "library", pinned: [], excluded: [] };
  }
  const pinned = stored.pinned.filter((item): item is string => typeof item === "string");
  const excluded = Array.isArray(stored.excluded)
    ? stored.excluded.filter(
        (item): item is string => typeof item === "string" && !pinned.includes(item),
      )
    : [];
  return {
    mode: pinned.length === 0 ? "library" : stored.mode,
    pinned,
    excluded,
  };
}

function persistScopes(buffers: Record<string, EditorBuffer>): void {
  try {
    localStorage.setItem(
      scopeStorageKey(),
      JSON.stringify(Object.fromEntries(Object.entries(buffers).map(([path, buffer]) => [path, buffer.scope]))),
    );
  } catch {
    // Completion steering remains valid in memory when storage is unavailable.
  }
}

function updateOne(
  buffers: Record<string, EditorBuffer>,
  path: string,
  update: (buffer: EditorBuffer) => EditorBuffer,
): Record<string, EditorBuffer> {
  const buffer = buffers[path];
  return buffer ? { ...buffers, [path]: update(buffer) } : buffers;
}

export const useEditorStore = create<EditorStore>((set) => ({
  buffers: {},
  activeEditorPath: null,

  setActiveEditor(path) {
    set({ activeEditorPath: path });
  },

  ensureBuffer(path, text) {
    set((state) => {
      const current = state.buffers[path];
      if (current) {
        if (current.dirty || current.text === text) return state;
        return { buffers: { ...state.buffers, [path]: { ...current, text } } };
      }
      return {
        buffers: {
          ...state.buffers,
          [path]: {
            text,
            dirty: false,
            cursor: text.length,
            scope: defaultScope(path),
            completion: null,
            lastCompletion: null,
            suggestionHistory: [],
            status: "idle",
            error: null,
          },
        },
      };
    });
  },

  updateBuffer(path, text, cursor) {
    set((state) => ({
      buffers: updateOne(state.buffers, path, (buffer) => ({
        ...buffer,
        text,
        cursor,
        dirty: true,
        completion: null,
        suggestionHistory: [],
        status: "idle",
        error: null,
      })),
    }));
  },

  setCursor(path, cursor) {
    set((state) => ({
      buffers: updateOne(state.buffers, path, (buffer) => ({
        ...buffer,
        cursor,
        suggestionHistory: buffer.cursor === cursor ? buffer.suggestionHistory : [],
      })),
    }));
  },

  markSaved(path) {
    set((state) => ({ buffers: updateOne(state.buffers, path, (buffer) => ({ ...buffer, dirty: false })) }));
  },

  beginCompletion(path, id) {
    set((state) => ({
      buffers: updateOne(state.buffers, path, (buffer) => ({
        ...buffer,
        completion: {
          id,
          text: "",
          mode: "append",
          sources: [],
          hydeQuery: "",
          composition: null,
        },
        status: "searching",
        error: null,
      })),
    }));
  },

  applyCompletionEvent(path, id, event) {
    set((state) => ({
      buffers: updateOne(state.buffers, path, (buffer) => {
        const completion = buffer.completion;
        if (!completion || completion.id !== id) return buffer;
        if (event.kind === "retrieval") {
          return {
            ...buffer,
            completion: { ...completion, sources: event.sources, hydeQuery: event.hyde_query },
            status: event.sources.length === 0 ? "nothing-relevant" : "searching",
          };
        }
        if (event.kind === "context") {
          return { ...buffer, completion: { ...completion, composition: event.composition } };
        }
        if (event.kind === "shown") {
          const shown = { ...completion, text: event.text, mode: event.mode };
          const suggestionHistory = [
            ...buffer.suggestionHistory.filter((candidate) => candidate !== event.text),
            event.text,
          ].slice(-MAX_SUGGESTION_HISTORY);
          return {
            ...buffer,
            completion: shown,
            lastCompletion: shown,
            suggestionHistory,
            status: "idle",
          };
        }
        if (event.kind === "suppressed") {
          return { ...buffer, completion: null, status: "nothing-relevant" };
        }
        return { ...buffer, completion: null, status: "error", error: event.message };
      }),
    }));
  },

  clearCompletion(path, status = "idle") {
    set((state) => ({
      buffers: updateOne(state.buffers, path, (buffer) => ({ ...buffer, completion: null, status })),
    }));
  },

  setScopeMode(path, mode) {
    set((state) => {
      const buffers = updateOne(state.buffers, path, (buffer) => ({
        ...buffer,
        scope: {
          ...buffer.scope,
          mode: buffer.scope.pinned.length === 0 ? "library" : mode,
        },
      }));
      persistScopes(buffers);
      return { buffers };
    });
  },

  togglePin(path, pinnedPath) {
    set((state) => {
      const buffers = updateOne(state.buffers, path, (buffer) => {
        const contains = buffer.scope.pinned.includes(pinnedPath);
        const pinned = contains
          ? buffer.scope.pinned.filter((candidate) => candidate !== pinnedPath)
          : [...buffer.scope.pinned, pinnedPath];
        return {
          ...buffer,
          scope: {
            pinned,
            excluded: contains
              ? buffer.scope.excluded
              : buffer.scope.excluded.filter((candidate) => candidate !== pinnedPath),
            mode: pinned.length === 0 ? "library" : buffer.scope.mode === "library" ? "prefer" : buffer.scope.mode,
          },
        };
      });
      persistScopes(buffers);
      return { buffers };
    });
  },

  excludeFromContext(path, excludedPath) {
    set((state) => {
      const buffers = updateOne(state.buffers, path, (buffer) => {
        const pinned = buffer.scope.pinned.filter((candidate) => candidate !== excludedPath);
        const excluded = buffer.scope.excluded.includes(excludedPath)
          ? buffer.scope.excluded
          : [...buffer.scope.excluded, excludedPath];
        return {
          ...buffer,
          scope: {
            pinned,
            excluded,
            mode: pinned.length === 0 ? "library" : buffer.scope.mode,
          },
        };
      });
      persistScopes(buffers);
      return { buffers };
    });
  },

  restoreToContext(path, restoredPath) {
    set((state) => {
      const buffers = updateOne(state.buffers, path, (buffer) => ({
        ...buffer,
        scope: {
          ...buffer.scope,
          excluded: buffer.scope.excluded.filter((candidate) => candidate !== restoredPath),
        },
      }));
      persistScopes(buffers);
      return { buffers };
    });
  },
  switchWorkspace(workspaceId) {
    activeWorkspaceId = workspaceId;
    storedScopes = typeof localStorage === "undefined" ? {} : readScopes();
    set({ buffers: {}, activeEditorPath: null });
  },
}));
