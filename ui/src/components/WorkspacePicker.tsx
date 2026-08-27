import { useState } from "react";
import { Edit2, Lock, Plus } from "react-feather";
import { useWorkspaceStore } from "../stores/useWorkspaceStore";
import { TextInputDialog } from "./TextInputDialog";
import { Tooltip } from "@leonrjg/wilkes-reader";
import { useToasts } from "./Toast";

type WorkspaceDialog =
  | { mode: "create" }
  | { mode: "rename"; workspaceId: string; currentName: string };

export default function WorkspacePicker() {
  const { addToast } = useToasts();
  const [dialog, setDialog] = useState<WorkspaceDialog | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const workspaces = useWorkspaceStore((state) => state.workspaces);
  const activeWorkspaceId = useWorkspaceStore((state) => state.activeWorkspaceId);
  const switching = useWorkspaceStore((state) => state.switching);
  const switchTo = useWorkspaceStore((state) => state.switchTo);
  const createAndSwitch = useWorkspaceStore((state) => state.createAndSwitch);
  const rename = useWorkspaceStore((state) => state.rename);
  const active = workspaces.find((workspace) => workspace.id === activeWorkspaceId);
  // A read-only workspace is listed and can be opened; only the controls that
  // would write to it are withheld.
  const activeIsReadOnly = active?.read_only ?? false;

  const submitDialog = async (name: string) => {
    if (!dialog) return;
    if (dialog.mode === "rename" && name === dialog.currentName) {
      setDialog(null);
      return;
    }
    setSubmitting(true);
    try {
      if (dialog.mode === "create") await createAndSwitch(name);
      else await rename(dialog.workspaceId, name);
      setDialog(null);
    } catch (error) {
      const fallback = dialog.mode === "create"
        ? "Could not create workspace"
        : "Could not rename workspace";
      addToast(error instanceof Error ? error.message : fallback, { type: "error" });
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <>
      <div className="flex h-6 shrink-0 items-center overflow-hidden rounded bg-[var(--bg-active)]">
        <select
          aria-label="Workspace"
          disabled={switching || submitting}
          value={activeWorkspaceId ?? ""}
          onChange={(event) => void switchTo(event.target.value).catch((error) =>
            addToast(error instanceof Error ? error.message : "Could not switch workspace", { type: "error" }),
          )}
          className="h-full max-w-36 bg-transparent px-2 text-xs font-medium text-[var(--text-main)] outline-none disabled:opacity-50"
        >
          {workspaces.map((workspace) => (
            <option key={workspace.id} value={workspace.id}>
              {workspace.read_only ? `${workspace.name} (read-only)` : workspace.name}
            </option>
          ))}
        </select>
        {activeIsReadOnly && (
          <Tooltip
            content={active?.managed_by
              ? `Read-only: this workspace is managed by ${active.managed_by}`
              : "Read-only workspace"}
          >
            <span
              aria-label="Read-only workspace"
              className="flex h-full w-4 items-center justify-center text-[var(--text-dim)]"
            >
              <Lock size={10} />
            </span>
          </Tooltip>
        )}
        <Tooltip content={activeIsReadOnly ? "Read-only workspaces cannot be renamed" : "Rename workspace"}>
          <button
            type="button"
            aria-label="Rename workspace"
            disabled={!active || activeIsReadOnly || switching || submitting}
            onClick={() => active && setDialog({
              mode: "rename",
              workspaceId: active.id,
              currentName: active.name,
            })}
            className="flex h-full w-6 items-center justify-center text-[var(--text-dim)] hover:text-[var(--text-main)] disabled:opacity-40"
          >
            <Edit2 size={10} />
          </button>
        </Tooltip>
        <Tooltip content="New workspace">
          <button
            type="button"
            aria-label="New workspace"
            disabled={switching || submitting}
            onClick={() => setDialog({ mode: "create" })}
            className="flex h-full w-6 items-center justify-center border-l border-[var(--border-main)] text-[var(--text-dim)] hover:text-[var(--text-main)] disabled:opacity-40"
          >
            <Plus size={11} />
          </button>
        </Tooltip>
      </div>
      <TextInputDialog
        open={dialog !== null}
        title={dialog?.mode === "rename" ? "Rename workspace" : "New workspace"}
        label="Workspace name"
        initialValue={dialog?.mode === "rename" ? dialog.currentName : "New workspace"}
        confirmLabel={dialog?.mode === "rename" ? "Rename" : "Create"}
        busy={submitting}
        onCancel={() => setDialog(null)}
        onSubmit={(name) => void submitDialog(name)}
      />
    </>
  );
}
