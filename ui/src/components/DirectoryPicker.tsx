import { useMemo } from "react";
import { confirmDialog } from "../lib/utils/dialog";
import { Folder, Star, X } from "react-feather";
import { useToasts } from "./Toast";
import { ContextMenu, useContextMenu } from "./ContextMenu";
import { api, isTauri } from "../services";
import { buildFileContextMenuItems, type ContextMenuTarget } from "../lib/fileActions";
import { useSettingsStore } from "../stores/useSettingsStore";
import { Tooltip } from "./Tooltip";

interface Props {
  directory: string;
  favorites: string[];
  recentDirs: string[];
  onChange: (dir: string) => void;
  onPickDirectory: () => void;
  onFavoriteAdd?: (dir: string) => void;
  onFavoriteRemove?: (dir: string) => void;
  onForgetDirectory?: (dir: string) => void;
}

function shortPath(p: string): string {
  const home = p.match(/^\/Users\/[^/]+/) ?? p.match(/^\/home\/[^/]+/);
  if (home) return "~" + p.slice(home[0].length);
  return p;
}

export default function DirectoryPicker({
  directory,
  favorites,
  recentDirs,
  onChange,
  onPickDirectory,
  onFavoriteAdd,
  onFavoriteRemove,
  onForgetDirectory,
}: Props) {
  const { addToast } = useToasts();
  const { menu, openMenu, closeMenu } = useContextMenu<ContextMenuTarget>();
  const settings = useSettingsStore((s) => s.settings);
  const isFavorite = (dir: string) => favorites.includes(dir);
  const onToast = (message: string, type: "success" | "error") => addToast(message, { type });

  // Combine favorites and recent dirs for the list, prioritizing favorites
  // and removing duplicates.
  const displayDirs = useMemo(() => {
    const combined = [...favorites];
    for (const d of recentDirs) {
      if (!combined.includes(d)) {
        combined.push(d);
      }
    }
    // Always ensure the current directory is in the list if it's not empty
    if (directory && !combined.includes(directory)) {
      combined.push(directory);
    }
    return combined;
  }, [favorites, recentDirs, directory]);

  return (
    <div className="flex items-center gap-1 min-w-0 w-full">
      <div className="flex h-6 items-center gap-0.5 bg-[var(--bg-active)] rounded overflow-hidden">
        <Tooltip content={directory || "Choose directory"}>
          <button
            onClick={onPickDirectory}
            className="h-full text-xs text-[var(--text-muted)] hover:text-[var(--text-main)] px-3 flex-shrink-0 flex items-center gap-1.5"
          >
            <Folder size={12} />
            <span>Open folder</span>
          </button>
        </Tooltip>
      </div>

      {/* Folders list (Favorites + History) */}
      {displayDirs.length > 0 && (
        <div className="flex items-center gap-1 overflow-x-auto flex-1 min-w-0 custom-scrollbar">
          {displayDirs.map((b) => {
            const favorite = isFavorite(b);
            const active = b === directory;

            return (
              <div
                key={b}
                className={`flex h-6 items-center gap-0.5 rounded transition-colors group bg-[var(--bg-active)]`}
                onContextMenu={(event) =>
                  openMenu({
                    event,
                    target: { kind: "directory", path: b, open: () => onChange(b) },
                    items: buildFileContextMenuItems({
                      target: { kind: "directory", path: b, open: () => onChange(b) },
                      api,
                      capabilities: { canOpenInFileManager: isTauri },
                      settings,
                      onToast,
                    }),
                  })}
              >
                {onForgetDirectory && (
                  <Tooltip content="Remove from history">
                    <button
                      onClick={async (e) => {
                        e.stopPropagation();
                        const confirmed = await confirmDialog(`Remove "${shortPath(b)}" from your history?`);
                        if (confirmed) onForgetDirectory(b);
                      }}
                      className="h-full text-[10px] pl-1.5 pr-1 text-[var(--text-dim)] hover:text-[var(--text-error)] transition-colors"
                    >
                      <X size={12} />
                    </button>
                  </Tooltip>
                )}
                <Tooltip content={b} className="font-mono break-all">
                  <button
                    onClick={() => onChange(b)}
                    className={`h-full select-none text-xs px-2 flex-shrink-0 truncate max-w-[100px] transition-colors ${
                      active
                        ? "text-[var(--text-main)] font-bold"
                        : "text-[var(--text-muted)] hover:text-[var(--text-main)]"
                    }`}
                  >
                    {shortPath(b).split("/").pop() || shortPath(b)}
                  </button>
                </Tooltip>
                {onFavoriteAdd && onFavoriteRemove && (
                  <Tooltip content={favorite ? "Remove favorite" : "Favorite this directory"}>
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        favorite ? onFavoriteRemove(b) : onFavoriteAdd(b);
                      }}
                      className={`h-full text-[10px] px-1.5 transition-colors ${
                        favorite
                          ? "text-[var(--accent-blue)]"
                          : "text-[var(--text-dim)] hover:text-[var(--accent-blue)]"
                      }`}
                    >
                      <Star size={10} fill={favorite ? "currentColor" : "none"} />
                    </button>
                  </Tooltip>
                )}
              </div>
            );
          })}
        </div>
      )}
      <ContextMenu menu={menu} onClose={closeMenu} />
    </div>
  );
}
