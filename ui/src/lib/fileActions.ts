import type { SearchApi } from "../services/api";
import type { ContextMenuItem } from "../components/ContextMenu";
import { zoteroMenuContributor } from "./integrations/zotero";
import type { MenuContributor } from "./integrations/types";
import type { Settings } from "./types";
import { isTauri } from "../services";
import { useChatStore } from "../stores/useChatStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { Copy, Edit2, ExternalLink, Folder, FolderPlus, MessageSquare, RefreshCw, Tag, Trash2 } from "react-feather";
import { useResearchStore } from "../stores/useResearchStore";

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
  /** Other known root directories the file could be moved into. */
  availableRoots?: string[];
  onMoveRequest?: (path: string) => void;
  deletionKind?: "trash" | "permanent";
  onDeleteRequest?: (path: string) => Promise<void>;
}

const menuContributors: MenuContributor[] = [zoteroMenuContributor];

export function buildFileContextMenuItems({
  target,
  api,
  capabilities,
  settings,
  onToast,
  onRenameRequest,
  availableRoots = [],
  onMoveRequest,
  deletionKind,
  onDeleteRequest,
}: BuildFileContextMenuItemsArgs): ContextMenuItem[] {
  const primaryItems: ContextMenuItem[] = [
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

  if (capabilities.canOpenInFileManager) {
    primaryItems.push({
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

  if (target.kind !== "directory" && isTauri) {
    primaryItems.push({
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

  if (target.kind === "directory") return primaryItems;

  const managementItems: ContextMenuItem[] = [
    {
      id: "rename",
      label: "Rename",
      icon: Edit2,
      run: () => onRenameRequest?.(target.path),
    },
  ];

  const research = useResearchStore.getState();
  const currentEntry = useSettingsStore.getState().fileList.find((entry) => entry.path === target.path);
  managementItems.push({
    id: "tag-create-and-add",
    label: "Create and add tag",
    icon: Tag,
    inlineInput: {
      placeholder: "New tag…",
      submitLabel: "Add",
      submit: async (name) => {
        try {
          const created = await research.createTag({ name });
          await research.updateDocumentTags({
            paths: [target.path],
            add_tag_ids: [created.id],
            remove_tag_ids: [],
          });
          await useSettingsStore.getState().refreshFileList();
          onToast(`Created and added ${created.name}`, "success");
        } catch (error) {
          console.error("Failed to create and add tag:", error);
          onToast("Failed to create and add tag", "error");
        }
      },
    },
  });
  for (const tag of research.tags) {
    const assigned = currentEntry?.tags?.some((item) => item.id === tag.id) ?? false;
    managementItems.push({
      id: `tag-${tag.id}`,
      label: `${assigned ? "Remove" : "Add"} tag: ${tag.name}`,
      icon: Tag,
      run: async () => {
        try {
          await research.updateDocumentTags({
            paths: [target.path],
            add_tag_ids: assigned ? [] : [tag.id],
            remove_tag_ids: assigned ? [tag.id] : [],
          });
          await useSettingsStore.getState().refreshFileList();
          onToast(assigned ? `Removed ${tag.name}` : `Added ${tag.name}`, "success");
        } catch (error) {
          console.error("Failed to update document tags:", error);
          onToast("Failed to update tags", "error");
        }
      },
    });
  }

  if (isTauri && availableRoots.length > 0) {
    managementItems.push({
        id: "move-to",
        label: "Move to...",
        icon: FolderPlus,
        run: () => onMoveRequest?.(target.path),
    });
  }

  if (settings) {
    managementItems.push(
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

  managementItems.push(
    {
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
    },
  );

  if (onDeleteRequest) {
    managementItems.push({
      id: "delete",
      label: deletionKind === "trash" ? "Move to Trash" : "Delete permanently",
      icon: Trash2,
      dividerBefore: true,
      run: () => onDeleteRequest(target.path),
    });
  }

  managementItems[0].dividerBefore = primaryItems.length > 0;
  return [...primaryItems, ...managementItems];
}
