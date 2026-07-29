import type {
  EmbedDone,
  EmbedError,
  EmbedProgress,
  EmbeddingEngine,
  Bookmark,
  BookmarkClustersQuery,
  BookmarkClustersResult,
  FileListChanged,
  FileListResponse,
  FileMatches,
  FileMetadataUpdate,
  GenerationDone,
  GenerationError,
  IndexStatus,
  AddOutcome,
  CitationResult,
  IntegrationStatus,
  MatchRef,
  DocumentMetadata,
  ModelDescriptor,
  NewBookmark,
  OpenAlexWork,
  PreviewData,
  RelatedDocument,
  RelatedDocumentsQuery,
  SelectedEmbedder,
  SemanticScholarPaper,
  SearchQuery,
  SearchStats,
  Settings,
  Tag,
  NewTag,
  DocumentTagUpdate,
  SmartCollection,
  NewSmartCollection,
  CollectionValidation,
  SearchLogEntry,
  BookmarkClusterLabelled,
  GeneratorDescriptor,
  GenerationStreamEvent,
} from "../lib/types";
import { randomId } from "../lib/types";
import type { SearchApi, WebSourceApi } from "./api";

export class HttpSearchApi implements SearchApi {
  private controllers = new Map<string, AbortController>();

  async search(
    query: SearchQuery,
    onResult: (fm: FileMatches) => void,
    onComplete: (stats: SearchStats) => void,
  ): Promise<string> {
    const searchId = randomId();
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

  async relatedDocuments(query: RelatedDocumentsQuery): Promise<RelatedDocument[]> {
    const res = await fetch("/api/related-documents", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(query),
    });
    if (!res.ok) throw new Error(`relatedDocuments failed: ${res.status}`);
    return res.json() as Promise<RelatedDocument[]>;
  }

  async clusterBookmarks(query: BookmarkClustersQuery): Promise<BookmarkClustersResult> {
    const res = await fetch("/api/bookmarks/clusters", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(query),
    });
    if (!res.ok) throw new Error(`clusterBookmarks failed: ${res.status}`);
    return res.json() as Promise<BookmarkClustersResult>;
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

  async listBookmarks(): Promise<Bookmark[]> {
    const res = await fetch("/api/bookmarks");
    if (!res.ok) throw new Error(`listBookmarks failed: ${res.status}`);
    return res.json() as Promise<Bookmark[]>;
  }

  async addBookmark(bookmark: NewBookmark): Promise<Bookmark> {
    const res = await fetch("/api/bookmarks", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(bookmark),
    });
    if (!res.ok) throw new Error(`addBookmark failed: ${res.status}`);
    return res.json() as Promise<Bookmark>;
  }

  async removeBookmark(id: string): Promise<void> {
    const res = await fetch(`/api/bookmarks/${encodeURIComponent(id)}`, { method: "DELETE" });
    if (!res.ok && res.status !== 204) throw new Error(`removeBookmark failed: ${res.status}`);
  }

  async updateBookmarkNote(id: string, note: string | null): Promise<Bookmark> {
    const res = await fetch(`/api/bookmarks/${encodeURIComponent(id)}`, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ note }),
    });
    if (!res.ok) throw new Error(`updateBookmarkNote failed: ${res.status}`);
    return res.json() as Promise<Bookmark>;
  }

  async listFiles(root: string, collectionId?: string | null, tagIds: string[] = [], collectionExpression?: string | null): Promise<FileListResponse> {
    const query = new URLSearchParams({ root });
    if (collectionId) query.set("collection_id", collectionId);
    if (tagIds.length) query.set("tag_ids", tagIds.join(","));
    if (collectionExpression) query.set("collection_expression", collectionExpression);
    const res = await fetch(`/api/files?${query.toString()}`);
    if (!res.ok) throw new Error(`listFiles failed: ${res.status}`);
    return res.json() as Promise<FileListResponse>;
  }

  async listTags(): Promise<Tag[]> { return this.json<Tag[]>("/api/tags"); }
  async createTag(tag: NewTag): Promise<Tag> { return this.json<Tag>("/api/tags", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(tag) }); }
  async updateTag(id: string, tag: NewTag): Promise<Tag> { return this.json<Tag>(`/api/tags/${encodeURIComponent(id)}`, { method: "PATCH", headers: { "Content-Type": "application/json" }, body: JSON.stringify(tag) }); }
  async deleteTag(id: string): Promise<void> { await this.ok(`/api/tags/${encodeURIComponent(id)}`, { method: "DELETE" }); }
  async updateDocumentTags(update: DocumentTagUpdate): Promise<void> { await this.ok("/api/documents/tags", { method: "PATCH", headers: { "Content-Type": "application/json" }, body: JSON.stringify(update) }); }
  async listSmartCollections(): Promise<SmartCollection[]> { return this.json<SmartCollection[]>("/api/smart-collections"); }
  async createSmartCollection(collection: NewSmartCollection): Promise<SmartCollection> { return this.json<SmartCollection>("/api/smart-collections", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(collection) }); }
  async updateSmartCollection(id: string, collection: NewSmartCollection): Promise<SmartCollection> { return this.json<SmartCollection>(`/api/smart-collections/${encodeURIComponent(id)}`, { method: "PATCH", headers: { "Content-Type": "application/json" }, body: JSON.stringify(collection) }); }
  async deleteSmartCollection(id: string): Promise<void> { await this.ok(`/api/smart-collections/${encodeURIComponent(id)}`, { method: "DELETE" }); }
  async validateSmartCollection(expression: string): Promise<CollectionValidation> { return this.json<CollectionValidation>("/api/smart-collections/validate", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ expression }) }); }
  async listSearchLog(limit = 100): Promise<SearchLogEntry[]> { return this.json<SearchLogEntry[]>(`/api/search-log?limit=${limit}`); }
  async deleteSearchLog(id: string): Promise<void> { await this.ok(`/api/search-log/${encodeURIComponent(id)}`, { method: "DELETE" }); }
  async clearSearchLog(): Promise<void> { await this.ok("/api/search-log", { method: "DELETE" }); }

  private async json<T>(url: string, init?: RequestInit): Promise<T> {
    const response = await fetch(url, init);
    if (!response.ok) throw new Error(`${url} failed: ${response.status}`);
    return response.json() as Promise<T>;
  }

  private async ok(url: string, init?: RequestInit): Promise<void> {
    const response = await fetch(url, init);
    if (!response.ok) throw new Error(`${url} failed: ${response.status}`);
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

  async renameFile(path: string, newName: string): Promise<string> {
    const res = await fetch("/api/file/rename", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path, new_name: newName }),
    });
    if (!res.ok) throw new Error(`renameFile failed: ${res.status}`);
    return res.json() as Promise<string>;
  }

  async getFileMetadata(path: string): Promise<DocumentMetadata> {
    const res = await fetch("/api/file/metadata", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path }),
    });
    if (!res.ok) throw new Error(`getFileMetadata failed: ${res.status}`);
    return res.json() as Promise<DocumentMetadata>;
  }

  async resolveFileMetadata(path: string): Promise<DocumentMetadata> {
    const res = await fetch("/api/file/metadata/resolve", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path }),
    });
    if (!res.ok) throw new Error(`resolveFileMetadata failed: ${res.status}`);
    return res.json() as Promise<DocumentMetadata>;
  }

  async refreshFileMetadata(path?: string): Promise<void> {
    const init: RequestInit = path
      ? {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ path }),
        }
      : { method: "POST" };
    const res = await fetch("/api/file/metadata/refresh", init);
    if (!res.ok) throw new Error(`refreshFileMetadata failed: ${res.status}`);
  }

  async zoteroStatus(): Promise<IntegrationStatus> {
    const res = await fetch("/api/integrations/zotero/status");
    if (!res.ok) throw new Error(`zoteroStatus failed: ${res.status}`);
    return res.json() as Promise<IntegrationStatus>;
  }

  async zoteroAddItem(path: string): Promise<AddOutcome> {
    const res = await fetch("/api/integrations/zotero/add", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path }),
    });
    if (!res.ok) throw new Error(`zoteroAddItem failed: ${res.status}`);
    return res.json() as Promise<AddOutcome>;
  }

  async zoteroGenerateCitation(path: string): Promise<CitationResult> {
    const res = await fetch("/api/integrations/zotero/citation", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path }),
    });
    if (!res.ok) throw new Error(`zoteroGenerateCitation failed: ${res.status}`);
    return res.json() as Promise<CitationResult>;
  }

  async semanticScholarStatus(): Promise<IntegrationStatus> {
    const res = await fetch("/api/integrations/semantic-scholar/status");
    if (!res.ok) throw new Error(`semanticScholarStatus failed: ${res.status}`);
    return res.json() as Promise<IntegrationStatus>;
  }

  async semanticScholarLookup(doi: string): Promise<SemanticScholarPaper> {
    const res = await fetch("/api/integrations/semantic-scholar/lookup", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ doi }),
    });
    if (!res.ok) throw new Error(`semanticScholarLookup failed: ${res.status}`);
    return res.json() as Promise<SemanticScholarPaper>;
  }

  async openAlexStatus(): Promise<IntegrationStatus> {
    const res = await fetch("/api/integrations/openalex/status");
    if (!res.ok) throw new Error(`openAlexStatus failed: ${res.status}`);
    return res.json() as Promise<IntegrationStatus>;
  }

  async openAlexLookup(doi: string): Promise<OpenAlexWork> {
    const res = await fetch("/api/integrations/openalex/lookup", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ doi }),
    });
    if (!res.ok) throw new Error(`openAlexLookup failed: ${res.status}`);
    return res.json() as Promise<OpenAlexWork>;
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

  async openPath(path: string): Promise<void> {
    if (path.startsWith("http://") || path.startsWith("https://")) {
      window.open(path, "_blank", "noopener,noreferrer");
    }
    // Opening local filesystem paths in the OS's file manager is not possible in browser mode.
    // No endpoint exists for this in the server, so we just return.
    return;
  }

  async revealPath(_path: string): Promise<void> {
    // Revealing local filesystem paths in the OS's file manager is not possible in browser mode.
    return;
  }

  async writeClipboard(text: string): Promise<void> {
    if (!navigator.clipboard?.writeText) {
      throw new Error("Clipboard API unavailable");
    }
    await navigator.clipboard.writeText(text);
  }

  // ── Worker Management ────────────────────────────────────────────────────────

  async getWorkerStatus(): Promise<import("../lib/types").WorkerStatus> {
    const res = await fetch("/api/worker/status");
    if (!res.ok) throw new Error(`getWorkerStatus failed: ${res.status}`);
    return res.json() as Promise<import("../lib/types").WorkerStatus>;
  }

  async getWorkerStatuses(): Promise<import("../lib/types").WorkerStatus[]> {
    const res = await fetch("/api/worker/statuses");
    if (!res.ok) throw new Error(`getWorkerStatuses failed: ${res.status}`);
    return res.json() as Promise<import("../lib/types").WorkerStatus[]>;
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

  async downloadModel(selected: SelectedEmbedder): Promise<void> {
    const res = await fetch("/api/embed/download", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ selected }),
    });
    if (!res.ok && res.status !== 202) throw new Error(`downloadModel failed: ${res.status}`);
  }

  async buildIndex(root: string, selected: SelectedEmbedder): Promise<void> {
    const res = await fetch("/api/embed/build", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ root, selected }),
    });
    if (!res.ok && res.status !== 202) throw new Error(`buildIndex failed: ${res.status}`);
  }

  async cancelEmbed(): Promise<void> {
    const res = await fetch("/api/embed/cancel", { method: "DELETE" });
    if (!res.ok && res.status !== 204) throw new Error(`cancelEmbed failed: ${res.status}`);
  }

  async getIndexStatus(root?: string): Promise<IndexStatus> {
    const query = root ? `?root=${encodeURIComponent(root)}` : "";
    const res = await fetch(`/api/embed/status${query}`);
    if (!res.ok) throw new Error(`getIndexStatus failed: ${res.status}`);
    return res.json() as Promise<IndexStatus>;
  }

  async deleteIndex(root?: string): Promise<void> {
    const query = root ? `?root=${encodeURIComponent(root)}` : "";
    const res = await fetch(`/api/embed/index${query}`, { method: "DELETE" });
    if (!res.ok && res.status !== 204) throw new Error(`deleteIndex failed: ${res.status}`);
  }

  // Every server-pushed app event shares one EventSource. A refcount opens the
  // connection for the first subscriber and closes it after the last.
  private eventSource: EventSource | null = null;
  private eventSourceRefs = 0;

  private acquireEventSource(): EventSource {
    if (!this.eventSource) {
      this.eventSource = new EventSource("/api/events");
    }
    this.eventSourceRefs++;
    return this.eventSource;
  }

  private releaseEventSource(eventName: string, listener: (e: any) => void): void {
    if (this.eventSource) {
      this.eventSource.removeEventListener(eventName, listener);
    }
    this.eventSourceRefs--;
    if (this.eventSourceRefs <= 0) {
      this.eventSource?.close();
      this.eventSource = null;
      this.eventSourceRefs = 0;
    }
  }

  async onGenerationProgress(handler: (p: EmbedProgress) => void): Promise<() => void> {
    const es = this.acquireEventSource();
    const listener = (e: any) => handler(JSON.parse(e.data));
    es.addEventListener("generation-progress", listener);
    return () => this.releaseEventSource("generation-progress", listener);
  }

  async onGenerationDone(handler: (d: GenerationDone) => void): Promise<() => void> {
    const es = this.acquireEventSource();
    const listener = (e: any) => handler(JSON.parse(e.data));
    es.addEventListener("generation-done", listener);
    return () => this.releaseEventSource("generation-done", listener);
  }

  async onGenerationError(handler: (e: GenerationError) => void): Promise<() => void> {
    const es = this.acquireEventSource();
    const listener = (e: any) => handler(JSON.parse(e.data));
    es.addEventListener("generation-error", listener);
    return () => this.releaseEventSource("generation-error", listener);
  }

  async onEmbedProgress(handler: (p: EmbedProgress) => void): Promise<() => void> {
    const es = this.acquireEventSource();
    const listener = (e: any) => handler(JSON.parse(e.data));
    es.addEventListener("embed-progress", listener);
    return () => this.releaseEventSource("embed-progress", listener);
  }

  async onEmbedDone(handler: (d: EmbedDone) => void): Promise<() => void> {
    const es = this.acquireEventSource();
    const listener = (e: any) => handler(JSON.parse(e.data));
    es.addEventListener("embed-done", listener);
    return () => this.releaseEventSource("embed-done", listener);
  }

  async onEmbedError(handler: (e: EmbedError) => void): Promise<() => void> {
    const es = this.acquireEventSource();
    const listener = (e: any) => handler(JSON.parse(e.data));
    es.addEventListener("embed-error", listener);
    return () => this.releaseEventSource("embed-error", listener);
  }

  async onManagerEvent(handler: (event: string) => void): Promise<() => void> {
    const es = this.acquireEventSource();
    const listener = (e: any) => handler(JSON.parse(e.data));
    es.addEventListener("manager-event", listener);
    return () => this.releaseEventSource("manager-event", listener);
  }

  async onFileListChanged(
    handler: (event: FileListChanged) => void,
  ): Promise<() => void> {
    const es = this.acquireEventSource();
    const listener = (e: any) => handler(JSON.parse(e.data));
    es.addEventListener("file-list-changed", listener);
    return () => this.releaseEventSource("file-list-changed", listener);
  }

  async onFileMetadataUpdated(
    handler: (updates: FileMetadataUpdate[]) => void,
  ): Promise<() => void> {
    const es = this.acquireEventSource();
    const listener = (e: any) => handler(JSON.parse(e.data));
    es.addEventListener("file-metadata-updated", listener);
    return () => this.releaseEventSource("file-metadata-updated", listener);
  }

  // ── Generation commands ────────────────────────────────────────────────────

  async isGenerationReady(): Promise<boolean> {
    const res = await fetch("/api/generation/ready");
    if (!res.ok) throw new Error(`isGenerationReady failed: ${res.status}`);
    return res.json() as Promise<boolean>;
  }

  async listGenerationModels(): Promise<GeneratorDescriptor[]> {
    const res = await fetch("/api/generation/models");
    if (!res.ok) throw new Error(`listGenerationModels failed: ${res.status}`);
    return res.json() as Promise<GeneratorDescriptor[]>;
  }

  async getGenerationModelSize(modelId: string): Promise<number> {
    const res = await fetch(
      `/api/generation/models/size?model_id=${encodeURIComponent(modelId)}`,
    );
    if (!res.ok) throw new Error(`getGenerationModelSize failed: ${res.status}`);
    return res.json() as Promise<number>;
  }

  async loadGenerationModel(): Promise<boolean> {
    const res = await fetch("/api/generation/load", { method: "POST" });
    if (!res.ok) throw new Error(`loadGenerationModel failed: ${res.status}`);
    return res.json() as Promise<boolean>;
  }

  async explainRelatedDocument(
    requestId: string,
    anchorPath: string,
    path: string,
  ): Promise<void> {
    const res = await fetch("/api/generation/explain-related", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ request_id: requestId, anchor_path: anchorPath, path }),
    });
    if (!res.ok) throw new Error(`explainRelatedDocument failed: ${res.status}`);
  }

  async summarizeDocument(requestId: string, path: string): Promise<void> {
    const res = await fetch("/api/generation/summarize", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ request_id: requestId, path }),
    });
    if (!res.ok) throw new Error(`summarizeDocument failed: ${res.status}`);
  }

  async onBookmarkClusterLabelled(
    handler: (event: BookmarkClusterLabelled) => void,
  ): Promise<() => void> {
    const es = this.acquireEventSource();
    const listener = (e: any) => handler(JSON.parse(e.data));
    es.addEventListener("bookmark-cluster-labelled", listener);
    return () => this.releaseEventSource("bookmark-cluster-labelled", listener);
  }

  async onGenerationStream(
    handler: (event: GenerationStreamEvent) => void,
  ): Promise<() => void> {
    const es = this.acquireEventSource();
    const listener = (e: any) => handler(JSON.parse(e.data));
    es.addEventListener("generation-stream", listener);
    return () => this.releaseEventSource("generation-stream", listener);
  }
}

export class HttpSourceApi implements WebSourceApi {
  type = "web" as const;
  deletionKind = "permanent" as const;

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
