# Proposal: Zotero Integration for Wilkes

## 1. Guiding constraints (from the codebase)

The app already has the two extension points this feature needs, and the cleanest
design is to *extend* them rather than invent parallel machinery:

- **Backend capability registries** — `MetadataExtractorRegistry` in
  `crates/core/src/metadata/mod.rs` is a trait-object registry
  (`FileMetadataExtractor`). Integrations follow the same shape.
- **Context-menu composition** — `buildFileContextMenuItems()` in
  `ui/src/lib/fileActions.ts` is already the single composition point that
  assembles the file right-click menu, and `ResultList` is the only caller
  (`ui/src/components/ResultList.tsx`). This is where integration items are appended.
- **Settings** — one flat `Settings` struct (`crates/core/src/types.rs`) mirrored in
  `ui/src/lib/types.ts`, merged patch-wise by `update_settings`
  (`crates/api/src/commands/settings.rs`). Settings tabs are panels in
  `SettingsModal.tsx` (see `ExtensionsPanel`).
- **Command layering** — logic lives in `wilkes_core`, is wrapped as async fns in
  `wilkes_api::commands::*`, and exposed twice: as `#[tauri::command]` in
  `crates/desktop/src/lib.rs` and over HTTP in `crates/server`. Any new command must
  be added in both surfaces, and the shared `SearchApi` interface
  (`ui/src/services/api.ts`) is the frontend contract.

The single new architectural concept is the **integration**: an optional,
settings-gated capability that (a) has its own config block and (b) can contribute
right-click actions. Everything else reuses existing patterns.

**Decision (locked):** Feature 1 targets the **Zotero local API only**
(`http://127.0.0.1:23119`, Zotero 7+). No cloud/Web-API fallback is implemented, per
the project rule against unrequested fallbacks. The cloud path is documented in §5 as
a possible future route but is out of scope.

---

## 2. Settings model

Add one nested block to `Settings` (Rust + TS), defaulting to disabled so behavior is
unchanged until the user opts in:

```rust
// crates/core/src/types.rs
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct IntegrationsSettings {
    pub zotero: ZoteroSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZoteroSettings {
    pub enabled: bool,          // default false
    pub base_url: String,       // default "http://127.0.0.1:23119"
    pub citation_style: String, // default "chicago-note-bibliography"
}
```

`#[serde(default)]` on the `integrations` field keeps existing settings files loading
cleanly (same approach already used for `semantic`, `max_results`, etc.). The
patch-merge logic needs no change — it already round-trips arbitrary nested objects.

**Why a nested `integrations.zotero` rather than flat keys:** it gives each
integration an ownership boundary, so adding a second integration later never collides
with Zotero's config or the top-level settings namespace.

---

## 3. Backend architecture

### 3.1 A Zotero client in `wilkes_core`

New module `crates/core/src/integrations/zotero/` — a thin, well-typed HTTP client
against Zotero's local API:

```
integrations/
  mod.rs           // Integration trait + registry (mirrors metadata registry)
  zotero/
    mod.rs
    client.rs      // ZoteroClient: base_url, reqwest, typed methods
    model.rs       // ZoteroItem, ZoteroCreator, etc. (serde)
    lookup.rs      // resolve a local file -> Zotero item (by DOI, then title)
```

The client is stateless except for `base_url`; it is constructed per call from
`ZoteroSettings`, matching how `get_file_metadata` builds its registry per invocation.

**Two distinct HTTP surfaces on the same host/port (verified against 9.0.4)** — the
client wraps both, and they have different availability:

| Surface | Base | Always on? | Capability |
|---|---|---|---|
| **Connector server** | `/connector/*` | Yes (on whenever Zotero runs) | **Write only.** `saveItems`, `saveStandaloneAttachment`, `getSelectedCollection`. No library read/search. |
| **Local API** | `/api/*` | **No — opt-in** | **Read/search.** DOI/title/attachment queries, CSL citation formatting. |

This split is the single most important contract finding: **all resolution — and
therefore metadata lookup, citation, and the Add-to-Zotero dedup gate — needs the local
API, which the user must enable.** The connector alone can write but cannot tell you
whether an item already exists (there is no `searchIdentifiers`/read endpoint — both
return 404).

### 3.2 An `Integration` trait (the one new abstraction)

```rust
pub trait Integration: Send + Sync {
    fn id(&self) -> &'static str;                 // "zotero"
    fn is_enabled(&self, settings: &Settings) -> bool;
    async fn health_check(&self, settings: &Settings) -> anyhow::Result<IntegrationStatus>;
}
```

This is deliberately small. It is *not* a plugin system — it's a registry so the
settings UI can enumerate integrations and report reachability, exactly as
`MetadataExtractorRegistry` enumerates extractors. Feature-specific calls (metadata
lookup, citation) are ordinary commands, not trait methods, so the trait doesn't
balloon as features are added.

### 3.3 Commands (`crates/api/src/commands/integrations/zotero.rs`)

```rust
pub async fn zotero_status(settings) -> Result<IntegrationStatus>;               // reachable? version? library found?
pub async fn zotero_lookup_metadata(settings, path) -> Result<DocumentMetadata>; // Feature 1
pub async fn zotero_add_item(settings, path) -> Result<AddOutcome>;              // Feature 3
```

`zotero_lookup_metadata` reuses the existing `DocumentMetadata` type — it runs item
resolution (§3.4) and maps the resolved item's fields onto `DocumentMetadata`.

**Error handling (per project rules):** if the item isn't found, that is a real
"not found" result surfaced to the UI (a toast), *not* a silently-returned
`DocumentMetadata::default()`. Network/Zotero-down errors are logged and returned as
errors, never swallowed.

Exposure points for each new command:
- `#[tauri::command]` added to the `generate_handler!` list in `crates/desktop/src/lib.rs`.
- Matching HTTP route in `crates/server/src/http/`.
- New methods on the `SearchApi` interface + both service implementations
  (`ui/src/services/tauri.ts`, `ui/src/services/http.ts`).

### 3.4 Item resolution (shared by all features)

Resolution maps a local file to a single Zotero item and is the one piece of logic
every feature depends on — metadata lookup consumes it, citation generation consumes
it, and "Add to Zotero" (§6) uses it as its **deduplication gate**. It lives in
`integrations/zotero/lookup.rs` and returns both the item (if any) and a confidence
level so callers can decide whether a weak match is acceptable.

The resolution order balances precision against query cost:

1. **DOI** — cheap, indexed, authoritative *when the file has one*. Single query
   (`?q=<doi>&qmode=everything`). Wilkes extracts normal DOIs locally
   (`crates/core/src/metadata/doi.rs`) and normalizes detected arXiv IDs into
   their arXiv DOI form (`10.48550/arXiv.<id>`), so arXiv papers still flow
   through this DOI path rather than a separate Zotero lookup.
2. **Exact absolute path** — ground truth for **linked-file attachments**, whose
   stored `attachment.path` is the file itself. A byte-for-byte path match identifies
   the exact owning item — a stronger signal than DOI. Requires enumerating attachment
   items (see caveat below).
3. **Filename** — the fallback for **stored/imported attachments**, where Zotero holds
   its own copy under `storage/<KEY>/` so absolute paths never match; only
   `attachment.filename` is comparable. Weaker (collisions possible).
4. **Title** fuzzy match — weakest, last resort.

DOI sits first only because it is a single cheap indexed query; when a file has no DOI,
path becomes the primary signal. Each step carries its own confidence, and steps 3–4
must be able to return "not found" rather than guess.

**Zotero-side caveat:** the local API has **no query-by-path endpoint** (`q` searches
titles/creators; `qmode=everything` widens it but is unreliable for paths). So steps
2–3 require one bulk fetch of attachment items (`?itemType=attachment`) compared
client-side — worth caching per library. Step 1 avoids this entirely.

---

## 4. Frontend architecture

### 4.1 Integrations settings tab

New `IntegrationsPanel.tsx` alongside `ExtensionsPanel`, wired into `SettingsModal` as
a new tab id `"integrations"` in the sidebar and content switch. It renders: the enable
toggle, base URL, citation-style selector, and a "Test connection" button that calls
`zotero_status` and shows reachability. No new state plumbing — it uses the same
`handleUpdateSettings(patch)` already in `SettingsModal`.

### 4.2 The context-menu extension point (the key UI decision)

Today `buildFileContextMenuItems` builds a fixed array. To let an enabled integration
contribute items *without* `fileActions` importing Zotero specifics, introduce a small
contributor list:

```ts
// ui/src/lib/integrations/types.ts
export interface MenuContributorCtx {
  target: ContextMenuTarget;
  api: SearchApi;
  settings: Settings;
  onToast: (m: string, t: "success" | "error") => void;
}
export type MenuContributor = (ctx: MenuContributorCtx) => ContextMenuItem[];
```

```ts
// ui/src/lib/integrations/zotero.ts
export const zoteroMenuContributor: MenuContributor = (ctx) => {
  if (!ctx.settings.integrations?.zotero.enabled) return [];
  if (ctx.target.kind === "directory") return [];
  return [
    {
      id: "zotero-metadata",
      label: "Get metadata from Zotero",
      run: async () => { /* call api.zoteroLookupMetadata, toast result */ },
    },
    {
      id: "zotero-add",
      label: "Add to Zotero",
      run: async () => { /* call api.zoteroAddItem, toast added / already-present */ },
    },
  ];
};
```

`buildFileContextMenuItems` takes `settings` and appends
`contributors.flatMap(c => c(ctx))` after its built-in items. `ResultList` already has
`settings` available to pass in. This keeps a **single owner** for the menu
(fileActions composes; contributors are self-contained per integration) — no second
menu-building mechanism. It is the clean seam that makes "the integration adds options
to the right-click menu" true for *any* future integration, not just Zotero.

---

## 5. Feature 2 viability — generating citations from the Zotero end

**Verdict: viable and low-risk on Zotero 7+, via the local API. It is essentially free
once Feature 1's item-resolution exists.**

Zotero exposes CSL citation formatting through three channels; assessed purely on the
Zotero side:

| Channel | Endpoint | Auth / setup | Offline | Assessment |
|---|---|---|---|---|
| **Local API (Zotero 7+)** | `GET http://127.0.0.1:23119/api/users/0/items/<KEY>?include=citation,bib&style=<id>` | none | yes (Zotero must run) | **Chosen route.** Same CSL engine and endpoint shape as the web API, but against the local library — no key, no sync. |
| Web API (cloud) | `GET https://api.zotero.org/users/<uid>/items/<KEY>?include=bib&style=<id>` | API key + userID, library must be synced | no | Out of scope; adds credential management and a sync dependency. |
| Better BibTeX plugin | `/better-bibtex/cayw` / JSON-RPC | user must install BBT | yes | Off-limits as a hard dependency; nice-to-have detection only. |

Key findings for the local-API route:

- **The formatting is done entirely by Zotero.** Passing `include=citation,bib` (or
  `format=bib`) returns ready-to-use HTML/text for the citation and bibliography entry.
  Wilkes never touches CSL — it picks a `style` id and renders the returned string. No
  citation engine, no CSL processor, no style files in our tree.
- **Styles are Zotero's.** The user's installed CSL styles are addressable by id
  (`chicago-note-bibliography`, `apa`, `ieee`, …). The style selector in the
  Integrations tab is just a string passed through.
- **The hard part is resolution, not formatting** — mapping the local file to a Zotero
  item key. That is exactly the work the shared resolver (§3.4) already does. So Feature
  2 reuses that resolver and adds one command
  (`zotero_generate_citation(path, style) -> String`) plus one context-menu item
  ("Copy citation from Zotero"). No new architecture.

**Constraints stated honestly:**
1. Requires **Zotero running** with the local API **explicitly enabled** — it is
   **opt-in, not on by default** (verified against Zotero 9.0.4: `/api/` returns
   `403 "Local API is not enabled"` until the user turns it on in Settings → Advanced).
   The `zotero_status` health check must distinguish *three* states — Zotero down /
   connector reachable but local API off / fully ready — and the Integrations tab must
   tell the user to enable the local API, since every read feature depends on it.
2. Citation quality is bounded by match confidence — a file with no DOI and a poor title
   match may resolve to the wrong item or none. The resolver must return a
   confidence/`not-found` signal, and the UI must not fabricate a citation on a weak
   match.
3. Older Zotero (v6 and earlier) has no local read API. Given the locked "local API
   only" decision, those users simply see "Zotero not reachable"; the cloud path is not
   built.

**Recommendation:** commit to the local-API route for both features, gate on the
`zotero_status` health check.

---

## 6. Feature 3 — Add to Zotero (with deduplication)

Right-click → "Add to Zotero" adds the file to the user's library. The explicit
requirement is **not to create a duplicate when the file is already in Zotero**.

### 6.1 Deduplication is our responsibility, not Zotero's

Zotero does **not** prevent duplicates on save — its connector will happily create a
second item, and its "Duplicate Items" view is a manual after-the-fact tool. So the
resolver from §3.4 **is** the dedup gate:

```
zotero_add_item(path):
  1. resolve(path)                      // DOI → path → filename → title
  2. if a confident match exists:
        return AddOutcome::AlreadyPresent { item_key }   // no-op, do NOT add
     else:
        add the item, return AddOutcome::Added { item_key }
```

A confident resolver hit short-circuits the add; only a genuine "not found" proceeds.
The `AddOutcome` enum makes the two success paths distinct so the UI can toast
"Added to Zotero" vs. "Already in Zotero" — we never silently treat an already-present
file as a fresh add.

**Which resolution step catches a re-add matters, and the contract reshapes it (see
6.2):** files added *through Wilkes* land as imported copies in Zotero's `storage/`, so
a later re-add of the same on-disk file will **not** match by exact path — it matches by
**DOI (if present) or filename**. Exact-path matching only helps for items the user
*manually linked* in Zotero. Consequence: for a DOI-less file, the only dedup signal on
re-add is filename, which can collide — so `zotero_add_item` must treat a filename-only
match as low-confidence and surface a "possible duplicate, add anyway?" choice rather
than silently adding or silently skipping.

### 6.2 Zotero-side write path (verified)

The local API is read-only, so **writes go through the connector server**
(`/connector/*`, always on). Verified against 9.0.4:
- `getSelectedCollection` → `200`, reports the target library/collection and
  `filesEditable: true`.
- `saveStandaloneAttachment` → `400 {"error":"METADATA_NOT_PROVIDED"}` on an empty body,
  confirming it is the standalone-file entry point and its validation contract.
- `saveItems` exists for creating a bibliographic item with attachments.

**Correction to the earlier draft: a linked-file attachment is _not_ achievable over
these HTTP surfaces.** The connector's attachment flow is an **upload/import** (API v3
advertises `supportsAttachmentUpload: true`), so Zotero **copies the bytes into
`storage/`**. There is no connector parameter to reference a local path in place, and
the local API — the only surface that models linked attachments — is read-only. So
"Add to Zotero" necessarily produces a stored copy; the "no linked-file duplication"
goal from the previous draft is not reachable via automation and is dropped. What we
*can* guarantee is **no duplicate library item**, via the resolver dedup gate.

### 6.3 Verified write contract (live round-trip against 9.0.4)

Both create paths were exercised against the running instance:

- **`POST /connector/saveItems`** — body `{ sessionID, uri, items: [{ itemType,
  title, DOI, creators, attachments }] }` → **`201 Created`, empty body.**
- **`POST /connector/saveStandaloneAttachment`** (the path Feature 3 uses for a local
  file) — metadata via an **`X-Metadata` JSON header** (`{ sessionID, title, url }`),
  the **file bytes as the raw request body** with the file's `Content-Type` →
  **`201 { "canRecognize": true }`.** `canRecognize` signals Zotero can then run its PDF
  metadata recognizer on the stored file.

**Critical consequence — neither call returns the created item key.** `AddOutcome::Added`
therefore cannot get the key from the write response; Wilkes must **read it back via the
local API** using the DOI/title it just wrote. That is a *third* place the feature
depends on the local API, on top of resolution and dedup:

1. Pre-add dedup lookup (resolver) — local API.
2. The add itself — connector (works with local API off).
3. Post-add key retrieval — local API.

So the honest posture is: **the connector alone is insufficient for a correct
Add-to-Zotero.** With the local API disabled, Wilkes cannot dedup *or* confirm what it
created, so `zotero_add_item` must refuse and tell the user to enable the local API
rather than fire a blind, unverifiable, potentially-duplicate write. Items land in
Zotero's *currently selected* collection (`getSelectedCollection`), so the UI should
surface that target before adding.

---

## 7. Scope boundaries for this proposal

Built in the first effort: settings model + Integrations tab, the `Integration`
trait/registry, the Zotero local-API client, the shared resolver (§3.4), the
`zotero_status` + `zotero_lookup_metadata` + `zotero_add_item` commands (both surfaces),
the menu-contributor seam, and the "Get metadata from Zotero" and "Add to Zotero" items.
The one item to de-risk first is the connector write payload (§6.3).

Feature 2 (citations) is **assessed as viable and architecturally free** but, per the
framing of this proposal, only the Zotero-end viability is decided here; the
`zotero_generate_citation` command and its menu item are a labeled follow-on that plugs
into the same resolver and contributor seam.
