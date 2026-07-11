import { useEffect, useRef, useState } from "react";
import type { ButtonHTMLAttributes, ReactNode } from "react";

interface CopyButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "onClick"> {
  copy: () => Promise<void> | void;
  children: ReactNode;
  copiedChildren?: ReactNode;
  copiedAriaLabel?: string;
  copiedTitle?: string;
  resetAfterMs?: number;
  onClick?: ButtonHTMLAttributes<HTMLButtonElement>["onClick"];
  onCopyError?: (error: unknown) => void;
}

/** A button that reports successful clipboard writes before returning to its normal label. */
export function CopyButton({
  copy,
  children,
  copiedChildren,
  copiedAriaLabel = "Copied",
  copiedTitle: _copiedTitle = "Copied",
  resetAfterMs = 2_000,
  onClick,
  onCopyError = (error) => console.error("Copy failed:", error),
  "aria-label": ariaLabel,
  title: _title,
  ...buttonProps
}: CopyButtonProps) {
  const [copied, setCopied] = useState(false);
  const resetTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => () => {
    if (resetTimer.current) clearTimeout(resetTimer.current);
  }, []);

  const handleClick = async (event: React.MouseEvent<HTMLButtonElement>) => {
    onClick?.(event);
    if (event.defaultPrevented) return;

    try {
      await copy();
      setCopied(true);
      if (resetTimer.current) clearTimeout(resetTimer.current);
      resetTimer.current = setTimeout(() => setCopied(false), resetAfterMs);
    } catch (error) {
      onCopyError(error);
    }
  };

  return (
    <button
      {...buttonProps}
      type={buttonProps.type ?? "button"}
      onClick={handleClick}
      aria-label={copied ? copiedAriaLabel : ariaLabel}
    >
      {copied ? copiedChildren ?? children : children}
    </button>
  );
}
