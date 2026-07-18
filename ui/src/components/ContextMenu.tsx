import type { MouseEvent as ReactMouseEvent } from "react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { Icon } from "react-feather";

interface ContextMenuItemBase {
  id: string;
  label: string;
  icon?: Icon;
  disabled?: boolean;
  dividerBefore?: boolean;
}

export type ContextMenuItem = ContextMenuItemBase & (
  | { run: () => Promise<void> | void; inlineInput?: never }
  | {
      run?: never;
      inlineInput: {
        placeholder: string;
        submitLabel: string;
        submit: (value: string) => Promise<void> | void;
      };
    }
);

interface ContextMenuPosition {
  x: number;
  y: number;
}

interface ContextMenuState<T> {
  position: ContextMenuPosition;
  target: T;
  items: ContextMenuItem[];
  size?: "default" | "content";
}

interface OpenContextMenuArgs<T> {
  event: ReactMouseEvent;
  target: T;
  items: ContextMenuItem[];
  size?: "default" | "content";
}

export function useContextMenu<T>() {
  const [menu, setMenu] = useState<ContextMenuState<T> | null>(null);

  const closeMenu = useCallback(() => {
    setMenu(null);
  }, []);

  const openMenu = useCallback(({ event, target, items, size = "default" }: OpenContextMenuArgs<T>) => {
    event.preventDefault();
    setMenu({
      position: { x: event.clientX, y: event.clientY },
      target,
      items,
      size,
    });
  }, []);

  return { menu, openMenu, closeMenu };
}

interface ContextMenuProps<T> {
  menu: ContextMenuState<T> | null;
  onClose: () => void;
}

export function ContextMenu<T>({ menu, onClose }: ContextMenuProps<T>) {
  const menuRef = useRef<HTMLDivElement | null>(null);
  const [position, setPosition] = useState<ContextMenuPosition | null>(null);
  // Id of the item whose async `run()` is in flight, so we can show a spinner
  // and block further clicks. Generic — every menu action gets this for free.
  const [pendingId, setPendingId] = useState<string | null>(null);

  useEffect(() => {
    if (!menu) return;

    const handlePointerDown = (event: PointerEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) {
        onClose();
      }
    };

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
      }
    };

    const handleScroll = () => onClose();

    document.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("scroll", handleScroll, true);
    window.addEventListener("resize", handleScroll);

    return () => {
      document.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("scroll", handleScroll, true);
      window.removeEventListener("resize", handleScroll);
    };
  }, [menu, onClose]);

  useEffect(() => {
    if (!menu) {
      setPosition(null);
    }
    setPendingId(null);
  }, [menu]);

  useLayoutEffect(() => {
    if (!menu || !menuRef.current) return;

    const bounds = menuRef.current.getBoundingClientRect();
    const margin = 8;
    const maxX = Math.max(margin, window.innerWidth - bounds.width - margin);
    const maxY = Math.max(margin, window.innerHeight - bounds.height - margin);

    setPosition({
      x: Math.min(Math.max(menu.position.x, margin), maxX),
      y: Math.min(Math.max(menu.position.y, margin), maxY),
    });
  }, [menu]);

  const content = useMemo(() => {
    if (!menu) return null;

    return (
      <div
        ref={menuRef}
        role="menu"
        className={`fixed z-[150] rounded-lg border border-[var(--border-main)] bg-[var(--bg-app)] p-1 shadow-2xl ${
          menu.size === "content" ? "w-max min-w-max" : "min-w-44"
        }`}
        style={{
          left: `${position?.x ?? menu.position.x}px`,
          top: `${position?.y ?? menu.position.y}px`,
          visibility: position ? "visible" : "hidden",
        }}
      >
        {menu.items.map((item) => {
          const Icon = item.icon;
          const isPending = pendingId === item.id;
          const showIconSlot = menu.size !== "content" || Icon || isPending;
          const inlineInput = item.inlineInput;
          if (inlineInput) {
            return (
              <div key={item.id}>
                {item.dividerBefore && (
                  <div role="separator" className="my-1 border-t border-[var(--border-main)]" />
                )}
                <form
                  role="group"
                  aria-label={item.label}
                  onSubmit={(event) => {
                    event.preventDefault();
                    if (item.disabled || pendingId !== null) return;
                    const form = event.currentTarget;
                    const input = form.elements.namedItem("value") as HTMLInputElement;
                    const value = input.value.trim();
                    if (!value) return;
                    let result: Promise<void> | void;
                    try {
                      result = inlineInput.submit(value);
                    } catch (error) {
                      console.error("context menu action failed", error);
                      onClose();
                      return;
                    }
                    if (!result || typeof result.then !== "function") {
                      onClose();
                      return;
                    }
                    setPendingId(item.id);
                    void result
                      .catch((error) => console.error("context menu action failed", error))
                      .finally(() => {
                        setPendingId(null);
                        onClose();
                      });
                  }}
                  className="flex items-center gap-2 rounded-md px-3 py-1.5"
                >
                  <span className="flex h-4 w-4 flex-shrink-0 items-center justify-center text-[var(--text-muted)]">
                    {isPending ? (
                      <span data-testid="context-menu-spinner" className="h-3 w-3 animate-spin rounded-full border-2 border-[var(--text-muted)] border-t-transparent" />
                    ) : (
                      Icon && <Icon size={13} aria-hidden="true" />
                    )}
                  </span>
                  <input
                    name="value"
                    aria-label={item.label}
                    placeholder={inlineInput.placeholder}
                    disabled={item.disabled || pendingId !== null}
                    className="min-w-28 flex-1 rounded border border-[var(--border-main)] bg-[var(--bg-input)] px-2 py-1 text-xs text-[var(--text-main)] outline-none focus:border-[var(--accent-blue)]"
                  />
                  <button
                    type="submit"
                    disabled={item.disabled || pendingId !== null}
                    className="rounded bg-[var(--accent-blue)] px-2 py-1 text-[10px] text-white disabled:opacity-50"
                  >
                    {inlineInput.submitLabel}
                  </button>
                </form>
              </div>
            );
          }
          return (
            <div key={item.id}>
              {item.dividerBefore && (
                <div role="separator" className="my-1 border-t border-[var(--border-main)]" />
              )}
              <button
                role="menuitem"
                disabled={item.disabled || pendingId !== null}
                onClick={() => {
                  if (item.disabled || pendingId !== null) return;
                  let result: Promise<void> | void;
                  try {
                    result = item.run();
                  } catch (error) {
                    console.error("context menu action failed", error);
                    onClose();
                    return;
                  }
                  if (!result || typeof result.then !== "function") {
                    onClose();
                    return;
                  }
                  // Keep the menu open with a spinner until the action settles,
                  // so the click has immediate visible feedback.
                  setPendingId(item.id);
                  void result
                    .catch((error) => {
                      console.error("context menu action failed", error);
                    })
                    .finally(() => {
                      setPendingId(null);
                      onClose();
                    });
                }}
                className={`flex w-full items-center rounded-md px-3 py-1.5 text-left text-xs text-[var(--text-main)] hover:bg-[var(--bg-hover)] disabled:cursor-not-allowed disabled:opacity-50 ${
                  showIconSlot ? "gap-2" : ""
                }`}
              >
                {showIconSlot && (
                  <span className="flex h-4 w-4 flex-shrink-0 items-center justify-center text-[var(--text-muted)]">
                    {isPending ? (
                      <span
                        data-testid="context-menu-spinner"
                        className="h-3 w-3 rounded-full border-2 border-[var(--text-muted)] border-t-transparent animate-spin"
                      />
                    ) : (
                      Icon && <Icon size={13} aria-hidden="true" />
                    )}
                  </span>
                )}
                <span className="truncate">{item.label}</span>
              </button>
            </div>
          );
        })}
      </div>
    );
  }, [menu, onClose, position, pendingId]);

  if (!menu || !content) return null;
  return createPortal(content, document.body);
}
