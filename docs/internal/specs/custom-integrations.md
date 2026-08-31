# Custom integrations — Design

Status: proposed, nothing implemented
Depends on: `core/src/integrations`, `core/src/network::ProviderHttpClient`,
`Settings.integrations`, `agent/src/mcp.rs::literature_search`
Premise: the mapping from a service's JSON to our result types is the feature.
The transport is secondary, and the code that exists today is almost entirely
that mapping written by hand, once per provider.

## 1. Purpose

Adding OpenAlex meant writing Rust in nine places across five crates and eight
files in the UI:

| Where | What |
| --- | --- |
| `core/src/types.rs` | `OpenAlexSettings`, a field on `IntegrationsSettings`, defaults |
| `core/src/integrations/openalex/{client,model,mod}.rs` | the client |
| `core/src/integrations/mod.rs` | `IntegrationRegistry::default()` |
| `api/src/commands/integrations/openalex.rs` + `mod.rs` | two commands |
| `desktop/src/lib.rs` | two `#[tauri::command]`s + two `invoke_handler` entries |
| `server/src/lib.rs` | two handlers + two routes |
| `agent/src/mcp.rs` | an enum variant and a match arm |
| `ui/` | `types.ts`, `api.ts`, `tauri.ts`, `http.ts`, a hand-written form in `IntegrationsPanel.tsx` |

This is the design for a user adding the tenth provider without any of that.

## 2. Invariant

**A provider is a description of a service, not a compilation unit.**

Concretely: whether a provider is built in or was pasted in by the user this
morning must be invisible to every caller. One registry answers *who are the
providers*; one engine answers *how do I ask this one*; one projection answers
*what does its answer mean in our types*. Nothing below adds a second way to
reach a service, a second downloader, or a second place that decides what a
search result is.

## 3. There is no "our interface" yet — that is the first problem

"Map any service to our interface" presumes one interface. There are three and
a half:

- `CitationSource` — DOI in, DOIs out. A real trait, provider-neutral by
  construction (`integrations/citations.rs`).
- `CatalogueSource` — `fetch_all` → `CatalogueRecord`. A real trait.
- `Integration` — `id` / `is_enabled` / `health_check`. A real trait, but a
  *lifecycle* trait, not a capability one.
- Literature search — **no trait at all.** It is an enum
  (`LiteratureProviderParam`) matched in `mcp.rs` with one copy-pasted arm per
  provider, each arm re-checking `enabled` and re-constructing a client.

The last one is the responsibility that is duplicated rather than owned, and it
is exactly the one users want to extend. So step zero, before any manifest
exists, is extracting it:

```rust
#[async_trait]
pub trait LiteratureSource: Send + Sync {
    fn id(&self) -> &str;
    async fn search(&self, query: &str, limit: usize)
        -> anyhow::Result<Vec<LiteratureSearchResult>>;
}
```

`OpenAlexClient` and `SemanticScholarClient` already have this method with this
exact signature; they gain an `impl` and lose nothing. `literature_search`
stops matching an enum and starts looking up a string id in the registry. A
custom integration is then not a special case — it is a third implementor
entering through the same door the built-ins use. If we skip this step and bolt
custom providers onto the enum, we have built the second mechanism that
`AGENTS.md` forbids.

## 4. The registry becomes runtime state

`IntegrationRegistry::default()` builds a `Vec` of built-ins at construction and
is never consulted for search or citations. It becomes the single owner:

- Built from built-ins **plus enabled manifests**, rebuilt whenever settings or
  manifests change, held behind an `RwLock` in `AppContext`.
- Indexed by id, not iterated by position.
- Typed lookups: `literature(id) -> Option<&dyn LiteratureSource>`,
  `citations(id)`, `catalogue(id)`. A provider that declares no search
  capability simply is not in the search index, so "provider not found" and
  "provider cannot search" are the same, already-handled error.

Custom ids are namespaced `custom:<slug>`. A manifest can never shadow
`openalex`, and every log line and error message stays unambiguous about which
kind of provider produced it.

## 5. The extension mechanism: a declarative manifest

### 5.1 Why declarative, and not a scripting engine

Read `openalex/client.rs` and `semantic_scholar/client.rs` with the question
*what does this actually do?* The answer, in both, is: build a URL from a
template, GET it with a header or a query parameter for identification, walk
into the JSON body, and project a handful of fields — with a small, closed set
of normalizations (`normalize_doi`, first-four-chars-of-a-date, strip an id
prefix). That is a data transformation, and it is the same one every time.

An embedded scripting engine (Rhai, Lua, QuickJS) would express that too, and
also everything else: a new runtime dependency, a sandbox to get right, an
unbounded support surface, and a manifest nobody can audit by reading it. The
cases that need it do not currently exist. If one appears, §9 says where it
goes — and it is not "make the manifest Turing-complete".

Delegating to an out-of-process MCP server does not avoid the work either.
Wilkes is an MCP *server*; it is not a client of third-party MCP servers, so
that path is a new client **plus** the same projection problem, because an
arbitrary MCP tool returns arbitrary JSON that still has to become a
`LiteratureSearchResult`. The projection is the irreducible part. Build it
first, keep the fetcher behind a seam, and MCP-as-transport stays a later,
cheap addition rather than a competing mechanism.

### 5.2 Shape

```toml
manifest_version = 1
id = "crossref"
name = "Crossref"

[http]
base_url = "https://api.crossref.org"

[http.auth]                       # kind = none | header | query
kind = "header"
name = "Crossref-Plus-API-Token"
secret_ref = "crossref_token"     # a name, never a value — see §7

[capabilities.health]
path = "/works/10.1145/3801158"

[capabilities.search]
path = "/works?query.bibliographic={query}&rows={limit}"
items = "message.items[*]"

[capabilities.search.fields]
id             = "DOI"
doi            = { path = "DOI", coerce = "normalize_doi" }
title          = "title[0]"
year           = { path = "published.date-parts[0][0]", coerce = "int" }
venue          = "container-title[0]"
citation_count = { path = "is-referenced-by-count", coerce = "int" }
pdf_url        = { first_of = ["link[0].URL", "resource.primary.URL"] }
```

Three rules keep this a description rather than a program:

**Templates are typed substitution, never concatenation.** `{query}`,
`{limit}`, `{doi}` are the only placeholders; the engine owns percent-encoding,
so a manifest author cannot produce an injection and cannot forget to encode.
An unknown placeholder, or one the capability does not supply, is a save-time
error. The host and scheme come from `base_url` and are fixed at save time —
no template may change them.

**The field map is a projection.** Selector grammar: dotted keys, `[n]`, `[*]`
for the item list, and `first_of` for an ordered fallback. No filters, no
predicates, no arithmetic. Every transformation beyond selection comes from a
closed vocabulary of coercions — `int`, `bool`, `normalize_doi`, `year_from_date`,
`strip_html`, `join`, `absolute_url` — each of which is a function that already
exists in the tree or is three lines. Anything a manifest cannot say, it says
loudly by failing to load, not by producing a plausible-looking wrong record.

**Each capability declares one request.** The one real exception is OpenAlex's
lookup, which retries under a different filter when the DOI filter is empty;
that is expressible as an ordered `attempts` list tried until non-empty, and it
is the only sequencing v1 allows. Chaining a *second* request keyed by the
*first's output* — which is what `CitationSource` needs (§9) — is where a
manifest becomes a program, and v1 does not go there.

### 5.3 Contract per capability

Each capability names the required fields of its output type. `search` requires
`id` and `title`; everything else in `LiteratureSearchResult` may be null. A
manifest whose field map cannot supply a required field is invalid at save
time, not empty at query time.

## 6. Validation is a probe, and it is mandatory

`IntegrationsPanel` already refuses to enable Zotero until `zoteroStatus()`
returns ready, and `validate_program` in `research.rs` already compiles a
smart-collection expression *and executes it against a sample* before accepting
it. Custom integrations follow that precedent exactly:

1. **Parse** — schema, template placeholders, selector grammar, coercion names,
   required-field coverage. All static, all fast, all before any network use.
2. **Probe** — run each declared capability once against a fixed sample input
   and show, side by side: the request URL (secrets redacted), the raw response,
   and the projected structs.
3. **Report every unresolved field by name.** A selector that matched nothing is
   reported, not silently nulled. This is the whole difference between a
   mapping tool and a guessing tool.

A manifest cannot be enabled until every declared capability has probed clean.
At runtime a selector that stops matching (the service changed shape) is a
logged warning per field per response, not a silent null and not a hard failure
of the whole search — that classification lives in one place in the engine.

## 7. Storage, secrets, trust

**Manifests are documents, so they live like documents.** A
`custom_integrations` table (`id`, `manifest`, `manifest_version`, `revision`,
`enabled`, timestamps) next to `smart_collections`, not a field on
`IntegrationsSettings` — that struct is a fixed record of named built-ins, and
growing a `Vec` on it would put a user-editable, revisioned, importable
document inside the app's static configuration.

**Secrets are referenced, never contained.** `secret_ref` names a value stored
separately (settings/keychain). A manifest is therefore safe to export, paste
in a bug report, and share; importing one never carries a credential, and a
secret is only ever sent to the host its own manifest declares.

**Importing is an egress decision.** A manifest is a description of who Wilkes
will talk to, authored by whoever handed over the file. Import shows the host
and the capabilities before saving. All traffic goes through
`ProviderHttpClient`, so retry, backoff, `Retry-After`, and rate-limit
classification are inherited rather than reimplemented — plus a response size
cap and timeout, which the built-in path should gain at the same time.

**Read-only in v1.** No custom capability writes: not to the library, not to a
remote service. A custom search returning `pdf_url` feeds
`acquire::download_to_root`, which remains the only writer and keeps its
existing scheme, size, and duplicate checks. Zotero's write surface
(`save_standalone_attachment`) is explicitly not manifest-expressible.

## 8. The UI stops being hand-written per provider

Fixing the Rust duplication and leaving `IntegrationsPanel.tsx` with one
bespoke form per provider would move the cost, not remove it. So a provider —
built in or custom — publishes its config schema (`enabled`, `base_url`, and
its own typed fields: `api_key` secret, `email` string, a `citation_style`
enum), and one component renders any of them. Custom integrations add exactly
two things to that panel: an import/edit surface for the manifest, and a Probe
button showing §6's side-by-side result. The status row is the same row the
built-ins use.

MCP's `literature_search` loses its enum: `provider` becomes a string validated
against the registry, and the tool description is generated from the enabled
ids so the agent is told what it may actually name.

## 9. Deliberately excluded, and why

- **`CitationSource` (`references`).** OpenAlex needs a fetch, then a batched
  second fetch keyed by the first's output. Declarative chaining is the point
  where a manifest turns into a program. Custom citation sources wait for a
  real second case, and then get a *narrow* two-stage form — not a scripting
  engine.
- **`CatalogueSource` (`fetch_all`).** Needs pagination, a dryness heuristic,
  and progress reporting (see `catalogue/providers.rs`). Manifest-expressible
  in principle, but it is a second design, not a corollary of this one.
- **Writes.** §7.
- **Non-JSON responses.** No XML, no HTML scraping. A provider that does not
  serve JSON is a Rust client.

## 10. Proof that the abstraction is right

Reimplement the *search* capability of OpenAlex and Semantic Scholar as
manifests and run them against the existing `mockito` fixtures in
`openalex/client.rs` and `semantic_scholar/client.rs`. If the engine does not
produce byte-identical `LiteratureSearchResult`s from those exact response
bodies, the projection vocabulary is wrong and it is cheaper to learn that
before the UI exists than after. The built-ins stay Rust regardless — they do
more than search — but their search path is the test case.

## 11. Order of work

1. Extract `LiteratureSource`; make `literature_search` a registry lookup.
   *No new capability yet, and the enum is gone in the same change.*
2. Make `IntegrationRegistry` runtime state in `AppContext` with typed lookups.
3. Manifest schema, parser, selector/coercion engine, projection — with §10 as
   its test suite.
4. Storage, secret refs, import/export.
5. Probe command; wire it to enablement.
6. Schema-driven `IntegrationsPanel`; migrate the three built-in forms onto it.
