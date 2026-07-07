import type { SearchApi } from "../services/api";
import type { ContextMenuItem } from "../components/ContextMenu";
import { zoteroMenuContributor } from "./integrations/zotero";
import type { MenuContributor } from "./integrations/types";
import type { Settings } from "./types";
import { isTauri } from "../services";
import { useChatStore } from "../stores/useChatStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { Copy, Edit2, ExternalLink, Folder, MessageSquare, RefreshCw } from "react-feather";

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
  settings?: Settings | null;
  onToast: (message: string, type: "success" | "error") => void;
  onRenameRequest?: (path: string) => void;
}

const menuContributors: MenuContributor[] = [zoteroMenuContributor];

export function buildFileContextMenuItems({
  target,
  api,
  capabilities,
  settings,
  onToast,
  onRenameRequest,
}: BuildFileContextMenuItemsArgs): ContextMenuItem[] {
  const items: ContextMenuItem[] = [
    {
      id: "open",
      label: "Open",
      icon: ExternalLink,
      run: () => target.open(),
    },
    {
      id: "copy-path",
      label: "Copy path",
      icon: Copy,
      run: async () => {
        try {
          await api.writeClipboard(target.path);
          onToast("Path copied", "success");
        } catch (error) {
          console.error("Failed to copy path:", error);
          onToast("Failed to copy path", "error");
        }
      },
    },
  ];

  if (target.kind !== "directory") {
    items.push({
      id: "rename",
      label: "Rename",
      icon: Edit2,
      run: () => onRenameRequest?.(target.path),
    });

    items.push({
      id: "refresh-metadata",
      label: "Refresh metadata",
      icon: RefreshCw,
      run: async () => {
        try {
          await api.refreshFileMetadata(target.path);
          useSettingsStore.getState().refreshFileList();
          onToast("Metadata refresh started", "success");
        } catch (error) {
          console.error("Failed to refresh metadata:", error);
          onToast("Failed to refresh metadata", "error");
        }
      },
    });

    if (isTauri) {
      items.push({
        id: "ask-about-file",
        label: "Ask about this file",
        icon: MessageSquare,
        run: async () => {
          const chat = useChatStore.getState();
          await chat.openPane();
          chat.addContext(target.path);
        },
      });
    }
  }

  if (capabilities.canOpenInFileManager) {
    items.push({
      id: "open-in-file-manager",
      label: target.kind === "directory" ? "Open in file manager" : "Reveal in folder",
      icon: Folder,
      run: async () => {
        if (target.kind === "directory") {
          await api.openPath(target.path);
        } else {
          await api.revealPath(target.path);
        }
      },
    });
  }

  if (settings) {
    items.push(
      ...menuContributors.flatMap((contributor) =>
        contributor({
          target,
          api,
          settings,
          onToast,
        }),
      ),
    );
  }

  return items;
}
