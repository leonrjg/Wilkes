import { useEffect, useRef } from "react";
import { ArrowLeft, ArrowRight, Columns, Minimize2, X, XSquare } from "react-feather";
import type { ContextMenuItem } from "./ContextMenu";
import { paneActiveTab, useViewerStore } from "../stores/useViewerStore";
import { fileName } from "./DocumentEntryRow";
import { useFileContextMenu } from "./FileContextMenu";
import { Tooltip } from "@leonrjg/wilkes-reader";

interface ViewerTabsProps {
  standalone?: boolean;
  /** The pane whose tabs this strip shows. */
  paneId: string;
}

export default function ViewerTabs({ standalone = false, paneId }: ViewerTabsProps) {
  const allTabs = useViewerStore((state) => state.tabs);
  const tabs = allTabs.filter((tab) => tab.paneId === paneId);
  const panes = useViewerStore((state) => state.panes);
  const paneIndex = panes.findIndex((pane) => pane.id === paneId);
  const leftPane = panes[paneIndex - 1] ?? null;
  const rightPane = panes[paneIndex + 1] ?? null;
  const split = panes.length > 1;
  const activeTabId = useViewerStore((state) => paneActiveTab(state, paneId)?.id ?? null);
  const focused = useViewerStore((state) => state.focusedPaneId === paneId);
  const activateTab = useViewerStore((state) => state.activateTab);
  const closeTab = useViewerStore((state) => state.closeTab);
  const closePane = useViewerStore((state) => state.closePane);
  const splitTab = useViewerStore((state) => state.splitTab);
  const moveTabToPane = useViewerStore((state) => state.moveTabToPane);
  const joinPanes = useViewerStore((state) => state.joinPanes);
  const { openFileMenu, fileMenu } = useFileContextMenu();
  const tabRefs = useRef(new Map<string, HTMLButtonElement>());

  useEffect(() => {
    const activeTab = tabRefs.current.get(activeTabId ?? "");
    if (!activeTab || typeof activeTab.scrollIntoView !== "function") return;
    activeTab.scrollIntoView({
      behavior: "smooth",
      block: "nearest",
      inline: "nearest",
    });
  }, [activeTabId]);

  if (tabs.length === 0) return null;

  const focusTab = (index: number) => {
    const tab = tabs[index];
    if (!tab) return;
    activateTab(tab.id);
    requestAnimationFrame(() => tabRefs.current.get(tab.id)?.focus());
  };

  const closeAndFocusNeighbor = (id: string, index: number) => {
    const neighbor =
      id === activeTabId
        ? tabs[index + 1] ?? tabs[index - 1] ?? null
        : tabs.find((tab) => tab.id === activeTabId) ?? null;
    closeTab(id);
    if (neighbor) {
      requestAnimationFrame(() => tabRefs.current.get(neighbor.id)?.focus());
    }
  };

  return (
    <>
      <div
        role="tablist"
        aria-label={split ? `Open documents, pane ${paneIndex + 1} of ${panes.length}` : "Open documents"}
        className="flex min-h-9 flex-shrink-0 items-end overflow-x-auto border-b border-[var(--border-main)] bg-[var(--bg-sidebar)] custom-scrollbar"
      >
        {tabs.map((tab, index) => {
          const active = tab.id === activeTabId;
          const name = fileName(tab.path);
          return (
            <div
              key={tab.id}
              onContextMenu={(event) => {
                if (standalone) return;
                openFileMenu(
                  event,
                  {
                    kind: "file",
                    path: tab.path,
                    open: () => activateTab(tab.id),
                  },
                  [
                    // A pane's only tab split off would leave its pane empty,
                    // which is the same layout it started from.
                    ...(tabs.length > 1
                      ? [{
                          id: "split-right",
                          label: "Split Right",
                          icon: Columns,
                          run: () => splitTab(tab.id),
                        } satisfies ContextMenuItem]
                      : []),
                    ...(leftPane
                      ? [{
                          id: "move-to-left-pane",
                          label: "Move to Left Pane",
                          icon: ArrowLeft,
                          run: () => moveTabToPane(tab.id, leftPane.id),
                        } satisfies ContextMenuItem]
                      : []),
                    ...(rightPane
                      ? [{
                          id: "move-to-right-pane",
                          label: "Move to Right Pane",
                          icon: ArrowRight,
                          run: () => moveTabToPane(tab.id, rightPane.id),
                        } satisfies ContextMenuItem]
                      : []),
                    ...(split
                      ? [{
                          id: "join-panes",
                          label: "Join All Panes",
                          icon: Minimize2,
                          run: () => joinPanes(),
                        } satisfies ContextMenuItem]
                      : []),
                    {
                      id: "close-tab",
                      label: "Close",
                      icon: X,
                      run: () => closeAndFocusNeighbor(tab.id, index),
                    },
                    {
                      id: "close-all-tabs",
                      label: split ? "Close All in Pane" : "Close All",
                      icon: XSquare,
                      run: () => closePane(paneId),
                    },
                  ],
                );
              }}
              className={[
                "group flex h-9 min-w-[8rem] max-w-[14rem] flex-shrink-0 items-center border-r border-t-2 border-[var(--border-main)]",
                active
                  ? `${focused ? "border-t-[var(--accent-blue)]" : "border-t-[var(--border-strong)]"} bg-[var(--bg-app)] text-[var(--text-main)]`
                  : "border-t-transparent bg-[var(--bg-sidebar)] text-[var(--text-muted)] hover:bg-[var(--bg-hover)]",
              ].join(" ")}
            >
            <Tooltip content={tab.path} className="min-w-0 flex-1">
              <button
                ref={(node) => {
                  if (node) tabRefs.current.set(tab.id, node);
                  else tabRefs.current.delete(tab.id);
                }}
                id={`viewer-tab-${tab.id}`}
                type="button"
                role="tab"
                aria-label={name}
                aria-selected={active}
                aria-controls={`viewer-tabpanel-${paneId}`}
                tabIndex={active ? 0 : -1}
                onClick={() => activateTab(tab.id)}
                onAuxClick={(event) => {
                  if (event.button === 1) {
                    event.preventDefault();
                    closeTab(tab.id);
                  }
                }}
                onKeyDown={(event) => {
                  if (event.key === "ArrowLeft") {
                    event.preventDefault();
                    focusTab((index - 1 + tabs.length) % tabs.length);
                  } else if (event.key === "ArrowRight") {
                    event.preventDefault();
                    focusTab((index + 1) % tabs.length);
                  } else if (event.key === "Home") {
                    event.preventDefault();
                    focusTab(0);
                  } else if (event.key === "End") {
                    event.preventDefault();
                    focusTab(tabs.length - 1);
                  } else if (event.key === "Delete") {
                    event.preventDefault();
                    closeAndFocusNeighbor(tab.id, index);
                  }
                }}
                className="flex h-full min-w-0 flex-1 select-none items-center truncate px-3 text-left text-xs"
              >
                {name}
              </button>
            </Tooltip>
            <Tooltip content={`Close ${name}`}>
              <button
                type="button"
                aria-label={`Close ${name}`}
                onClick={() => closeAndFocusNeighbor(tab.id, index)}
                className={[
                  "mr-1 inline-flex rounded p-1 transition-colors",
                  active
                    ? "text-[var(--text-dim)] hover:bg-red-500/10 hover:text-red-500"
                    : "text-transparent group-hover:text-[var(--text-dim)] focus-visible:text-[var(--text-dim)] hover:!bg-red-500/10 hover:!text-red-500",
                ].join(" ")}
              >
                <X size={12} />
              </button>
            </Tooltip>
            </div>
          );
        })}
      </div>
      {fileMenu}
    </>
  );
}
