import { Tooltip } from "@leonrjg/wilkes-reader";
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { Check, Plus, RefreshCw, Square, Trash2, X } from "react-feather";
import type { BrowseRequest, Feed, FeedItem } from "../lib/types";
import { configuredLibraryRoots } from "../lib/configuredRoots";
import { formatTimestampFullDate } from "../lib/dateFormatting";
import { confirmDialog } from "../lib/utils/dialog";
import { listingSource, useFeedsStore } from "../stores/useFeedsStore";
import { useToasts } from "./Toast";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useViewerStore } from "../stores/useViewerStore";
import CatalogueCandidate from "./CatalogueCandidate";

/**
 * The feeds a workspace follows and the one inbox they fill.
 *
 * A feed watches a blog, a page of links or a provider's listing, and says
 * only what is new; an item comes into the library when it is imported from
 * here, or straight away when its feed's auto-import switch is on. Nothing in
 * the inbox is in the library yet.
 */
export default function FeedsView({ readOnly }: { readOnly: boolean }) {
  const load = useFeedsStore((s) => s.load);
  const feeds = useFeedsStore((s) => s.feeds);
  const error = useFeedsStore((s) => s.error);
  const refreshing = useFeedsStore((s) => s.refreshing);
  const refresh = useFeedsStore((s) => s.refresh);
  const stop = useFeedsStore((s) => s.stop);
  const [adding, setAdding] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  useEffect(() => {
    void load();
  }, [load]);

  const act = async (action: () => Promise<unknown>) => {
    try {
      setActionError(null);
      await action();
    } catch (failure) {
      console.error("feeds:", failure);
      setActionError(`${failure}`);
    }
  };

  return (
    <div className="flex flex-col gap-1.5 pt-2">
      <div className="flex items-center gap-1.5">
        {!readOnly && (
          <SmallButton onClick={() => setAdding(!adding)} pressed={adding} label="Add a feed">
            <Plus size={11} /> Add
          </SmallButton>
        )}
        {!readOnly && feeds.length > 0 && (
          refreshing ? (
            <SmallButton onClick={() => void act(stop)} label="Stop refreshing">
              <Square size={10} /> Stop
            </SmallButton>
          ) : (
            <SmallButton onClick={() => void act(refresh)} label="Refresh every feed now">
              <RefreshCw size={11} /> Refresh now
            </SmallButton>
          )
        )}
      </div>
      {adding && <AddFeedForm onDone={() => setAdding(false)} />}
      {error !== null && <Failure>The feeds could not be read: {error}</Failure>}
      {actionError !== null && <Failure>{actionError}</Failure>}
      {feeds.length === 0 ? (
        !adding && (
          <Hint>
            No feeds yet. Add a blog's address, or use “Follow” on a filtered listing to be told
            of what it adds.
          </Hint>
        )
      ) : (
        <>
          <FeedList readOnly={readOnly} />
          <Inbox readOnly={readOnly} />
        </>
      )}
    </div>
  );
}

/** A blog by its address: the feed itself, or a page that advertises one. */
function AddFeedForm({ onDone }: { onDone: () => void }) {
  const add = useFeedsStore((s) => s.add);
  const directory = useSettingsStore((s) => s.directory);
  const favorites = useSettingsStore((s) => s.favorites);
  const recentDirs = useSettingsStore((s) => s.recentDirs);
  const roots = useMemo(
    () => configuredLibraryRoots({ directory, favorites, recentDirs }),
    [directory, favorites, recentDirs],
  );
  const [url, setUrl] = useState("");
  const [name, setName] = useState("");
  const [root, setRoot] = useState(directory || roots[0] || "");
  const [autoImport, setAutoImport] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  if (roots.length === 0) {
    return <Hint>Choose a library directory first: a feed's posts are written into one.</Hint>;
  }

  return (
    <form
      className="flex flex-col gap-1.5 rounded border border-[var(--border-main)] bg-[var(--bg-app)] p-2"
      onSubmit={(event) => {
        event.preventDefault();
        if (url.trim() === "" || busy) return;
        setBusy(true);
        setFailure(null);
        add({
          source: { kind: "syndication", url: url.trim() },
          name: name.trim() === "" ? null : name.trim(),
          root,
          auto_import: autoImport,
        })
          .then(onDone)
          .catch((error) => setFailure(`${error}`))
          .finally(() => setBusy(false));
      }}
    >
      <input
        value={url}
        onChange={(event) => setUrl(event.target.value)}
        placeholder="Blog or feed address"
        aria-label="Blog or feed address"
        autoFocus
        className="rounded border border-[var(--border-main)] bg-[var(--bg-sidebar)] px-1.5 py-1 text-[11px] text-[var(--text-main)] outline-none"
      />
      <input
        value={name}
        onChange={(event) => setName(event.target.value)}
        placeholder="Name (the feed's own when empty)"
        aria-label="Feed name"
        className="rounded border border-[var(--border-main)] bg-[var(--bg-sidebar)] px-1.5 py-1 text-[11px] text-[var(--text-main)] outline-none"
      />
      {roots.length > 1 && (
        <label className="flex items-center gap-1.5 text-[10px] text-[var(--text-dim)]">
          Into
          <select
            value={root}
            onChange={(event) => setRoot(event.target.value)}
            aria-label="Library the feed writes into"
            className="min-w-0 flex-1 rounded border border-[var(--border-main)] bg-[var(--bg-sidebar)] px-1 py-0.5 text-[10px] text-[var(--text-main)]"
          >
            {roots.map((candidate) => (
              <option key={candidate} value={candidate}>
                {candidate}
              </option>
            ))}
          </select>
        </label>
      )}
      <label className="flex items-center gap-1.5 text-[10px] text-[var(--text-dim)]">
        <input
          type="checkbox"
          checked={autoImport}
          onChange={(event) => setAutoImport(event.target.checked)}
        />
        Import new posts straight away
      </label>
      {failure !== null && <Failure>{failure}</Failure>}
      <div className="flex justify-end gap-2 text-[10px]">
        <button type="button" onClick={onDone} className="text-[var(--text-dim)] hover:text-[var(--text-main)]">
          Cancel
        </button>
        <button
          type="submit"
          disabled={busy || url.trim() === ""}
          className="font-bold text-[var(--accent-blue)] hover:underline disabled:opacity-50"
        >
          {busy ? "Adding…" : "Add feed"}
        </button>
      </div>
    </form>
  );
}

function FeedList({ readOnly }: { readOnly: boolean }) {
  const feeds = useFeedsStore((s) => s.feeds);
  const filter = useFeedsStore((s) => s.filter);
  const setFilter = useFeedsStore((s) => s.setFilter);
  return (
    <div role="list" aria-label="Feeds" className="flex flex-col divide-y divide-[var(--border-main)]">
      {feeds.map((feed) => (
        <FeedRow
          key={feed.id}
          feed={feed}
          selected={filter === feed.id}
          onSelect={() => void setFilter(filter === feed.id ? null : feed.id)}
          readOnly={readOnly}
        />
      ))}
    </div>
  );
}

/** When something happened, in words: "3 h ago", or the date for anything
 *  older than a day. */
function ago(ms: number): string {
  const minutes = Math.round((Date.now() - ms) / 60000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  if (minutes < 24 * 60) return `${Math.round(minutes / 60)} h ago`;
  return formatTimestampFullDate(ms) ?? new Date(ms).toISOString().slice(0, 10);
}

function FeedRow({
  feed,
  selected,
  onSelect,
  readOnly,
}: {
  feed: Feed;
  selected: boolean;
  onSelect: () => void;
  readOnly: boolean;
}) {
  const update = useFeedsStore((s) => s.update);
  const remove = useFeedsStore((s) => s.remove);
  const [failure, setFailure] = useState<string | null>(null);
  const [moving, setMoving] = useState(false);
  const status =
    feed.last_success_at_ms === null
      ? feed.last_attempt_at_ms === null
        ? "Not refreshed yet"
        : "Never refreshed successfully"
      : `Refreshed ${ago(feed.last_success_at_ms)}`;
  const pausedUntil =
    feed.retry_after_ms !== null && feed.retry_after_ms > Date.now() ? feed.retry_after_ms : null;

  return (
    <div role="listitem" className="flex flex-col gap-0.5 py-1.5 text-[10px] leading-snug">
      <div className="flex items-center gap-1.5">
        <button
          type="button"
          onClick={onSelect}
          aria-pressed={selected}
          aria-label={`Show ${feed.name}'s inbox`}
          className={`min-w-0 flex-1 truncate text-left text-[11px] font-bold ${
            selected ? "text-[var(--accent-blue)]" : "text-[var(--text-main)]"
          } hover:underline`}
        >
          {feed.name}
        </button>
        {feed.inbox_count > 0 && (
          <span className="shrink-0 rounded bg-[var(--bg-active)] px-1 tabular-nums text-[var(--text-main)]">
            {feed.inbox_count}
          </span>
        )}
        {!readOnly && (
          <>
            <Tooltip content="Import new items straight away, up to 20 a refresh">
              <label className="flex shrink-0 items-center gap-1 text-[var(--text-dim)]">
                <input
                  type="checkbox"
                  checked={feed.auto_import}
                  aria-label={`Auto-import ${feed.name}`}
                  onChange={(event) =>
                    void update(feed.id, { auto_import: event.target.checked }).catch((error) =>
                      setFailure(`${error}`),
                    )
                  }
                />
                Auto
              </label>
            </Tooltip>
            <Tooltip content="Remove this feed">
              <button
                type="button"
                aria-label={`Remove ${feed.name}`}
                onClick={async () => {
                  if (!(await confirmDialog(`Stop following ${feed.name}? Its inbox is cleared.`))) return;
                  const deleteFiles = await confirmDialog(
                    `Also delete the files ${feed.name} imported, wherever they were put? Nothing else is touched.`,
                  );
                  await remove(feed.id, deleteFiles).catch((error) => setFailure(`${error}`));
                }}
                className="shrink-0 text-[var(--text-dim)] hover:text-red-400"
              >
                <Trash2 size={11} />
              </button>
            </Tooltip>
          </>
        )}
      </div>
      <p className="text-[var(--text-dim)]">
        {status} · into{" "}
        {readOnly ? (
          <span title={`${feed.root}/${feed.folder}`}>{feed.folder}</span>
        ) : (
          <Tooltip content={`${feed.root}/${feed.folder} — change where new posts go`}>
            <button
              type="button"
              aria-label={`Change where ${feed.name} imports`}
              onClick={() => setMoving((open) => !open)}
              className="hover:text-[var(--text-main)] hover:underline"
            >
              {feed.folder}
            </button>
          </Tooltip>
        )}
        {pausedUntil !== null && ` · paused by the site until ${ago(pausedUntil).replace(" ago", "")}`}
      </p>
      {moving && <MoveFeedForm feed={feed} onDone={() => setMoving(false)} />}
      {feed.may_have_missed && (
        <p className="text-amber-500">
          Every item the last refresh found was new, so older ones may have been missed.
        </p>
      )}
      {feed.last_error !== null && <Failure>{feed.last_error}</Failure>}
      {failure !== null && <Failure>{failure}</Failure>}
    </div>
  );
}

/** Where a feed's new imports go: a library of the workspace and a folder
 *  in it. What it imported before stays where it is. */
function MoveFeedForm({ feed, onDone }: { feed: Feed; onDone: () => void }) {
  const update = useFeedsStore((s) => s.update);
  const directory = useSettingsStore((s) => s.directory);
  const favorites = useSettingsStore((s) => s.favorites);
  const recentDirs = useSettingsStore((s) => s.recentDirs);
  const roots = useMemo(
    () => configuredLibraryRoots({ directory, favorites, recentDirs }),
    [directory, favorites, recentDirs],
  );
  // The feed's root is stored resolved; it is listed even when the
  // workspace names it another way.
  const choices = roots.includes(feed.root) ? roots : [feed.root, ...roots];
  const [root, setRoot] = useState(feed.root);
  const [folder, setFolder] = useState(feed.folder);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const unchanged = root === feed.root && folder.trim() === feed.folder;

  return (
    <form
      className="mt-1 flex flex-col gap-1.5 rounded border border-[var(--border-main)] bg-[var(--bg-app)] p-2"
      onSubmit={(event) => {
        event.preventDefault();
        if (busy || unchanged || folder.trim() === "") return;
        setBusy(true);
        setFailure(null);
        update(feed.id, {
          ...(root !== feed.root ? { root } : {}),
          ...(folder.trim() !== feed.folder ? { folder: folder.trim() } : {}),
        })
          .then(onDone)
          .catch((error) => setFailure(`${error}`))
          .finally(() => setBusy(false));
      }}
    >
      {choices.length > 1 && (
        <label className="flex items-center gap-1.5 text-[10px] text-[var(--text-dim)]">
          Into
          <select
            value={root}
            onChange={(event) => setRoot(event.target.value)}
            aria-label={`Library ${feed.name} writes into`}
            className="min-w-0 flex-1 rounded border border-[var(--border-main)] bg-[var(--bg-sidebar)] px-1 py-0.5 text-[10px] text-[var(--text-main)]"
          >
            {choices.map((candidate) => (
              <option key={candidate} value={candidate}>
                {candidate}
              </option>
            ))}
          </select>
        </label>
      )}
      <input
        value={folder}
        onChange={(event) => setFolder(event.target.value)}
        aria-label={`Folder ${feed.name} writes into`}
        className="rounded border border-[var(--border-main)] bg-[var(--bg-sidebar)] px-1.5 py-1 text-[11px] text-[var(--text-main)] outline-none"
      />
      <p className="text-[var(--text-dim)]">New posts go here; those already imported stay where they are.</p>
      {failure !== null && <Failure>{failure}</Failure>}
      <div className="flex justify-end gap-2 text-[10px]">
        <button type="button" onClick={onDone} className="text-[var(--text-dim)] hover:text-[var(--text-main)]">
          Cancel
        </button>
        <button
          type="submit"
          disabled={busy || unchanged || folder.trim() === ""}
          className="font-bold text-[var(--accent-blue)] hover:underline disabled:opacity-50"
        >
          Save
        </button>
      </div>
    </form>
  );
}

function Inbox({ readOnly }: { readOnly: boolean }) {
  const items = useFeedsStore((s) => s.items);
  const feeds = useFeedsStore((s) => s.feeds);
  const filter = useFeedsStore((s) => s.filter);
  const inboxTotal = useFeedsStore((s) => s.inboxTotal);
  const more = useFeedsStore((s) => s.more);
  const loadMore = useFeedsStore((s) => s.loadMore);
  const setFilter = useFeedsStore((s) => s.setFilter);
  const filtered = feeds.find((feed) => feed.id === filter);

  return (
    <section aria-label="Inbox" className="flex flex-col">
      <h4 className="mt-2 flex items-baseline gap-1.5 border-b border-[var(--border-strong)] pb-1 text-[10px] font-bold uppercase tracking-wider text-[var(--text-main)]">
        Inbox
        <span className="font-normal normal-case tracking-normal text-[var(--text-muted)]">
          {filtered ? `${filtered.name} · ${filtered.inbox_count}` : inboxTotal}
        </span>
        {filtered && (
          <button
            type="button"
            onClick={() => void setFilter(null)}
            aria-label="Show every feed's inbox"
            className="ml-auto font-normal normal-case tracking-normal text-[var(--text-dim)] hover:text-[var(--text-main)]"
          >
            All feeds
          </button>
        )}
      </h4>
      {items.length === 0 ? (
        <Hint>Nothing waiting. New items appear here when the feeds refresh.</Hint>
      ) : (
        <div className="divide-y divide-[var(--border-main)]">
          {items.map((item) => (
            <InboxRow
              key={item.id}
              item={item}
              feedName={feeds.find((feed) => feed.id === item.feed_id)?.name ?? ""}
              readOnly={readOnly}
            />
          ))}
        </div>
      )}
      {more && (
        <button
          type="button"
          onClick={() => void loadMore()}
          className="mt-1.5 self-start rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-2 py-0.5 text-[10px] font-bold uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-main)]"
        >
          Load more
        </button>
      )}
    </section>
  );
}

/** When an item is from: the date its source gave, or — never a guess — when
 *  Wilkes first saw it. */
function itemDate(item: FeedItem): string {
  if (item.published !== null) {
    return formatTimestampFullDate(Date.parse(item.published)) ?? item.published.slice(0, 10);
  }
  return `first seen ${formatTimestampFullDate(item.first_seen_at_ms) ?? ""}`.trim();
}

function InboxRow({ item, feedName, readOnly }: { item: FeedItem; feedName: string; readOnly: boolean }) {
  const importItem = useFeedsStore((s) => s.importItem);
  const dismiss = useFeedsStore((s) => s.dismiss);
  const importing = useFeedsStore((s) => s.importing[item.id] === true);
  const openFile = useViewerStore((s) => s.openFile);
  const [failure, setFailure] = useState<string | null>(item.last_error);
  const title = item.work.title ?? item.link;
  // A picture that could not be fetched is the one failure the user can
  // choose to go past.
  const missingPicture = failure !== null && failure.includes("the picture ");

  const run = (allowMissingImages: boolean) => {
    setFailure(null);
    importItem(item.id, allowMissingImages).catch((error) => setFailure(`${error}`));
  };

  const actions: ReactNode = readOnly ? undefined : (
    <>
      {!item.preview_only && (
        <Tooltip content="Import into the feed's folder">
          <button
            type="button"
            aria-label={`Import ${title}`}
            disabled={importing}
            onClick={() => run(false)}
            className="inline-flex h-[22px] items-center gap-1 rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-1.5 text-[10px] font-bold text-[var(--text-main)] hover:bg-[var(--bg-active)] disabled:opacity-50"
          >
            <Check size={11} /> {importing ? "Importing…" : "Import"}
          </button>
        </Tooltip>
      )}
      <Tooltip content="Dismiss: it will not come back">
        <button
          type="button"
          aria-label={`Dismiss ${title}`}
          disabled={importing}
          onClick={() => void dismiss(item.id).catch((error) => setFailure(`${error}`))}
          className="inline-flex h-[22px] w-[22px] items-center justify-center rounded text-[var(--text-dim)] hover:text-[var(--text-main)] disabled:opacity-50"
        >
          <X size={12} />
        </button>
      </Tooltip>
    </>
  );

  return (
    <div className="py-0.5">
      <CatalogueCandidate
        provider={`feed:${item.feed_id}`}
        work={item.work}
        source={[feedName, itemDate(item)].filter(Boolean).join(" · ")}
        refinable={false}
        actions={actions}
      />
      <div className="flex flex-col gap-0.5 pb-1 text-[10px] leading-snug">
        {item.preview_only && (
          <p className="text-amber-500">
            Preview only: the page shows the start of a paid post, so it is not imported.
          </p>
        )}
        {item.held && <p className="text-[var(--text-dim)]">Already in the library.</p>}
        {item.imported_path !== null && (
          <button
            type="button"
            onClick={() => openFile(item.imported_path!)}
            className="self-start text-[var(--accent-blue)] hover:underline"
          >
            Open the imported file
          </button>
        )}
        {failure !== null && (
          <Failure>
            {failure}
            {missingPicture && !readOnly && (
              <>
                {" "}
                <button
                  type="button"
                  onClick={() => run(true)}
                  className="font-bold text-[var(--accent-blue)] hover:underline"
                >
                  Import without it
                </button>
              </>
            )}
          </Failure>
        )}
      </div>
    </div>
  );
}

function SmallButton({
  children,
  onClick,
  label,
  pressed,
}: {
  children: ReactNode;
  onClick: () => void;
  label: string;
  pressed?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      aria-pressed={pressed}
      className="inline-flex items-center gap-1 rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-1.5 py-0.5 text-[10px] font-bold uppercase tracking-wider text-[var(--text-dim)] transition-colors hover:text-[var(--text-main)]"
    >
      {children}
    </button>
  );
}

function Hint({ children }: { children: ReactNode }) {
  return <p className="py-1.5 text-[10px] leading-snug text-[var(--text-muted)]">{children}</p>;
}

function Failure({ children }: { children: ReactNode }) {
  return <p className="text-[10px] leading-snug text-red-400">{children}</p>;
}

/**
 * Follow what is on screen: a provider's listing, or its answer to a keyword,
 * read newest first at every refresh. What it adds lands in Feeds, and is
 * written under the current directory.
 */
export function FollowButton({
  provider,
  name,
  request,
}: {
  provider: string;
  name: string;
  request: BrowseRequest;
}) {
  const add = useFeedsStore((s) => s.add);
  const directory = useSettingsStore((s) => s.directory);
  const { addToast } = useToasts();
  const [busy, setBusy] = useState(false);
  if (directory === "") return null;
  return (
    <Tooltip content={`Be told of what ${name} adds here, newest first, in Feeds`}>
      <button
        type="button"
        disabled={busy}
        aria-label={`Follow ${name}`}
        onClick={() => {
          setBusy(true);
          add({ source: listingSource(provider, request), root: directory })
            .then((feed) => addToast(`Following ${feed.name}. New works will appear in Feeds.`, { type: "info" }))
            .catch((error) => addToast(`Could not follow ${name}: ${error}`, { type: "error" }))
            .finally(() => setBusy(false));
        }}
        className="ml-auto font-normal normal-case tracking-normal text-[var(--accent-blue)] hover:underline disabled:opacity-50"
      >
        {busy ? "Following…" : "Follow"}
      </button>
    </Tooltip>
  );
}
