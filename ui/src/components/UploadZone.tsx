import { useCallback, useEffect, useRef, useState } from "react";
import type { FileEntry } from "../lib/types";
import type { SearchApi, WebSourceApi } from "../services/api";

interface Props {
  source: WebSourceApi;
  api: SearchApi;
  root: string;
  onRootChange: (root: string) => void;
}

interface UploadProgress {
  loaded: number;
  total: number;
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
        reject(new Error(`Upload failed: ${xhr.status}`));
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

export default function UploadZone({ source, api, root, onRootChange }: Props) {
  const [fileList, setFileList] = useState<FileEntry[]>([]);
  const [uploading, setUploading] = useState(false);
  const [progress, setProgress] = useState<UploadProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [dragOver, setDragOver] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const dirInputRef = useRef<HTMLInputElement>(null);

  // Load existing files if root is already set
  useEffect(() => {
    if (!root) return;
    api.listFiles(root).then(setFileList).catch((e) => console.error("listFiles error:", e));
  }, [root, api]);

  const handleUpload = useCallback(async (files: File[]) => {
    if (files.length === 0) return;
    setUploading(true);
    setProgress(null);
    setError(null);

    try {
      const result = await uploadWithProgress(files, setProgress);
      onRootChange(result.root);
      const updated = await api.listFiles(result.root);
      setFileList(updated);
    } catch (e) {
      console.error("Upload error:", e);
      setError(e instanceof Error ? e.message : "Upload failed");
    } finally {
      setUploading(false);
      setProgress(null);
    }
  }, [api, onRootChange]);

  const handleDrop = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(false);
    const files = Array.from(e.dataTransfer.files);
    handleUpload(files);
  }, [handleUpload]);

  const handleDeleteFile = useCallback(async (filePath: string) => {
    try {
      await source.deleteFile(filePath);
      setFileList((prev) => prev.filter((f) => f.path !== filePath));
    } catch (e) {
      console.error("Delete error:", e);
    }
  }, [source]);

  const handleDeleteAll = useCallback(async () => {
    try {
      await source.deleteAll();
      setFileList([]);
    } catch (e) {
      console.error("Delete all error:", e);
    }
  }, [source]);

  const pct = progress && progress.total > 0
    ? Math.round((progress.loaded / progress.total) * 100)
    : null;

  return (
    <div className="flex items-center gap-2 min-w-0">
      {/* Drop zone / upload trigger */}
      <div
        onDrop={handleDrop}
        onDragOver={(e) => { e.preventDefault(); setDragOver(true); }}
        onDragLeave={() => setDragOver(false)}
        className={`flex items-center gap-1 text-xs rounded px-2 py-1 border transition-colors cursor-pointer ${
          dragOver
            ? "border-blue-500 bg-blue-950 text-blue-300"
            : "border-neutral-700 bg-neutral-800 text-neutral-400 hover:text-neutral-100 hover:border-neutral-500"
        }`}
      >
        {uploading ? (
          <span className="animate-pulse">
            {pct !== null ? `Uploading ${pct}%…` : "Uploading…"}
          </span>
        ) : (
          <>
            <button
              onClick={() => fileInputRef.current?.click()}
              title="Upload files"
              className="hover:text-white"
            >
              Files
            </button>
            <span className="text-neutral-600">/</span>
            <button
              onClick={() => dirInputRef.current?.click()}
              title="Upload directory"
              className="hover:text-white"
            >
              Folder
            </button>
            {fileList.length > 0 && (
              <span className="text-neutral-500 ml-1">{fileList.length} files</span>
            )}
          </>
        )}
      </div>

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
