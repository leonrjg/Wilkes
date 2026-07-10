import { useEffect, useRef } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { readTextScrollPosition, saveTextScrollPosition } from "./textScrollMemory";

interface MarkdownViewerProps {
  content: string;
  documentPath: string;
  restoreScrollPosition?: boolean;
}

export default function MarkdownViewer({ content, documentPath, restoreScrollPosition = true }: MarkdownViewerProps) {
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const scroll = scrollRef.current;
    if (!scroll) return;

    const savePosition = () => {
      const maximum = scroll.scrollHeight - scroll.clientHeight;
      saveTextScrollPosition(documentPath, "rendered", maximum > 0 ? scroll.scrollTop / maximum : 0);
    };
    const onScroll = () => savePosition();
    scroll.addEventListener("scroll", onScroll, { passive: true });

    let frame: number | null = null;
    if (restoreScrollPosition) {
      const position = readTextScrollPosition(documentPath, "rendered");
      if (position !== null) {
        frame = window.requestAnimationFrame(() => {
          scroll.scrollTop = position * Math.max(scroll.scrollHeight - scroll.clientHeight, 0);
        });
      }
    }

    return () => {
      if (frame !== null) window.cancelAnimationFrame(frame);
      savePosition();
      scroll.removeEventListener("scroll", onScroll);
    };
  }, [content, documentPath, restoreScrollPosition]);

  return (
    <div ref={scrollRef} className="h-full overflow-auto px-6 py-5 text-sm text-[var(--text-main)]">
      <article className="prose-document">
        <ReactMarkdown
          remarkPlugins={[remarkGfm]}
          components={{
            a: ({ children, href }) => (
              <a href={href} target="_blank" rel="noreferrer">
                {children}
              </a>
            ),
          }}
        >
          {content}
        </ReactMarkdown>
      </article>
    </div>
  );
}
