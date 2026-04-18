import type { SearchApi } from "../services/api";
import type { ContextMenuItem } from "../components/ContextMenu";

export type ContextMenuTarget =
  | { kind: "file" | "match"; path: string; open: () => void }
  | { kind: "directory"; path: string; open: () => void };

export interface ContextMenuCapabilities {
  canOpenInFileManager: boolean;
}

interface BuildFileContextMenuItemsArgs {
  target: ContextMenuTarget;
  api: SearchApi;
  capabilities: ContextMenuCapabilities;
  onToast: (message: string, type: "success" | "error") => void;
}

function parentDir(path: string): string {
  const normalized = path.replace(/[\\/]+$/, "");
  const idx = Math.max(normalized.lastIndexOf("/"), normalized.lastIndexOf("\\"));
  if (idx <= 0) return normalized;
  return normalized.slice(0, idx);
}

async function copyToClipboard(text: string): Promise<void> {
  if (!navigator.clipboard?.writeText) {
    throw new Error("Clipboard API unavailable");
  }
  await navigator.clipboard.writeText(text);
}

export function buildFileContextMenuItems({
  target,
  api,
  capabilities,
  onToast,
}: BuildFileContextMenuItemsArgs): ContextMenuItem[] {
  const items: ContextMenuItem[] = [
    {
      id: "open",
      label: "Open",
      run: () => target.open(),
    },
    {
      id: "copy-path",
      label: "Copy path",
      run: async () => {
        try {
          await copyToClipboard(target.path);
          onToast("Path copied", "success");
        } catch (error) {
          console.error("Failed to copy path:", error);
          onToast("Failed to copy path", "error");
        }
      },
    },
  ];

  if (capabilities.canOpenInFileManager) {
    items.push({
      id: "open-in-file-manager",
      label: target.kind === "directory" ? "Open in file manager" : "Open containing folder",
      run: async () => {
        const path = target.kind === "directory" ? target.path : parentDir(target.path);
        await api.openPath(path);
      },
    });
  }

  return items;
}
