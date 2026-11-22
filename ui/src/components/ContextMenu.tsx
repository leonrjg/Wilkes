import type { MouseEvent as ReactMouseEvent } from "react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Check, type Icon } from "react-feather";

interface ContextMenuItemBase {
  id: string;
  label: string;
  icon?: Icon;
  disabled?: boolean;
  dividerBefore?: boolean;
  /** The item stands for the current choice: shown with a check, and
   *  announced as the checked one. */
  checked?: boolean;
  /** A second, narrower action on the same row, shown as an icon button at
   *  its end: "do this to that one" beside "choose that one". */
  secondary?: {
    label: string;
    icon: Icon;
    run: () => Promise<void> | void;
  };
}

/** The menu's focusable actions, in the order arrow keys move through. */
function menuActions(menu: HTMLElement | null): HTMLElement[] {
  if (!menu) return [];
  return Array.from(
    menu.querySelectorAll<HTMLElement>(
      '[role="menuitem"]:not(:disabled), [role="menuitemradio"]:not(:disabled)',
    ),
  );
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

  /** Opens the menu at a point, for a menu opened by something other than a
   *  pointer: a button's own corner, or a command chosen from the native menu. */
  const openMenuAt = useCallback(
    ({ position, target, items, size = "default" }: Omit<OpenContextMenuArgs<T>, "event"> & {
      position: ContextMenuPosition;
    }) => {
      setMenu({ position, target, items, size });
    },
    [],
  );

  return { menu, openMenu, openMenuAt, closeMenu };
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
    // Where focus was when the menu opened, given back when it closes: a menu
    // opened from a button returns the keyboard to that button.
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    return () => {
      if (opener?.isConnected) opener.focus({ preventScroll: true });
    };
  }, [menu]);

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
        return;
      }
      if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
      const actions = menuActions(menuRef.current);
      if (actions.length === 0) return;
      event.preventDefault();
      const current = actions.indexOf(document.activeElement as HTMLElement);
      const next =
        event.key === "Home" ? 0
          : event.key === "End" ? actions.length - 1
            : event.key === "ArrowDown" ? (current + 1) % actions.length
              : (current <= 0 ? actions.length : current) - 1;
      actions[next].focus({ preventScroll: true });
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

  // Once placed, the keyboard is in the menu: on the checked item when there
  // is one, otherwise on the first.
  const placed = position !== null;
  useEffect(() => {
    if (!menu || !placed) return;
    const actions = menuActions(menuRef.current);
    const checked = actions.find((action) => action.getAttribute("aria-checked") === "true");
    (checked ?? actions[0])?.focus({ preventScroll: true });
  }, [menu, placed]);

  // One way to run an action, for a row's own and its secondary one alike.
  const runAction = useCallback((id: string, run: () => Promise<void> | void) => {
    if (pendingId !== null) return;
    let result: Promise<void> | void;
    try {
      result = run();
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
    setPendingId(id);
    void result
      .catch((error) => {
        console.error("context menu action failed", error);
      })
      .finally(() => {
        setPendingId(null);
        onClose();
      });
  }, [onClose, pendingId]);

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
          const Icon = item.icon ?? (item.checked ? Check : undefined);
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
          const run = item.run;
          const secondary = item.secondary;
          const SecondaryIcon = secondary?.icon;
          const secondaryId = `${item.id}:secondary`;
          return (
            <div key={item.id}>
              {item.dividerBefore && (
                <div role="separator" className="my-1 border-t border-[var(--border-main)]" />
              )}
              <div className="flex items-center rounded-md hover:bg-[var(--bg-hover)]">
              <button
                role={item.checked === undefined ? "menuitem" : "menuitemradio"}
                aria-checked={item.checked === undefined ? undefined : item.checked}
                disabled={item.disabled || pendingId !== null}
                onClick={() => {
                  if (item.disabled) return;
                  runAction(item.id, run);
                }}
                className={`flex min-w-0 flex-1 items-center rounded-md px-3 py-1.5 text-left text-xs text-[var(--text-main)] outline-none focus-visible:bg-[var(--bg-hover)] disabled:cursor-not-allowed disabled:opacity-50 ${
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
              {secondary && SecondaryIcon && (
                <button
                  role="menuitem"
                  aria-label={secondary.label}
                  title={secondary.label}
                  disabled={pendingId !== null}
                  onClick={() => runAction(secondaryId, secondary.run)}
                  className="mr-1 flex h-6 w-6 flex-shrink-0 items-center justify-center rounded text-[var(--text-dim)] outline-none hover:bg-[var(--bg-active)] hover:text-[var(--text-main)] focus-visible:bg-[var(--bg-active)] disabled:opacity-50"
                >
                  {pendingId === secondaryId ? (
                    <span
                      data-testid="context-menu-spinner"
                      className="h-3 w-3 rounded-full border-2 border-[var(--text-muted)] border-t-transparent animate-spin"
                    />
                  ) : (
                    <SecondaryIcon size={12} aria-hidden="true" />
                  )}
                </button>
              )}
              </div>
            </div>
          );
        })}
      </div>
    );
  }, [menu, position, pendingId, runAction]);

  if (!menu || !content) return null;
  return createPortal(content, document.body);
}
