import type { MouseEvent as ReactMouseEvent } from "react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";

export interface ContextMenuItem {
  id: string;
  label: string;
  disabled?: boolean;
  run: () => Promise<void> | void;
}

interface ContextMenuPosition {
  x: number;
  y: number;
}

interface ContextMenuState<T> {
  position: ContextMenuPosition;
  target: T;
  items: ContextMenuItem[];
}

interface OpenContextMenuArgs<T> {
  event: ReactMouseEvent;
  target: T;
  items: ContextMenuItem[];
}

export function useContextMenu<T>() {
  const [menu, setMenu] = useState<ContextMenuState<T> | null>(null);

  const closeMenu = useCallback(() => {
    setMenu(null);
  }, []);

  const openMenu = useCallback(({ event, target, items }: OpenContextMenuArgs<T>) => {
    event.preventDefault();
    setMenu({
      position: { x: event.clientX, y: event.clientY },
      target,
      items,
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
        className="fixed z-[150] min-w-44 rounded-lg border border-[var(--border-main)] bg-[var(--bg-app)] p-1 shadow-2xl"
        style={{
          left: `${position?.x ?? menu.position.x}px`,
          top: `${position?.y ?? menu.position.y}px`,
          visibility: position ? "visible" : "hidden",
        }}
      >
        {menu.items.map((item) => (
          <button
            key={item.id}
            role="menuitem"
            disabled={item.disabled}
            onClick={async () => {
              if (item.disabled) return;
              await item.run();
              onClose();
            }}
            className="flex w-full items-center rounded-md px-3 py-1.5 text-left text-xs text-[var(--text-main)] hover:bg-[var(--bg-hover)] disabled:cursor-not-allowed disabled:opacity-50"
          >
            {item.label}
          </button>
        ))}
      </div>
    );
  }, [menu, onClose, position]);

  if (!menu || !content) return null;
  return createPortal(content, document.body);
}
