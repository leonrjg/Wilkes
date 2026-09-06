import { useState, useEffect } from "react";
import type { SearchApi, DataPaths } from "../services/api";
import { isTauri } from "../services";
import { useSemanticStore } from "../stores/useSemanticStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useActiveWorkspaceReadOnly, useWorkspaceStore } from "../stores/useWorkspaceStore";
import { confirmDialog } from "../lib/utils/dialog";
import { Tooltip } from "@leonrjg/wilkes-reader";

interface Props {
  api: SearchApi;
  isActive: boolean;
}

export default function DataPanel({ api, isActive }: Props) {
  const [paths, setPaths] = useState<DataPaths | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isDeleting, setIsDeleting] = useState(false);
  const [deletingWorkspaceId, setDeletingWorkspaceId] = useState<string | null>(null);
  const indexStatus = useSemanticStore((s) => s.indexStatus);
  const refreshCurrentRootStatus = useSemanticStore((s) => s.refreshCurrentRootStatus);
  const handleCurrentRootIndexRemoved = useSemanticStore((s) => s.handleCurrentRootIndexRemoved);
  const directory = useSettingsStore((s) => s.directory);
  const readOnly = useActiveWorkspaceReadOnly();
  const workspaces = useWorkspaceStore((s) => s.workspaces);
  const activeWorkspaceId = useWorkspaceStore((s) => s.activeWorkspaceId);
  const removeWorkspace = useWorkspaceStore((s) => s.remove);
  const refreshWorkspaces = useWorkspaceStore((s) => s.refreshList);

  const fetchPaths = async () => {
    try {
      const p = await api.getDataPaths();
      setPaths(p);
    } catch (e: any) {
      setError(e.toString());
    }
  };

  useEffect(() => {
    fetchPaths();
  }, [api]);

  useEffect(() => {
    if (!isActive) return;
    setError(null);
    fetchPaths();
    // The registry is read again rather than trusted from startup: a managed
    // corpus appears when its application asks for one, which can happen at
    // any moment while this window is open, and a list that missed it would
    // be a list of the workspaces there used to be.
    refreshWorkspaces().catch((e) => {
      setError(e?.message ?? e?.toString?.() ?? "Failed to list workspaces");
    });
    refreshCurrentRootStatus().catch((e) => {
      setError(e?.toString?.() ?? "Failed to refresh semantic index status");
    });
  }, [isActive, api, refreshCurrentRootStatus, refreshWorkspaces]);

  const onOpen = (path: string) => {
    api.openPath(path).catch((e) => setError(e.toString()));
  };

  const onDeleteIndex = async () => {
    if (!await confirmDialog("Are you sure you want to delete the semantic index for the current directory? This cannot be undone and will require a reindex for this directory.")) {
      return;
    }
    setIsDeleting(true);
    try {
      await api.deleteIndex(directory || undefined);
      await handleCurrentRootIndexRemoved();
      await refreshCurrentRootStatus();
      await fetchPaths();
    } catch (e: any) {
      setError(e.toString());
    } finally {
      setIsDeleting(false);
    }
  };

  // Deleting a workspace is offered whether it is managed or not. Read-only
  // protects a managed corpus's *content* — only its owning application may
  // write documents into it — and that is a different question from whether
  // the user may stop hosting it at all, which is theirs alone: the bytes are
  // on their disk.
  const onDeleteWorkspace = async (id: string, name: string, managedBy: string | null) => {
    const owner = managedBy
      ? `\n\nThis workspace is managed by ${managedBy}. Deleting it removes that application's corpus from this machine; the application will build a new one the next time it asks for it.`
      : "";
    if (!await confirmDialog(
      `Delete the workspace "${name}"?\n\nIts semantic index, settings, bookmarks, research and uploaded files are deleted permanently. Documents in the folders it indexed are not touched.${owner}`,
    )) {
      return;
    }
    setDeletingWorkspaceId(id);
    try {
      // Deleting the active workspace activates another one first, so the
      // paths and the index status on this page are no longer the ones on
      // screen.
      if (await removeWorkspace(id)) {
        await fetchPaths();
        await refreshCurrentRootStatus();
      }
    } catch (e: any) {
      setError(e?.message ?? e.toString());
    } finally {
      setDeletingWorkspaceId(null);
    }
  };

  const formatBytes = (bytes: number): string => {
    if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(2)} GB`;
    if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(2)} MB`;
    return `${Math.round(bytes / 1024)} KB`;
  };

  if (error) {
    return (
      <div className="p-4 bg-red-900/20 border border-red-900/50 rounded-lg">
        <p className="text-xs text-red-400 leading-relaxed">{error}</p>
        <button 
          onClick={() => {
            setError(null);
            fetchPaths();
            refreshCurrentRootStatus().catch((e) => {
              setError(e?.toString?.() ?? "Failed to refresh semantic index status");
            });
          }}
          className="mt-2 text-[10px] text-red-400 underline hover:text-red-300"
        >
          Try again
        </button>
      </div>
    );
  }

  if (!paths) {
    return (
      <div className="flex items-center justify-center h-32">
        <div className="w-5 h-5 border-2 border-[var(--accent-blue)] border-t-transparent rounded-full animate-spin" />
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-6">
      <section>
        <div className="flex flex-col gap-1.5 mb-4">
          <h3 className="text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider">Semantic Index Database</h3>
          <p className="text-[11px] text-[var(--text-muted)] leading-relaxed">
            Stores chunks and vector embeddings for semantic search.
          </p>
        </div>

        <div className="p-3 bg-[var(--bg-active)] rounded-lg border border-[var(--border-main)] flex flex-col gap-3">
          {indexStatus ? (
            <>
              <div className="grid grid-cols-2 gap-4">
                <div className="flex flex-col gap-1">
                  <span className="text-[10px] text-[var(--text-dim)] uppercase font-bold tracking-tighter">Status</span>
                  <span className="text-[10px] text-green-500 font-medium">Ready ({indexStatus.indexed_files} files)</span>
                </div>
                <div className="flex flex-col gap-1">
                  <span className="text-[10px] text-[var(--text-dim)] uppercase font-bold tracking-tighter">Size on Disk</span>
                  <span className="text-[10px] text-[var(--text-main)] font-mono">
                    {indexStatus.db_size_bytes ? formatBytes(indexStatus.db_size_bytes) : "Unknown"}
                  </span>
                </div>
                <div className="flex flex-col gap-1">
                  <span className="text-[10px] text-[var(--text-dim)] uppercase font-bold tracking-tighter">Total Chunks</span>
                  <span className="text-[10px] text-[var(--text-main)] font-mono">{indexStatus.total_chunks.toLocaleString()}</span>
                </div>
                <div className="flex flex-col gap-1">
                  <span className="text-[10px] text-[var(--text-dim)] uppercase font-bold tracking-tighter">Model</span>
                  <Tooltip content={indexStatus.model_id}>
                    <span className="text-[10px] text-[var(--text-main)] truncate">{indexStatus.model_id.split('/').pop()}</span>
                  </Tooltip>
                </div>
              </div>
              
              <div className="flex flex-col gap-1">
                <span className="text-[10px] text-[var(--text-dim)] uppercase font-bold tracking-tighter">Path</span>
                <span className="text-[10px] text-[var(--text-main)] font-mono break-all selectable">
                  {paths.workspace}/semantic_index.db
                </span>
              </div>

              <div className="flex gap-2 mt-1">
                {isTauri && (
                  <button
                    onClick={() => onOpen(paths.workspace)}
                    className="px-3 py-1.5 bg-[var(--bg-app)] hover:bg-[var(--bg-active)] text-[var(--text-main)] text-[10px] font-bold uppercase tracking-wider rounded border border-[var(--border-main)] transition-colors"
                  >
                    Open in File Manager
                  </button>
                )}
                <button
                  onClick={onDeleteIndex}
                  disabled={isDeleting || readOnly}
                  title={readOnly
                    ? "This workspace's index is managed by another application"
                    : undefined}
                  className="px-3 py-1.5 bg-red-900/20 hover:bg-red-900/40 text-red-400 text-[10px] font-bold uppercase tracking-wider rounded border border-red-900/50 transition-colors disabled:opacity-50"
                >
                  {isDeleting ? "Deleting..." : "Delete current index"}
                </button>
              </div>
            </>
          ) : (
            <div className="py-4 text-center">
              <p className="text-xs text-[var(--text-dim)] italic">No semantic index built yet.</p>
              <p className="text-[10px] text-[var(--text-dim)] mt-1">Configure and build your index in the Models page.</p>
            </div>
          )}
        </div>
      </section>

      <section>
        <div className="flex flex-col gap-1.5 mb-4">
          <h3 className="text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider">Workspaces</h3>
          <p className="text-[11px] text-[var(--text-muted)] leading-relaxed">
            Every library on this installation, including the corpora other
            applications keep here. Deleting one removes its index, settings and
            uploads from disk; the documents it indexed are left where they are.
          </p>
        </div>

        <div className="p-3 bg-[var(--bg-active)] rounded-lg border border-[var(--border-main)] flex flex-col gap-2">
          {workspaces.length === 0 ? (
            <p className="py-2 text-center text-xs text-[var(--text-dim)] italic">No workspaces listed yet.</p>
          ) : workspaces.map((workspace) => {
            const isActive = workspace.id === activeWorkspaceId;
            const isLast = workspaces.length === 1;
            const root = workspace.active_root ?? workspace.roots[0] ?? null;
            return (
              <div
                key={workspace.id}
                className="flex items-center gap-3 py-1.5 border-b border-[var(--border-main)] last:border-b-0"
              >
                <div className="flex flex-col gap-1 min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="text-[11px] text-[var(--text-main)] font-medium truncate">{workspace.name}</span>
                    {isActive && (
                      <span className="shrink-0 px-1.5 py-0.5 rounded bg-[var(--bg-app)] text-[9px] font-bold uppercase tracking-wider text-[var(--accent-blue)]">
                        Active
                      </span>
                    )}
                    {workspace.managed_by && (
                      <Tooltip content={`Managed by ${workspace.managed_by}: only that application writes to it`}>
                        <span className="shrink-0 px-1.5 py-0.5 rounded bg-[var(--bg-app)] text-[9px] font-bold uppercase tracking-wider text-[var(--text-dim)]">
                          {workspace.managed_by}
                        </span>
                      </Tooltip>
                    )}
                  </div>
                  <span className="text-[10px] text-[var(--text-dim)] font-mono break-all">
                    {root ?? "No folder open"}
                  </span>
                </div>
                <Tooltip content={isLast
                  ? "The last workspace cannot be deleted"
                  : isActive
                    ? "Deletes this workspace after switching to another one"
                    : `Delete ${workspace.name} and everything it holds`}
                >
                  <button
                    onClick={() => onDeleteWorkspace(workspace.id, workspace.name, workspace.managed_by)}
                    disabled={isLast || deletingWorkspaceId !== null}
                    aria-label={`Delete workspace ${workspace.name}`}
                    className="shrink-0 px-3 py-1.5 bg-red-900/20 hover:bg-red-900/40 text-red-400 text-[10px] font-bold uppercase tracking-wider rounded border border-red-900/50 transition-colors disabled:opacity-50"
                  >
                    {deletingWorkspaceId === workspace.id ? "Deleting..." : "Delete"}
                  </button>
                </Tooltip>
              </div>
            );
          })}
        </div>
      </section>

      <section>
        <div className="flex flex-col gap-1.5 mb-4">
          <h3 className="text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider">Application Data</h3>
          <p className="text-[11px] text-[var(--text-muted)] leading-relaxed">
            Local storage for settings, logs, and cache. The installation directory
            holds everything shared across workspaces — the model cache and the
            catalogue mirror; the workspace directory holds this library's own
            databases.
          </p>
        </div>

        <div className="p-3 bg-[var(--bg-active)] rounded-lg border border-[var(--border-main)] flex flex-col gap-3">
          <div className="flex flex-col gap-1">
            <span className="text-[10px] text-[var(--text-dim)] uppercase font-bold tracking-tighter">Installation Path</span>
            <span className="text-[10px] text-[var(--text-main)] font-mono break-all selectable">
              {paths.app_data}
            </span>
          </div>
          <div className="flex flex-col gap-1">
            <span className="text-[10px] text-[var(--text-dim)] uppercase font-bold tracking-tighter">Workspace Path</span>
            <span className="text-[10px] text-[var(--text-main)] font-mono break-all selectable">
              {paths.workspace}
            </span>
          </div>
          <div className="flex gap-2">
            <button
              onClick={() => onOpen(paths.app_data)}
              className="w-fit px-3 py-1.5 bg-[var(--bg-app)] hover:bg-[var(--bg-active)] text-[var(--text-main)] text-[10px] font-bold uppercase tracking-wider rounded border border-[var(--border-main)] transition-colors"
            >
              Open Installation Folder
            </button>
            <button
              onClick={() => onOpen(paths.workspace)}
              className="w-fit px-3 py-1.5 bg-[var(--bg-app)] hover:bg-[var(--bg-active)] text-[var(--text-main)] text-[10px] font-bold uppercase tracking-wider rounded border border-[var(--border-main)] transition-colors"
            >
              Open Workspace Folder
            </button>
          </div>
        </div>
      </section>

    </div>
  );
}
