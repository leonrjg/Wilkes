# Spec: In-app "Ask the documents" chat via ACP

Status: **spec only — not yet implemented.**

A chat pane inside Wilkes that lets the user ask an LLM questions about one or
more open documents, without leaving the app or pasting paths. It drives the
user's own **Claude Code** and **Codex** CLIs (their subscription plans — no API
keys, no external chat API), through a **single transport**:
the [Agent Client Protocol (ACP)](https://agentclientprotocol.com).

---

## 1. Invariant

**Wilkes is an ACP *client*; each CLI is an ACP *agent* subprocess. There is
exactly one integration mechanism, one context-injection mechanism, and one
permission boundary — shared by all supported agents.**

Concretely, three single-owner rules that this spec must never violate:

1. **One transport.** All agent I/O is ACP JSON-RPC over the subprocess's
   stdin/stdout. No screen-scraping a TUI, no per-agent bespoke JSON schema.
   The *only* per-agent difference is the launch command (§5).

2. **Mandatory context is pushed, never pulled.** "Which documents, which page"
   is truth that Wilkes owns. Wilkes prepends it to every `session/prompt`
   deterministically. The agent is **never** relied upon to call a tool to
   discover the current file — that would put a required invariant behind the
   model's discretion, which is not an invariant at all (see §6). Tools exist
   only for *optional*, agent-initiated pulls (full document text, corpus
   search).

3. **One read-only boundary, enforced client-side.** Write and shell
   capabilities are denied at the Wilkes ACP-client layer, independent of each
   CLI's own flags. A Q&A pane cannot edit files or run commands (§8).

The chat feature adds *no new document model*. "A document in context" is a
path plus (optionally) a page — it reuses the existing `MatchRef` /
`SourceOrigin::PdfPage { page, .. }` location model and the existing
`ExtractedContent { text, source_map }` extraction. Reading a document for the
agent is the same extraction that already backs search; there is no second
text-extraction path.

---

## 2. Why ACP (and not the alternatives)

Assessed and rejected:

| Option | Verdict |
|---|---|
| Spawn interactive TUI, parse the rendered screen | Rejected. Human-facing ANSI UI; breaks every release; no structured turn/tool/permission events. |
| One-shot `claude -p` / `codex exec` per message + `--resume` | Works, but multiple resume mechanisms and JSON schemas, plus cold-start per turn. Violates the single-mechanism invariant. Kept only as the per-agent fallback in §5. |
| Reverse-MCP (Wilkes runs an MCP server the agent connects back to for context) | This is how Claude Code's own IDE extensions get editor context — but it is Claude-specific and, worse, makes *mandatory* context a *pull*. Used here only for the optional verbs, not for current-file context. |
| **ACP** | **Chosen.** One JSON-RPC protocol; Claude/Codex are launched through ACP adapters (§5). Maps 1:1 to a chat pane: `session/new` → `session/prompt` per turn → streamed `session/update`. |

ACP verified surface (protocol v1, Rust SDK crate `agent-client-protocol` 1.0.1):

- **Handshake:** `initialize { protocolVersion, clientCapabilities, clientInfo }`
  → `{ protocolVersion, agentCapabilities, authMethods }`.
- **Session:** `session/new { cwd, mcpServers[] }` → `{ sessionId }`.
- **Turn:** `session/prompt { sessionId, prompt: ContentBlock[] }` → `{ stopReason }`
  where `stopReason ∈ { end_turn, max_tokens, max_turn_requests, refusal, cancelled }`.
- **Streaming (agent → client notifications):** `session/update` with
  `sessionUpdate ∈ { agent_message_chunk, tool_call, tool_call_update, plan, usage_update }`.
- **Cancel:** `session/cancel { sessionId }` → turn ends with `cancelled`.
- **Client-implemented callbacks (agent → client requests):**
  `fs/read_text_file { sessionId, path, line?, limit? }` → `{ content }`;
  `fs/write_text_file { … }`; `session/request_permission { sessionId, toolCall, options[] }`
  → `{ outcome: { selected, optionId } | cancelled }`.
- **ContentBlock kinds:** `text`, `resource { uri, mimeType, text }`,
  `resource_link`, `image`.

---

## 3. Architecture

Wilkes already has every seam this needs; the design *extends* them.

- **Backend event streaming.** `crates/api/src/context.rs` defines the
  `EventEmitter` trait; `crates/desktop/src/lib.rs:212` implements it as
  `TauriEmitter(AppHandle)` over `app.emit(...)`, and the `server` crate
  implements it over a broadcast channel. Agent output streams out through this
  *same* trait — no new streaming machinery, and the chat works in both the
  desktop and `server` builds for free.

- **Per-id request/stream pattern.** `ui/src/services/tauri.ts` already runs the
  exact pattern this needs for search: the frontend generates an id, registers
  `listen(`search-result-${id}`)` / `listen(`search-complete-${id}`)`, *then*
  `invoke("search", { … })`. Chat mirrors it precisely with
  `chat/update-${turnId}` (see §7).

- **Command registration.** New `#[tauri::command]`s are added to the
  `invoke_handler![…]` list in `crates/desktop/src/lib.rs:572` and delegate into
  `wilkes_api`, exactly like `preview`, `open_file`, etc.

### 3.1 New pieces

```
crates/agent/                     ← new workspace member (ACP client + session mgr)
  src/
    lib.rs                        ← AgentBackend enum, launch config
    client.rs                     ← impl of the ACP Client role (fs/*, permission)
    session.rs                    ← ChatSession: one subprocess = one chat
    context.rs                    ← builds the pushed context block (§6)
    reader.rs                     ← path → Wilkes ExtractedContent (§6.3)
crates/api/src/commands/chat.rs   ← tauri-facing verbs, delegate to crates/agent
ui/src/components/ChatPane.tsx     ← the pane (mirrors PreviewPane placement)
ui/src/services/chat.ts            ← invoke + listen wrapper (mirrors tauri.ts)
ui/src/stores/chat.ts              ← messages, streaming state, context files
```

`crates/agent` depends on `wilkes_core` (for `ExtractedContent`,
`ExtractorRegistry`, `SourceMap`) and on the `agent-client-protocol` crate. It is
UI-framework-agnostic; the desktop/server crates own the `EventEmitter` wiring.

### 3.2 Data flow (one turn)

```
ChatPane ──invoke("chat_send",{sessionId,turnId,text})──▶ chat.rs ──▶ ChatSession
                                                                          │
                        context.rs prepends pushed block ────────────────┤
                                                                          ▼
                                                    session/prompt {sessionId, prompt[]}
                                                                          │ stdio
ChatPane ◀── emit("chat/update-<turnId>", …) ◀── EventEmitter ◀── session/update ── AGENT
   ▲                                                                      │ fs/read_text_file
   │                                                          reader.rs ──┤ (agent pulls a doc)
   │                                                     ExtractedContent │
   └───────── stopReason=end_turn closes the turn ◀───────────────────────┘
```

---

## 4. Session & lifecycle model

- **One long-lived agent subprocess per chat session** (not per message). This
  is what gives real multi-turn memory with zero resume juggling — the agent
  holds conversation state in-process.
- A `ChatSession` owns: the child process, the ACP connection, the `sessionId`,
  the ordered set of **context files**, and a cancel token per in-flight turn.
- Sessions live in `AppContext`-managed state (a `Mutex<HashMap<String, ChatSession>>`),
  mirroring how `ActiveSearches` is managed (`crates/desktop/src/lib.rs:563`).
- Switching the active agent (Claude ↔ Codex) starts a **new**
  `ChatSession` (new subprocess). Conversations do not migrate across agents;
  the pane shows a fresh thread. This keeps each backend's memory authoritative
  and avoids replaying history into a foreign agent.
- On app exit / pane close: `session/cancel` any in-flight turn, then kill the
  child. Handled in the existing exit path (`handle_exit_event`).

---

## 5. Agent backends (the *only* per-agent difference)

A single enum + launch table. Everything downstream is identical.

```rust
enum AgentBackend { ClaudeCode, Codex, Nanocoder }
```

| Backend | Launch (ACP mode) | Auth = user's plan | Notes |
|---|---|---|---|
| **Claude Code** | installed npm package `@agentclientprotocol/claude-agent-acp`, bin `claude-agent-acp` | local Claude Code login / subscription | Wilkes installs missing adapters only after an explicit user action. |
| **Codex** | installed npm package `@agentclientprotocol/codex-acp`, bin `codex-acp` | ChatGPT/Codex subscription login | Subscription auth failure is a `chat_start` runtime error, not availability. |
| **Nanocoder** | installed npm package `@nanocollective/nanocoder`, bin `nanocoder --acp --provider Ollama` | configured Ollama provider | PATH-only Nanocoder installs are intentionally ignored. |

**Discovery / preflight.** On pane open or backend dropdown open, Wilkes probes
each configured backend for an already installed npm package and resolvable bin
target. Missing packages are shown disabled with an explicit Install action. No
silent fallback between backends — the user picks.

---

## 6. Context injection — the load-bearing design

Two categories, split by *who must guarantee delivery*.

### 6.1 Pushed context (mandatory, every turn, deterministic)

Built by `crates/agent/src/context.rs` and prepended as the **first `text`
ContentBlock** of every `session/prompt`. The user's message follows as a second
block. The agent cannot miss it because it is literally in the turn.

```
<wilkes-context>
You are answering questions inside Wilkes, a document-search desktop app.
Answer about the documents below. You have READ-ONLY access; you cannot edit
files or run commands. To read a document, request it via the file API — Wilkes
returns clean extracted text (page-mapped for PDFs), not raw bytes.

Open document: /Users/…/wilkes-paper.pdf  (page 12 of 40)
Documents in context:
  - /Users/…/wilkes-paper.pdf        (40 pages)
  - /Users/…/appendix.pdf            (added this turn)     ← 8 pages
</wilkes-context>

<user message text>
```

Rules:
- **Only the first turn** carries the full "you are inside Wilkes…" preamble.
  Later turns carry just the compact state (open doc, page, context list, and a
  `← added this turn` marker for deltas). This keeps token cost low while the
  invariant (current context is always present) holds every turn.
- The block is generated from `ChatSession` state that the **frontend** updates
  via explicit verbs (§7) — e.g. `chat_add_context(path)` when the user clicks
  "Ask about this file", and an automatic update when the active `PreviewPane`
  document/page changes. The backend is the single source of truth for what the
  block says.

### 6.2 Pulled context (optional, agent-initiated)

Paths in the pushed block are *references*, not contents. The agent decides when
it actually needs the text and pulls it. Two channels, both client-owned:

- **`fs/read_text_file` (primary).** The agent's natural "read this file" action.
  Wilkes implements the client side and routes it through §6.3 — so even the
  agent's own built-in read yields Wilkes-extracted text, including for PDFs the
  CLI could not otherwise parse page-by-page.
- **MCP verbs (richer).** A small Wilkes MCP server passed to every agent via
  `session/new.mcpServers` (stdio). Verbs:
  - `wilkes_search(query, mode?)` — semantic/keyword search over the indexed
    corpus, returning `MatchRef`-shaped hits (reuses `commands/search`).
  - `get_document_text(path, page?, page_range?)` — explicit page-scoped
    fetch when the agent wants a specific page rather than the whole file.
  - `list_context()` — the current context set (redundant with the pushed
    block, provided only so an agent that wants to re-confirm can).

  The MCP server is the same binary/process serving over stdio; because ACP
  passes `mcpServers` per session, one config line wires it into all supported
  agents identically.

### 6.3 The reader (`crates/agent/src/reader.rs`)

`fs/read_text_file` and `get_document_text` both resolve here:

```
path ──▶ ExtractorRegistry.find(path) ──▶ ContentExtractor.extract(path)
                                             → ExtractedContent { text, source_map, .. }
```

- **Text files:** return `text` (honoring ACP `line`/`limit` when present).
- **PDFs:** `SourceMap.segments` map every byte range in `text` to
  `SourceOrigin::PdfPage { page, .. }`. For a page request, select the segments
  whose `origin.page == page` and return their concatenated `text` slice; for a
  whole-file read, return `text` verbatim. **This is the reason paths beat
  prompt-stuffing:** the CLIs cannot page-scope a PDF, Wilkes already can, and
  no PDF bytes ever cross the wire.
- Char-boundary safety: page slicing uses the byte ranges recorded by the
  extractor (already char-aligned segment boundaries), never ad-hoc `&s[..n]`
  (per the Rust guideline in `AGENTS.md`).
- Reads are confined to files in the session's context set plus, for
  `wilkes_search` hits, files under the indexed roots (reuse `path::is_under`).
  Anything else is refused (§8).

---

## 7. Frontend: UI / UX

The chat pane is an **additional, toggleable, right-docked column** — a peer of
the existing panes, not a replacement for any of them. It reuses the exact
layout, toggle, resize, and styling machinery already in `App.tsx` for the
bookmarks pane, so it looks and behaves like a native part of the app rather
than a bolted-on chat box.

### 7.1 Placement, toggle & layout

- **Toggle button (split button).** A new control is added to `SearchBar`'s
  `settingsSlot` (`ui/src/App.tsx:181`), immediately left of the Bookmarks
  button. It is a **split button**: a 32×32 `MessageSquare` toggle (identical
  class string + `fill="currentColor"`-when-open affordance to the Bookmarks
  button, `App.tsx:188`) with a narrow (~14px) caret segment attached on its
  right edge, sharing the same border/rounded styling so it reads as one control.
  - **Main segment (icon):** toggles the pane using the **preferred backend**
    (`chat_backend`, §7.10) — the common path, one click.
  - **Caret segment:** opens a small dropdown menu listing the supported backends —
    a shortcut to *open the pane directly on a specific agent* without first
    opening it and changing the in-pane selector.
- **Backend dropdown.** The menu reuses `chat_list_backends()` exactly like the
  in-pane selector (§7.3): each row is `● Claude Code` / `○ Codex`
  with the same green/grey **status dot**, and unavailable agents are disabled
  with their `auth_note` tooltip. Picking one **opens the pane (if closed) and
  starts/switches to that backend's session** (§4). This is a **transient**
  launch choice for the current session — like the in-pane selector, it does
  **not** rewrite the persisted `chat_backend`; the preferred default is changed
  only in Settings (§7.10). The preferred backend is marked (e.g. a "Default"
  tag) so the two notions stay visually distinct. Menu styling follows the
  existing `ContextMenu` component (`ui/src/components/ContextMenu.tsx`).
- **Column.** The pane renders as a right-docked column at the end of the main
  flex row (`App.tsx:247`), after `PreviewPane` and after the right-docked
  bookmarks column. It reuses the `bookmarksColumn` pattern verbatim: a
  `startResize` drag handle + a `flex-shrink-0` fixed-width div. Width lives in
  `chatWidth` state (default **380px**, min **320**, max `window.innerWidth *
  0.5`), mirroring `bookmarksWidth`.
- **Coexistence.** Chat, bookmarks, and preview can all be open at once; the row
  simply becomes `sidebar │ preview │ [bookmarks] │ [chat]`. On narrow windows
  the resize clamps keep preview usable. Chat is right-dock-only for v1 (no
  left/bottom dock — bookmarks already covers left); revisit only if requested.
- **Store.** A `useChatStore` (zustand) mirrors `useBookmarksStore`:
  `paneOpen`, `togglePane`, plus session/message/context state (§7.9). Pane-open
  is UI state (not persisted); the **preferred backend is** persisted in
  `Settings` (new `chat_backend` field, default **Claude Code**) — see §7.10.

### 7.2 Pane anatomy

Top to bottom, echoing `BookmarksPane`'s header idiom (title left, controls
right; icons from `react-feather`; tokens `--bg-*` / `--text-*` / `--border-*`):

```
┌────────────────────────────────────────────┐
│ Ask            [● Claude ▾]   [＋] [⤢] [✕]  │  ← header: title · agent selector · new/dock/close
├────────────────────────────────────────────┤
│  📄 wilkes-paper.pdf · p.12  ✕   ← open doc │  ← context strip (chips; open doc highlighted)
│  📄 appendix.pdf  ✕      ＋ Add current      │
├────────────────────────────────────────────┤
│                                            │
│  You                                       │
│    How does §3 handle multi-page matches?  │  ← user turn (right-labeled, --bg-card bubble)
│                                            │
│  Claude                                    │
│    ┌ 📄 Reading wilkes-paper.pdf p.3 ✓ ┐   │  ← tool-call chip (from tool_call updates)
│    It resolves the range across segments   │  ← assistant markdown, streamed live
│    on the first overlapping page (p.12).▍  │     ▍ = typing caret while streaming
│                                            │
├────────────────────────────────────────────┤
│ ┌────────────────────────────────────────┐ │
│ │ Ask about these 2 documents…           │ │  ← composer (auto-grow textarea)
│ └────────────────────────────────────────┘ │
│ Answering about 2 documents        [ Send ]│  ← context hint · Send/Stop
└────────────────────────────────────────────┘
```

### 7.3 Agent selector & session switching

- The header selector lists the supported backends from `chat_list_backends()`. Each
  row shows a **status dot** — green (ready), grey (not installed / not logged
  in, disabled, tooltip = its `auth_note` from §5). No silent fallback: an
  unavailable agent is simply unpickable.
- **On pane open**, the selector is initialized from the preferred backend
  (`chat_backend`, §7.10), falling back to the first *available* one only if the
  preferred is not ready. It is also driven by the header split-button dropdown
  (§7.1): opening the pane via a specific agent there sets this selector to match.
- **Switching the selector is transient** — it changes the *current session's*
  agent, not the persisted default. Neither the selector nor the header dropdown
  writes `chat_backend`; the persisted preference has a **single owner, the
  Settings panel (§7.10)**. (This keeps a one-off backend trial from
  silently becoming the permanent default.)
- Selecting a different backend starts a **new** `ChatSession` (§4). If the
  current thread is non-empty, a small inline confirm ("Switching to Codex
  starts a new conversation — the current thread will be cleared. Continue?")
  guards it. The **[＋] "New chat"** control does the same reset on the current
  backend.

### 7.4 Context strip (the visible half of §6.1)

The strip is the user-facing surface of the pushed context set — what the strip
shows is exactly what the pushed block will say, so context is never invisible.

- One **chip per document** in the session's context set: filename + page badge
  (for PDFs) + ✕ (calls `chat_remove_context`).
- The **open document** chip is highlighted (`--accent-blue-muted` background)
  and shows the live page (`· p.12`). It updates automatically: when
  `PreviewPane` changes doc or page, the frontend calls
  `chat_set_active_doc(...)` — silent, no message, just the badge moving. This is
  the visible proof that "current file + page" is pushed every turn without the
  user or model doing anything.
- **`＋ Add current`** adds the doc currently open in `PreviewPane`. Documents
  also enter via two existing surfaces, reused (no new mechanism):
  - **File context menu** — an "Ask about this file" item appended in
    `ui/src/lib/fileActions.ts` (the single composition point already used by the
    Zotero integration). Opens the pane if closed, adds the chip, focuses the
    composer.
  - **Reader text selection** — an "Ask about selection" action alongside the
    existing "Add bookmark" affordance; adds the file to context and seeds the
    composer with the quoted text.

### 7.5 Transcript rendering

- **User turns:** plain text in a `--bg-card` bubble, labeled "You".
- **Assistant turns:** markdown-rendered (reuse the app's existing markdown
  path; code/quotes styled with the same tokens), streamed token-by-token from
  `agent_message_chunk`. A caret (▍) shows while the turn is open.
- **Tool activity:** each `tool_call` renders as a compact chip inside the
  assistant turn ("📄 Reading appendix.pdf p.3"), flipping to ✓ on the matching
  `tool_call_update` (`completed`) or ✗ on error. Chips are muted
  (`--bg-active` / `--text-muted`) so they read as activity, not content.
- **Follow-along (reuses existing navigation).** ACP `tool_call.locations`
  carry `{ path, line }`, and answers cite pages (`p.12`). Both become clickable:
  clicking a tool chip or a page citation navigates `PreviewPane` to that
  doc/page through the **existing `selectMatch` path** (same call bookmarks and
  search results use) — no second navigation mechanism.
- Long transcripts virtualize with `@tanstack/react-virtual`, already a
  dependency (used by `BookmarksPane`).

### 7.6 Composer & keyboard

- Auto-growing `textarea`. **Enter** sends; **Shift+Enter** newlines;
  **Esc** blurs. While a turn streams, the **Send** button becomes **Stop**
  (`chat_cancel` → `session/cancel`), and the composer stays editable so the next
  question can be typed.
- A one-line hint under the composer states the context scope ("Answering about
  2 documents") so the user always knows what the model can see before sending.
- Empty-send and send-with-no-context are both allowed (a general question is
  valid); send-with-no-agent-ready is blocked with the setup hint (§7.7).

### 7.7 Pane states

| State | Rendering |
|---|---|
| **No agent available** | A setup card listing the supported backends with their `auth_note` and a **Recheck** button (`chat_list_backends`). Composer disabled. |
| **Ready, empty thread** | Context strip + a muted hint ("Ask a question about the documents above") + composer focused. |
| **Streaming** | Assistant bubble with live text + caret; tool chips appear as they arrive; Stop button. |
| **Agent error / exit** | An inline error row ("Codex error — check `codex` login"), a **Retry** action, and the turn closed. Logged (§9). |
| **Permission prompt** | Normally never shown: §8 auto-allows scoped reads and auto-denies writes/exec. If one is ever surfaced, it renders as a compact inline row with the tool title and the auto-decision noted — informational, not a modal. |

### 7.8 IPC surface

New Tauri commands (registered in `invoke_handler!` at
`crates/desktop/src/lib.rs:572`, delegating to `wilkes_api::commands::chat`):

| Command | Purpose |
|---|---|
| `chat_list_backends()` | Available/authenticated agents + `auth_note` + status. |
| `chat_start(backend) → sessionId` | Spawn subprocess, `initialize` + `session/new`. |
| `chat_add_context(sessionId, path)` | Add a document to the context set. |
| `chat_remove_context(sessionId, path)` | Remove one. |
| `chat_set_active_doc(sessionId, path, page?)` | Called when `PreviewPane` changes; updates the "Open document" line. |
| `chat_send(sessionId, turnId, text)` | Build prompt (§6.1) + `session/prompt`; stream via events. |
| `chat_cancel(sessionId, turnId)` | `session/cancel`. |
| `chat_close(sessionId)` | Kill subprocess. |

Streaming events (via `EventEmitter`, mirroring the per-id search channels in
`tauri.ts`): the frontend registers listeners *before* invoking `chat_send`.

| Event | Payload (from `session/update`) |
|---|---|
| `chat/update-<turnId>` | `{ kind: "text", delta }` from `agent_message_chunk` |
| `chat/update-<turnId>` | `{ kind: "tool", toolCallId, title, status, locations? }` from `tool_call` / `tool_call_update` |
| `chat/update-<turnId>` | `{ kind: "permission", … }` if ever surfaced (§8) |
| `chat/done-<turnId>` | `{ stopReason }` — unlistens, ends the turn |

### 7.9 New / touched frontend files

| File | Role |
|---|---|
| `ui/src/components/ChatPane.tsx` | The pane (header, context strip, transcript, composer). |
| `ui/src/stores/useChatStore.ts` | Session, messages, context set, `paneOpen` — mirrors `useBookmarksStore`. |
| `ui/src/services/chat.ts` | `invoke` + per-id `listen` wrapper — mirrors `tauri.ts`. |
| `ui/src/App.tsx` | Toggle button in `settingsSlot`; `chatColumn` in the flex row. |
| `ui/src/lib/fileActions.ts` | "Ask about this file" context-menu item. |
| `ui/src/components/preview/…` | "Ask about selection" action on reader selection. |

### 7.10 Preferred-backend setting

The default agent is a persisted user setting, reusing the existing `Settings`
plumbing exactly like `bookmarks_dock` (`crates/core/src/types.rs:666`) — no new
settings mechanism.

- **Rust (`crates/core/src/types.rs`).** Add to the `Settings` struct:

  ```rust
  #[serde(default)]
  pub chat_backend: AgentBackend,
  ```

  where the same `AgentBackend` enum used by the launch table (§5) derives
  `Default` with Claude as the default variant, matching the `BookmarkDock`
  idiom (`types.rs:743`):

  ```rust
  #[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
  pub enum AgentBackend {
      #[default]
      ClaudeCode,
      Codex,
  }
  ```

  `#[serde(default)]` on the field means existing `settings.json` files with no
  `chat_backend` key load as Claude — no migration needed. Add
  `chat_backend: AgentBackend::default()` to the `Settings` `Default` impl
  (`types.rs:701`).

- **TS mirror (`ui/src/lib/types.ts`).** Following the snake_case-shared idiom
  (`bookmarks_dock: BookmarkDock`):

  ```ts
  export type AgentBackend = "ClaudeCode" | "Codex";
  // in interface Settings:
  chat_backend: AgentBackend;
  ```

- **Patch path.** No new command: the value is read from `get_settings` and
  written through the existing `update_settings` patch merge
  (`crates/api/src/commands/settings.rs`), the same route `bookmarks_dock` uses.
  **This Settings `select` is the only writer** of `chat_backend`; the in-pane
  selector (§7.3) and the header dropdown (§7.1) read it as the initial value but
  make transient session-only changes, so there is exactly one owner of the
  persisted preference.

- **Settings UI.** A single-line **"Default chat agent"** `select` (Claude Code /
  Codex) added to `SettingsModal` — placed in the general/appearance
  panel next to the existing bookmarks-dock control. Unavailable backends are
  still selectable *as the preference* (the user may be about to log in); the
  live availability/greying is only enforced in the pane's own selector.

- **Default rationale.** Claude is the default because its ACP path
  (`@agentclientprotocol/claude-agent-acp`) is the most feature-complete of the
  supported adapters and its subscription auth is the most widely held among the
  target users. The user can change it at any time; nothing else in the spec
  depends on the specific default.

---

## 8. Permissions — the single read-only boundary

Owned entirely by the Wilkes ACP client (`crates/agent/src/client.rs`), so the
policy is identical for all supported agents and does not depend on each CLI's flags:

1. **Capability advertisement.** In `initialize`, Wilkes sets
   `clientCapabilities.fs.readTextFile = true`, `fs.writeTextFile = false`,
   `terminal = false`. Agents that honor capabilities will not even attempt
   writes or shell.
2. **Request interception.** `session/request_permission` for any write/exec/
   network tool is auto-answered with a `reject_once`-selected outcome and
   logged (never silently — per project rule "always log exceptions"). Read
   tools scoped to allowed paths (§6.3) are auto-allowed.
3. **`fs/write_text_file`** is implemented as an explicit error, not a no-op, so
   an agent that ignores the capability flag still cannot write.
4. **Defense in depth (belt & suspenders).** Where the CLI also offers a native
   restriction, set it too — e.g. Claude `--permission-mode`, Codex sandbox — but
   correctness does **not** rely on it. The client boundary is authoritative.

This makes read-only a structural property of one owner, not a behavior we hope
each agent respects.

---

## 9. Failure & edge handling (no silent suppression)

- Subprocess spawn failure / crash → `chat/done-<turnId>` with an error kind;
  pane shows "Codex error — check `codex` login". Logged.
- `initialize` version mismatch → negotiate down; if unsupported, disable that
  backend with a reason. Logged.
- Agent requests a path outside the allowed set → `fs/read_text_file` returns an
  error the agent sees; the denial is logged, not hidden.
- Extraction failure for a context doc → surfaced as a tool error to the agent
  and a chip in the pane; the turn continues.
- Cancel mid-stream → `session/cancel`, drain until `cancelled` stopReason,
  unlisten.

---

## 10. Build order

1. **`crates/agent` skeleton + Claude Code ACP adapter**, wired to a minimal
   `ChatPane` — proves handshake → prompt → streamed `session/update` end to end.
2. **`reader.rs` + `fs/read_text_file`** over `ExtractedContent` (text first,
   then PDF page-scoping via `source_map`).
3. **Pushed context block** (§6.1) + `chat_set_active_doc` hook from `PreviewPane`.
4. **Permission boundary** (§8).
5. **Codex backend** (`codex-acp`) — de-risk auth; wire the `ChatAgent` fallback
   trait if the adapter is not ready.
6. **MCP verbs** (`search`, `get_document_text`) via
   `session/new.mcpServers`.

Steps 1–4 deliver a working single-agent pane; 5 adds the second backend behind the
identical mechanism; 6 is the optional-pull enrichment.

---

## 11. Open questions for the user

- **Pane placement:** ~~replace/split with `PreviewPane`, or a separate toggled
  panel?~~ **Decided:** an additional right-docked, toggleable pane, peer to
  bookmarks/preview and open-able alongside them (§7.1).
- ~~**Default backend** when multiple are authenticated.~~ **Decided:**
  persisted `chat_backend` setting, default **Claude Code** (§7.10).
- **`server` build:** ship chat there too (Codex subscription auth won't work
  remotely), or desktop-only for v1? (Assumed: desktop-first; the `EventEmitter`
  abstraction keeps `server` viable later.)
- **MCP corpus search in v1**, or ship with `fs/read_text_file` only and add the
  MCP verbs in a follow-up? (Assumed: `fs` read in v1, MCP verbs as step 7.)
