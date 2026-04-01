interface Props {
  directory: string;
  bookmarks: string[];
  onChange: (dir: string) => void;
  onPickDirectory: () => void;
  onBookmarkAdd?: (dir: string) => void;
  onBookmarkRemove?: (dir: string) => void;
}

function shortPath(p: string): string {
  const home = p.match(/^\/Users\/[^/]+/) ?? p.match(/^\/home\/[^/]+/);
  if (home) return "~" + p.slice(home[0].length);
  return p;
}

export default function DirectoryPicker({
  directory,
  bookmarks,
  onChange,
  onPickDirectory,
  onBookmarkAdd,
  onBookmarkRemove,
}: Props) {
  const isBookmarked = directory ? bookmarks.includes(directory) : false;

  return (
    <div className="flex items-center gap-1 min-w-0">
      <button
        onClick={onPickDirectory}
        title={directory || "Choose directory"}
        className="text-xs text-[var(--text-muted)] hover:text-[var(--text-main)] bg-[var(--bg-active)] rounded px-2 py-1 truncate max-w-[200px] text-left"
      >
        {directory ? shortPath(directory) : "Choose directory…"}
      </button>

      {/* Bookmark toggle for current directory */}
      {directory && onBookmarkAdd && onBookmarkRemove && (
        <button
          onClick={() =>
            isBookmarked ? onBookmarkRemove(directory) : onBookmarkAdd(directory)
          }
          title={isBookmarked ? "Remove bookmark" : "Bookmark this directory"}
          className={`text-xs px-1.5 py-1 rounded transition-colors ${
            isBookmarked
              ? "text-yellow-400 hover:text-[var(--text-muted)]"
              : "text-[var(--text-dim)] hover:text-yellow-400"
          }`}
        >
          ★
        </button>
      )}

      {/* Bookmarks dropdown */}
      {bookmarks.length > 0 && (
        <div className="flex items-center gap-0.5 overflow-x-auto max-w-[160px]">
          {bookmarks.map((b) => (
            <button
              key={b}
              onClick={() => onChange(b)}
              title={b}
              className={`text-xs px-1.5 py-1 rounded flex-shrink-0 truncate max-w-[80px] transition-colors ${
                b === directory
                  ? "text-[var(--accent-blue)] bg-[var(--bg-active)]"
                  : "text-[var(--text-muted)] hover:text-[var(--text-main)] hover:bg-[var(--bg-active)]"
              }`}
            >
              {shortPath(b).split("/").pop() || shortPath(b)}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
