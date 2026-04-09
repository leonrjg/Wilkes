import { useCallback, useRef, useState } from "react";
import { File, Folder } from "react-feather";
import type { WebSourceApi } from "../services/api";
import { useSettingsStore } from "../stores/useSettingsStore";

interface Props {
  source: WebSourceApi;
  onRootChange: (root: string) => void;
}

interface UploadProgress {
  loaded: number;
  total: number;
}

function formatUploadError(status: number, responseText: string): string {
  const trimmed = responseText.trim();
  if (!trimmed) return `Upload failed: ${status}`;

  try {
    const parsed = JSON.parse(trimmed) as { error?: string };
    if (parsed.error) return `Upload failed: ${status} (${parsed.error})`;
  } catch {
    // Fall through to plain-text response handling.
  }

  return `Upload failed: ${status} (${trimmed})`;
}

function uploadWithProgress(
  files: File[],
  onProgress: (p: UploadProgress) => void,
): Promise<{ root: string; file_count: number }> {
  return new Promise((resolve, reject) => {
    const formData = new FormData();
    for (const file of files) {
      const name = (file as File & { webkitRelativePath?: string }).webkitRelativePath || file.name;
      formData.append("files", file, name);
    }

    const xhr = new XMLHttpRequest();
    xhr.open("POST", "/api/upload");

    xhr.upload.addEventListener("progress", (e) => {
      if (e.lengthComputable) onProgress({ loaded: e.loaded, total: e.total });
    });

    xhr.addEventListener("load", () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        try {
          resolve(JSON.parse(xhr.responseText) as { root: string; file_count: number });
        } catch {
          reject(new Error("Invalid upload response"));
        }
      } else {
        reject(new Error(formatUploadError(xhr.status, xhr.responseText ?? "")));
      }
    });

    xhr.addEventListener("error", () => reject(new Error("Upload network error")));
    xhr.addEventListener("abort", () => reject(new Error("Upload aborted")));
    xhr.send(formData);
  });
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export default function UploadZone({ source, onRootChange }: Props) {
  const fileList = useSettingsStore((s) => s.fileList);
  const refreshFileList = useSettingsStore((s) => s.refreshFileList);
  const preferSemantic = useSettingsStore((s) => s.preferSemantic);
  const startSemanticIndex = useSettingsStore((s) => s.startSemanticIndex);
  const [uploading, setUploading] = useState(false);
  const [progress, setProgress] = useState<UploadProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [dragOver, setDragOver] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const dirInputRef = useRef<HTMLInputElement>(null);

  const handleUpload = useCallback(async (files: File[]) => {
    if (files.length === 0) return;
    setUploading(true);
    setProgress(null);
    setError(null);

    try {
      const result = await uploadWithProgress(files, setProgress);
      onRootChange(result.root);
      if (preferSemantic) {
        startSemanticIndex().catch((e) => {
          console.error("Semantic reindex after upload failed:", e);
        });
      }
    } catch (e) {
      console.error("Upload error:", e);
      setError(e instanceof Error ? e.message : "Upload failed");
    } finally {
      setUploading(false);
      setProgress(null);
    }
  }, [onRootChange, preferSemantic, startSemanticIndex]);

  const handleDrop = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(false);
    const files = Array.from(e.dataTransfer.files);
    handleUpload(files);
  }, [handleUpload]);

  const handleDeleteFile = useCallback(async (filePath: string) => {
    try {
      await source.deleteFile(filePath);
      refreshFileList();
    } catch (e) {
      console.error("Delete error:", e);
    }
  }, [source, refreshFileList]);

  const handleDeleteAll = useCallback(async () => {
    try {
      await source.deleteAll();
      refreshFileList();
    } catch (e) {
      console.error("Delete all error:", e);
    }
  }, [source, refreshFileList]);

  const pct = progress && progress.total > 0
    ? Math.round((progress.loaded / progress.total) * 100)
    : null;

  return (
    <div className="flex items-center gap-2 min-w-0">
      {/* Drop zone / upload trigger */}
      {uploading ? (
        <span className="text-xs text-[var(--text-muted)] animate-pulse">
          {pct !== null ? `Uploading ${pct}%…` : "Uploading…"}
        </span>
      ) : (
        <div
          onDrop={handleDrop}
          onDragOver={(e) => { e.preventDefault(); setDragOver(true); }}
          onDragLeave={() => setDragOver(false)}
          className={`flex items-center gap-0.5 rounded transition-colors ${dragOver ? "ring-1 ring-[var(--accent-blue)]" : ""}`}
        >
          <button
            onClick={() => fileInputRef.current?.click()}
            title="Upload files"
            className="flex items-center gap-1.5 text-xs text-[var(--text-muted)] hover:text-[var(--text-main)] bg-[var(--bg-active)] hover:bg-[var(--bg-hover)] px-2 py-1 rounded-l border border-[var(--border-main)] hover:border-[var(--border-strong)] transition-colors"
          >
            <File size={11} />
            <span>Files</span>
          </button>
          <button
            onClick={() => dirInputRef.current?.click()}
            title="Upload folder"
            className="flex items-center gap-1.5 text-xs text-[var(--text-muted)] hover:text-[var(--text-main)] bg-[var(--bg-active)] hover:bg-[var(--bg-hover)] px-2 py-1 rounded-r border border-l-0 border-[var(--border-main)] hover:border-[var(--border-strong)] transition-colors"
          >
            <Folder size={11} />
            <span>Folder</span>
          </button>
        </div>
      )}

      <input
        ref={fileInputRef}
        type="file"
        multiple
        className="hidden"
        onChange={(e) => handleUpload(Array.from(e.target.files ?? []))}
      />
      <input
        ref={dirInputRef}
        type="file"
        // @ts-expect-error webkitdirectory is not in the standard TS types
        webkitdirectory=""
        multiple
        className="hidden"
        onChange={(e) => handleUpload(Array.from(e.target.files ?? []))}
      />

      {error && (
        <span className="text-xs text-red-400">{error}</span>
      )}

      {fileList.length > 0 && !uploading && (
        <div className="flex items-center gap-1">
          <div className="max-h-[200px] overflow-y-auto flex flex-col gap-0.5 hidden">
            {fileList.map((f) => (
              <div key={f.path} className="flex items-center gap-1 text-xs text-neutral-400">
                <span className="truncate max-w-[200px]" title={f.path}>
                  {f.path.split(/[/\\]/).pop()}
                </span>
                <span className="text-neutral-600">{formatBytes(f.size_bytes)}</span>
                <button
                  onClick={() => handleDeleteFile(f.path)}
                  className="text-neutral-600 hover:text-red-400 ml-1"
                  title="Remove file"
                >
                  ×
                </button>
              </div>
            ))}
          </div>
          <button
            onClick={handleDeleteAll}
            title="Clear all uploaded files"
            className="text-xs text-neutral-600 hover:text-red-400 px-1"
          >
            Clear all
          </button>
        </div>
      )}
    </div>
  );
}
