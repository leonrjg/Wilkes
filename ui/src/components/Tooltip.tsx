import React, { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

interface TooltipProps {
  content: React.ReactNode;
  children: React.ReactElement<React.HTMLAttributes<HTMLElement> & React.RefAttributes<HTMLElement>>;
  className?: string;
  /** Delay pointer-triggered tooltips; keyboard focus always opens immediately. */
  delayMs?: number;
  /** Keep the tooltip open while it is hovered so its contents can be selected. */
  interactive?: boolean;
  size?: "default" | "wide";
}

const INTERACTIVE_CLOSE_DELAY_MS = 150;

export function Tooltip({
  content,
  children,
  className = "",
  delayMs = 0,
  interactive = false,
  size = "default",
}: TooltipProps) {
  const id = useId();
  const triggerRef = useRef<HTMLElement | null>(null);
  const tooltipRef = useRef<HTMLDivElement>(null);
  const openTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const closeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const triggerPointerInsideRef = useRef(false);
  const tooltipPointerInsideRef = useRef(false);
  const triggerFocusedRef = useRef(false);
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<{ x: number; y: number } | null>(null);

  const cancelPendingOpen = () => {
    if (openTimerRef.current !== null) {
      clearTimeout(openTimerRef.current);
      openTimerRef.current = null;
    }
  };

  const cancelPendingClose = () => {
    if (closeTimerRef.current !== null) {
      clearTimeout(closeTimerRef.current);
      closeTimerRef.current = null;
    }
  };

  const closeFromInteraction = () => {
    cancelPendingClose();
    if (!interactive) {
      setOpen(false);
      return;
    }

    closeTimerRef.current = setTimeout(() => {
      closeTimerRef.current = null;
      if (
        !triggerPointerInsideRef.current
        && !tooltipPointerInsideRef.current
        && !triggerFocusedRef.current
      ) {
        setOpen(false);
      }
    }, INTERACTIVE_CLOSE_DELAY_MS);
  };

  const openFromPointer = () => {
    cancelPendingOpen();
    cancelPendingClose();
    if (delayMs <= 0) {
      setOpen(true);
      return;
    }
    openTimerRef.current = setTimeout(() => {
      openTimerRef.current = null;
      setOpen(true);
    }, delayMs);
  };

  useEffect(
    () => () => {
      if (openTimerRef.current !== null) clearTimeout(openTimerRef.current);
      if (closeTimerRef.current !== null) clearTimeout(closeTimerRef.current);
    },
    [],
  );

  useEffect(() => {
    if (!open || !interactive) return;

    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      cancelPendingOpen();
      cancelPendingClose();
      tooltipPointerInsideRef.current = false;
      setOpen(false);
    };

    document.addEventListener("keydown", closeOnEscape);
    return () => document.removeEventListener("keydown", closeOnEscape);
  }, [interactive, open]);

  useLayoutEffect(() => {
    if (!open || !triggerRef.current || !tooltipRef.current) return;

    const triggerRect = triggerRef.current.getBoundingClientRect();
    const tooltipRect = tooltipRef.current.getBoundingClientRect();
    const margin = 8;
    const gap = 6;
    const x = Math.min(
      Math.max(triggerRect.left + triggerRect.width / 2 - tooltipRect.width / 2, margin),
      window.innerWidth - tooltipRect.width - margin,
    );
    const above = triggerRect.top - tooltipRect.height - gap;
    const y = above >= margin
      ? above
      : Math.min(triggerRect.bottom + gap, window.innerHeight - tooltipRect.height - margin);

    setPosition({ x, y });
  }, [open, content]);

  if (content == null || content === "") return children;

  const child = React.Children.only(children);
  const childProps = child.props;
  const describedBy = [childProps["aria-describedby"], open ? id : null]
    .filter(Boolean)
    .join(" ") || undefined;

  return (
    <>
      {React.cloneElement(child, {
        ref: (node: HTMLElement | null) => {
          triggerRef.current = node;
          const { ref } = child.props;
          if (typeof ref === "function") ref(node);
          else if (ref && "current" in ref) {
            (ref as React.MutableRefObject<HTMLElement | null>).current = node;
          }
        },
        "aria-label": childProps["aria-label"] ?? (typeof content === "string" ? content : undefined),
        "aria-describedby": describedBy,
        onMouseEnter: (event: React.MouseEvent<HTMLElement>) => {
          childProps.onMouseEnter?.(event);
          triggerPointerInsideRef.current = true;
          openFromPointer();
        },
        onMouseLeave: (event: React.MouseEvent<HTMLElement>) => {
          childProps.onMouseLeave?.(event);
          triggerPointerInsideRef.current = false;
          cancelPendingOpen();
          closeFromInteraction();
        },
        onFocus: (event: React.FocusEvent<HTMLElement>) => {
          childProps.onFocus?.(event);
          triggerFocusedRef.current = true;
          cancelPendingOpen();
          cancelPendingClose();
          setOpen(true);
        },
        onBlur: (event: React.FocusEvent<HTMLElement>) => {
          childProps.onBlur?.(event);
          triggerFocusedRef.current = false;
          cancelPendingOpen();
          closeFromInteraction();
        },
      })}
      {open &&
        createPortal(
          <div
            ref={tooltipRef}
            id={id}
            role="tooltip"
            className={`${interactive ? "pointer-events-auto select-text" : "pointer-events-none"} fixed z-[180] ${size === "wide" ? "max-w-[440px]" : "max-w-[320px]"} rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-2 py-1 text-[11px] leading-snug text-[var(--text-main)] shadow-xl ${className}`}
            style={{
              left: `${position?.x ?? 0}px`,
              top: `${position?.y ?? 0}px`,
              visibility: position ? "visible" : "hidden",
            }}
            onMouseEnter={() => {
              if (!interactive) return;
              tooltipPointerInsideRef.current = true;
              cancelPendingClose();
            }}
            onMouseLeave={() => {
              if (!interactive) return;
              tooltipPointerInsideRef.current = false;
              closeFromInteraction();
            }}
          >
            {content}
          </div>,
          document.body,
        )}
    </>
  );
}
