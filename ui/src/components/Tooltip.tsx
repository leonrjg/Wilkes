import React, { useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

interface TooltipProps {
  content: React.ReactNode;
  children: React.ReactElement<React.HTMLAttributes<HTMLElement> & React.RefAttributes<HTMLElement>>;
  className?: string;
}

export function Tooltip({ content, children, className = "" }: TooltipProps) {
  const id = useId();
  const triggerRef = useRef<HTMLElement | null>(null);
  const tooltipRef = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<{ x: number; y: number } | null>(null);

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
          setOpen(true);
        },
        onMouseLeave: (event: React.MouseEvent<HTMLElement>) => {
          childProps.onMouseLeave?.(event);
          setOpen(false);
        },
        onFocus: (event: React.FocusEvent<HTMLElement>) => {
          childProps.onFocus?.(event);
          setOpen(true);
        },
        onBlur: (event: React.FocusEvent<HTMLElement>) => {
          childProps.onBlur?.(event);
          setOpen(false);
        },
      })}
      {open &&
        createPortal(
          <div
            ref={tooltipRef}
            id={id}
            role="tooltip"
            className={`pointer-events-none fixed z-[180] max-w-[320px] rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-2 py-1 text-[11px] leading-snug text-[var(--text-main)] shadow-xl ${className}`}
            style={{
              left: `${position?.x ?? 0}px`,
              top: `${position?.y ?? 0}px`,
              visibility: position ? "visible" : "hidden",
            }}
          >
            {content}
          </div>,
          document.body,
        )}
    </>
  );
}
