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

  async isSemanticReady(): Promise<boolean> {
    const res = await fetch("/api/embed/ready");
    if (!res.ok) throw new Error(`isSemanticReady failed: ${res.status}`);
    return res.json() as Promise<boolean>;
  }

  async getLogs(): Promise<string[]> {
    const res = await fetch("/api/logs");
    if (!res.ok) throw new Error(`getLogs failed: ${res.status}`);
    return res.json() as Promise<string[]>;
  }

  async clearLogs(): Promise<void> {
    const res = await fetch("/api/logs", { method: "DELETE" });
    if (!res.ok && res.status !== 204) throw new Error(`clearLogs failed: ${res.status}`);
  }

  async getPythonInfo(): Promise<string> {
    const res = await fetch("/api/worker/python-info");
    if (!res.ok) throw new Error(`getPythonInfo failed: ${res.status}`);
    return res.json() as Promise<string>;
  }

  async getSupportedEngines(): Promise<EmbeddingEngine[]> {
    const res = await fetch("/api/embed/engines");
    if (!res.ok) throw new Error(`getSupportedEngines failed: ${res.status}`);
    return res.json() as Promise<EmbeddingEngine[]>;
  }

  async getDataPaths(): Promise<any> {
    const res = await fetch("/api/data/paths");
    if (!res.ok) throw new Error(`getDataPaths failed: ${res.status}`);
    return res.json();
  }

  async openPath(_path: string): Promise<void> {
    // Opening paths in the OS's file manager is not possible in browser mode.
    // No endpoint exists for this in the server, so we just return.
    return;
  }

  // ── Worker Management ────────────────────────────────────────────────────────

  async getWorkerStatus(): Promise<import("../lib/types").WorkerStatus> {
    const res = await fetch("/api/worker/status");
    if (!res.ok) throw new Error(`getWorkerStatus failed: ${res.status}`);
    return res.json() as Promise<import("../lib/types").WorkerStatus>;
  }

  async killWorker(): Promise<void> {
    const res = await fetch("/api/worker/kill", { method: "POST" });
    if (!res.ok && res.status !== 204) throw new Error(`killWorker failed: ${res.status}`);
  }

  async setWorkerTimeout(secs: number): Promise<void> {
    const res = await fetch("/api/worker/timeout", {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ secs }),
    });
    if (!res.ok && res.status !== 204) throw new Error(`setWorkerTimeout failed: ${res.status}`);
  }

  // ── Semantic / embed commands ──────────────────────────────────────────────

  async listModels(engine: EmbeddingEngine): Promise<ModelDescriptor[]> {
    const res = await fetch(`/api/embed/models?engine=${encodeURIComponent(engine)}`);
    if (!res.ok) throw new Error(`listModels failed: ${res.status}`);
    return res.json() as Promise<ModelDescriptor[]>;
  }

  async getModelSize(engine: EmbeddingEngine, modelId: string): Promise<number> {
    const res = await fetch(`/api/embed/model-size?engine=${encodeURIComponent(engine)}&model_id=${encodeURIComponent(modelId)}`);
    if (!res.ok) throw new Error(`getModelSize failed: ${res.status}`);
    return res.json() as Promise<number>;
  }

  async downloadModel(model: EmbedderModel, engine: EmbeddingEngine): Promise<void> {
    const res = await fetch("/api/embed/download", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model, engine }),
    });
    if (!res.ok && res.status !== 202) throw new Error(`downloadModel failed: ${res.status}`);
  }

  async buildIndex(root: string, model: EmbedderModel, engine: EmbeddingEngine): Promise<void> {
    const res = await fetch("/api/embed/build", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ root, model, engine }),
    });
    if (!res.ok && res.status !== 202) throw new Error(`buildIndex failed: ${res.status}`);
  }

  async cancelEmbed(): Promise<void> {
    const res = await fetch("/api/embed/cancel", { method: "DELETE" });
    if (!res.ok && res.status !== 204) throw new Error(`cancelEmbed failed: ${res.status}`);
  }

  async getIndexStatus(): Promise<IndexStatus> {
    const res = await fetch("/api/embed/status");
    if (!res.ok) throw new Error(`getIndexStatus failed: ${res.status}`);
    return res.json() as Promise<IndexStatus>;
  }

  async deleteIndex(): Promise<void> {
    const res = await fetch("/api/embed/index", { method: "DELETE" });
    if (!res.ok && res.status !== 204) throw new Error(`deleteIndex failed: ${res.status}`);
  }

  async onEmbedProgress(handler: (p: EmbedProgress) => void): Promise<() => void> {
    const eventSource = new EventSource("/api/embed/events");
    eventSource.addEventListener("embed-progress", (e: any) => {
      handler(JSON.parse(e.data));
    });
    return () => eventSource.close();
  }

  async onEmbedDone(handler: (d: EmbedDone) => void): Promise<() => void> {
    const eventSource = new EventSource("/api/embed/events");
    eventSource.addEventListener("embed-done", (e: any) => {
      handler(JSON.parse(e.data));
    });
    return () => eventSource.close();
  }

  async onEmbedError(handler: (e: EmbedError) => void): Promise<() => void> {
    const eventSource = new EventSource("/api/embed/events");
    eventSource.addEventListener("embed-error", (e: any) => {
      handler(JSON.parse(e.data));
    });
    return () => eventSource.close();
  }

  async onManagerEvent(handler: (event: string) => void): Promise<() => void> {
    const eventSource = new EventSource("/api/embed/events");
    eventSource.addEventListener("manager-event", (e: any) => {
      handler(JSON.parse(e.data));
    });
    return () => eventSource.close();
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
