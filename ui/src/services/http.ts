import type {
  EmbedderModel,
  EmbedDone,
  EmbedError,
  EmbedProgress,
  EmbeddingEngine,
  FileEntry,
  FileMatches,
  IndexStatus,
  MatchRef,
  ModelDescriptor,
  PreviewData,
  SearchQuery,
  SearchStats,
  Settings,
} from "../lib/types";
import type { SearchApi, WebSourceApi } from "./api";

export class HttpSearchApi implements SearchApi {
  private controllers = new Map<string, AbortController>();

  async search(
    query: SearchQuery,
    onResult: (fm: FileMatches) => void,
    onComplete: (stats: SearchStats) => void,
  ): Promise<string> {
    const searchId = crypto.randomUUID();
    const controller = new AbortController();
    this.controllers.set(searchId, controller);

    this.streamSearch(controller.signal, query, onResult, onComplete)
      .catch((err) => {
        if (err.name !== "AbortError") console.error("Search stream error:", err);
      })
      .finally(() => this.controllers.delete(searchId));

    return searchId;
  }

  private async streamSearch(
    signal: AbortSignal,
    query: SearchQuery,
    onResult: (fm: FileMatches) => void,
    onComplete: (stats: SearchStats) => void,
  ): Promise<void> {
    const response = await fetch("/api/search", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(query),
      signal,
    });

    if (!response.ok || !response.body) {
      throw new Error(`Search request failed: ${response.status}`);
    }

    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";

    let currentEvent = "";
    let currentData = "";

    while (true) {
      const { done, value } = await reader.read();
      if (done) break;

      buffer += decoder.decode(value, { stream: true });
      const lines = buffer.split("\n");
      buffer = lines.pop() ?? "";

      for (const line of lines) {
        if (line.startsWith("event: ")) {
          currentEvent = line.slice(7).trim();
        } else if (line.startsWith("data: ")) {
          currentData = line.slice(6).trim();
        } else if (line === "") {
          if (currentEvent === "result" && currentData) {
            onResult(JSON.parse(currentData) as FileMatches);
          } else if (currentEvent === "complete" && currentData) {
            onComplete(JSON.parse(currentData) as SearchStats);
          }
          currentEvent = "";
          currentData = "";
        }
      }
    }
  }

  async cancelSearch(searchId: string): Promise<void> {
    this.controllers.get(searchId)?.abort();
    this.controllers.delete(searchId);
  }

  async preview(matchRef: MatchRef): Promise<PreviewData> {
    const res = await fetch("/api/preview", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(matchRef),
    });
    if (!res.ok) throw new Error(`Preview failed: ${res.status}`);
    return res.json() as Promise<PreviewData>;
  }

  async getSettings(): Promise<Settings> {
    const res = await fetch("/api/settings");
    if (!res.ok) throw new Error(`getSettings failed: ${res.status}`);
    return res.json() as Promise<Settings>;
  }

  async updateSettings(patch: Partial<Settings>): Promise<Settings> {
    const res = await fetch("/api/settings", {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(patch),
    });
    if (!res.ok) throw new Error(`updateSettings failed: ${res.status}`);
    return res.json() as Promise<Settings>;
  }

  async listFiles(root: string): Promise<FileEntry[]> {
    const res = await fetch(`/api/files?root=${encodeURIComponent(root)}`);
    if (!res.ok) throw new Error(`listFiles failed: ${res.status}`);
    return res.json() as Promise<FileEntry[]>;
  }

  async openFile(path: string): Promise<PreviewData> {
    const res = await fetch("/api/file", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path }),
    });
    if (!res.ok) throw new Error(`openFile failed: ${res.status}`);
    return res.json() as Promise<PreviewData>;
  }

  resolvePdfUrl(path: string): string {
    return `/asset?path=${encodeURIComponent(path)}`;
  }

  // ── Semantic / embed commands ──────────────────────────────────────────────

  async listModels(_engine: EmbeddingEngine): Promise<ModelDescriptor[]> {
    return [];
  }

  async getModelSize(_engine: EmbeddingEngine, _modelId: string): Promise<number> {
    return 0;
  }

  async downloadModel(_model: EmbedderModel, _engine: EmbeddingEngine): Promise<void> {}

  async buildIndex(_root: string, _model: EmbedderModel, _engine: EmbeddingEngine): Promise<void> {}

  async cancelEmbed(): Promise<void> {}

  async getIndexStatus(): Promise<IndexStatus> {
    throw new Error("Semantic search not available on web");
  }

  async deleteIndex(): Promise<void> {}

  async onEmbedProgress(_handler: (p: EmbedProgress) => void): Promise<() => void> {
    return Promise.resolve(() => {});
  }

  async onEmbedDone(_handler: (d: EmbedDone) => void): Promise<() => void> {
    return Promise.resolve(() => {});
  }

  async onEmbedError(_handler: (e: EmbedError) => void): Promise<() => void> {
    return Promise.resolve(() => {});
  }
}

export class HttpSourceApi implements WebSourceApi {
  type = "web" as const;

  async uploadFiles(files: File[]): Promise<string> {
    const formData = new FormData();
    for (const file of files) {
      const name = (file as File & { webkitRelativePath?: string }).webkitRelativePath || file.name;
      formData.append("files", file, name);
    }
    const res = await fetch("/api/upload", { method: "POST", body: formData });
    if (!res.ok) throw new Error(`Upload failed: ${res.status}`);
    const body = await res.json() as { root: string };
    return body.root;
  }

  async deleteFile(path: string): Promise<void> {
    const res = await fetch(`/api/upload?path=${encodeURIComponent(path)}`, { method: "DELETE" });
    if (!res.ok && res.status !== 204) throw new Error(`Delete failed: ${res.status}`);
  }

  async deleteAll(): Promise<void> {
    const res = await fetch("/api/upload/all", { method: "DELETE" });
    if (!res.ok && res.status !== 204) throw new Error(`Delete all failed: ${res.status}`);
  }
}
