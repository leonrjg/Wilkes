# Custom integrations — Design

Status: implemented for `search`, `health`, HTML/JSON projection, bounded
download resolution, and the UI. §9 lists the remaining exclusions.
Two decisions changed under implementation and are marked **revised** below.
Depends on: `core/src/integrations`, `core/src/network::ProviderHttpClient`,
`Settings.integrations`, `agent/src/mcp.rs::literature_search`
Premise: the mapping from a service's JSON or HTML to our result types is the feature.
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
    fn name(&self) -> &str;
    async fn search(&self, query: &str, limit: usize)
        -> anyhow::Result<Vec<LiteratureSearchResult>>;
    async fn resolve_download(&self, result: &LiteratureSearchResult)
        -> anyhow::Result<Option<ResolvedLiteratureDownload>>;
    async fn status(&self, enabled: bool) -> anyhow::Result<IntegrationStatus>;
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

- **Revised: derived, not held.** The plan was an `RwLock` in `AppContext`
  rebuilt on change. Implementation showed that unnecessary and worse: the MCP
  server is handed an `IntegrationsSettings` *by value* and can reach no shared
  state, and a registry is a pure function of settings. `from_settings` builds
  one on demand, so there is no window in which it disagrees with settings and
  no invalidation to forget. It allocates the same small clients the previous
  code allocated per request anyway.
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
template, GET it with a header or query parameter for identification, walk
into the response, and project a handful of fields through a small, closed set
of normalizations. Anna adds HTML/CSS projection and a short credentialed
download-resolution sequence, but it is still a bounded data transformation.

An embedded scripting engine (Rhai, Lua, QuickJS) would express that too, and
also everything else: a new runtime dependency, a sandbox to get right, an
unbounded support surface, and a manifest nobody can audit by reading it. The
concrete Anna case fits a finite resolver, so it does not justify that cost. If
a future provider does not fit, §9 says where the boundary is — and it is not
"make the manifest Turing-complete".

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

# Identification sent to the primary origin and same-origin resolver steps.
# `value` travels with the manifest; `secret` is only a name, whose value is
# stored separately — see §7. Exactly one of the two per param.
[[http.params]]
location = "header"               # header | query
name = "Crossref-Plus-API-Token"
secret = "crossref_token"

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

Three rules keep this a description rather than a general program:

**Templates are typed substitution, never unchecked concatenation.** `{query}`
and `{limit}` are the placeholders `search` supplies (`health` supplies none);
download-resolution steps receive the selected result plus values declared by
earlier steps. The engine owns percent-encoding. An unknown placeholder, or
one the capability does not yet supply, is a save-time error. Every request
host and scheme come from a literal `base_url`: the primary one under `http`,
or an optional override declared on a resolver step. A resolver's final URL
may be external because it is returned to the existing downloader rather than
fetched by the manifest engine.

**The field map is a projection.** Selector grammar: dotted keys, `[n]`, `[*]`
for the item list, and `first_of` for an ordered fallback. No filters, no
predicates, no arithmetic. Every transformation beyond selection comes from a
closed vocabulary of coercions — `int`, `bool`, `normalize_doi`, `year_from_date`,
`strip_html`, `join`, `absolute_url` — each of which is a function that already
exists in the tree or is three lines. Anything a manifest cannot say, it says
loudly by failing to load, not by producing a plausible-looking wrong record.

**Sequencing is finite and purpose-specific.** Search declares one GET.
Resolving a download after selection may declare at most four ordered GETs,
each pinned to its statically declared origin, with scalar projection from each
response. There are no branches, loops, arbitrary expressions, or writes.

### 5.3 Contract per capability

Each capability names the required fields of its output type. `search` requires
`id` and `title`; everything else in `LiteratureSearchResult` may be null. A
manifest whose field map cannot supply a required field is invalid at save
time, not empty at query time.

### 5.4 HTML responses and on-demand download resolution

`search.response_format = "html"` changes `items` and every field `path` or
`first_of` entry from the JSON selector grammar to CSS. A field reads the
matched elements' normalized text unless its object form names `attribute`.
`capture` plus `capture_group` is the bounded escape hatch for a service that
puts several facts in one line: Rust's linear-time regular-expression engine
runs it, and a missing capture is a mapping error rather than an empty value.
For repeated elements such as author links, `coerce = "join"` joins every CSS
match with `, `; without it a field keeps the selection's ordinary text value.
`input = "query"` (or `"limit"`) projects a typed search input instead of a
response selector. This covers DOI lookup pages whose returned HTML identifies
the matching hash but does not repeat the DOI needed by the download endpoint.

Download resolution is separate from search. A result advertises `provider`
acquisition, and only selecting it runs `resolve_download`. Its finite list of
GET steps is capped at four. A step inherits `http.base_url` or declares its
own literal HTTP(S) `base_url`, may add parameters of its own, and publishes
named scalar values for later steps. Relative values coerced with
`absolute_url` resolve against that step's effective base. Global `http.params`
are inherited only when the step has the primary origin; they are never copied
to a cross-origin step. Redirects are likewise restricted to the request's
declared origin. The final URL and optional filename are handed to the existing
catalogue downloader. The manifest never writes a file and search never spends
one authenticated request per displayed result.

An Anna-style provider is therefore expressible without code:

```toml
manifest_version = 1
id = "anna-journals"
name = "Anna journal search"

[http]
base_url = "https://annas-archive.example"

[[http.params]]
location = "header"
name = "User-Agent"
value = "Mozilla/5.0"

[capabilities.search]
path = "/search?q={query}&content=journal"
response_format = "html"
items = '''div:has(> a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"])'''

[capabilities.search.fields]
id = { path = '''a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"]''', attribute = "href", capture = '''^/md5/([0-9a-f]+)$''' }
title = '''div.max-w-full a[href^="/md5/"]'''
authors = { path = '''a[href^="/search"]:has(span[class="icon-[mdi--user-edit]"])''', coerce = "join" }
publisher = '''a[href^="/search"]:has(span[class="icon-[mdi--company]"])'''
language = { path = "div.text-gray-800", capture = '''^\s*✅?\s*([^\[]+?)\s*\[''' }
file_format = { path = "div.text-gray-800", capture = '''(?i)\b(EPUB|PDF|MOBI|AZW3|AZW|DJVU|CBZ|CBR|FB2|DOCX?|TXT)\b''' }
file_size = { path = "div.text-gray-800", capture = '''(?i)(\d+(?:\.\d+)?\s*(?:MB|KB|GB|TB))''' }
landing_page_url = { path = '''a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"]''', attribute = "href", coerce = "absolute_url" }

[capabilities.resolve_download]
url = "{download_url}"
filename = "{title}.{file_format}"

[[capabilities.resolve_download.steps]]
# Optional when an intermediate API is on another declared origin:
# base_url = "https://annas-archive.example"
path = "/dyn/api/fast_download.json?md5={id}"
response_format = "json"

[[capabilities.resolve_download.steps.params]]
location = "query"
name = "key"
secret = "anna_key"

[capabilities.resolve_download.steps.fields]
download_url = "download_url"
```

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

A manifest cannot be enabled until search and its declared download resolver
have probed clean. A health check still runs through status rather than the
projection probe because its response body has no contract.
At runtime a selector that stops matching (the service changed shape) is a
logged warning per field per response, not a silent null and not a hard failure
of the whole search — that classification lives in one place in the engine.

## 7. Storage, secrets, trust

**Revised: manifests live in settings, not in a table of their own.** The plan
was a `custom_integrations` table beside `smart_collections`. That breaks on the
consumer that matters most: the MCP server receives an `IntegrationsSettings`
by value and has no database handle, so a table would have needed a second
channel to reach it — the exact duplication this feature exists to remove.
`IntegrationsSettings.custom` is a `Vec<CustomIntegrationConfig>`, and the
manifest is stored as the user wrote it, comments and all, because it is the
document they edit and share. `update_settings` is the one choke point that
refuses a manifest it cannot load back, so the registry's load-time drop can
never happen to something the user was not told about.

**Secrets are referenced, never contained.** `secret` names a value stored
separately (settings/keychain). A manifest is therefore safe to export, paste
in a bug report, and share; importing one never carries a credential, and a
secret is only ever sent to the origin beside which it is declared. Primary
parameters are not inherited by cross-origin resolver steps.

**Importing is an egress decision.** A manifest is a description of who Wilkes
will talk to, authored by whoever handed over the file. Import shows every
deduplicated origin and the capabilities before saving —
`custom_integration_summary`, which reads the manifest and touches neither the
network nor the settings file. Each origin is pinned three ways: a base URL is
literal HTTP(S), validation refuses a path that does not start with `/`, and
`request` re-parses the assembled URL and refuses one whose origin has moved.
The manifest HTTP client also refuses cross-origin redirects. All traffic goes
through `ProviderHttpClient`, so retry, backoff, `Retry-After`, and rate-limit
classification are inherited rather than reimplemented — plus a response size
cap and timeout, which the built-in path should gain at the same time.

**Read-only in v1.** No custom capability writes: not to the library, not to a
remote service. A custom search returning `pdf_url` feeds
`acquire::download_to_root`, which remains the only writer and keeps its
existing scheme, size, and duplicate checks. Zotero's write surface
(`save_standalone_attachment`) is explicitly not manifest-expressible.

## 8. The UI stops being hand-written per provider

Fixing the Rust duplication and leaving `IntegrationsPanel.tsx` with one
bespoke form per provider would move the cost, not remove it. So a built-in
provider is a row in `ui/src/lib/integrations/providers.ts` — its fields and
their kinds (`url`, a nullable `password`, a `select` over citation styles),
how to check it, what to say when it cannot be reached — and `ProviderForm`
renders any row. The fourth provider is four lines in that table and nothing
anywhere else.

Manifest-defined providers are deliberately *not* rows in it. Their fields are
not known until a user writes them, which is the whole point of them, so they
are described by their manifest and rendered by `CustomIntegrations`.

**The panel is tabbed, not stacked.** Form-after-form made the panel's length
the sum of how many providers exist — a shape that only got worse once a user
could add their own, and worst of all for the manifest editor, which is the
tallest thing in the panel by far. One tab per provider plus one for Custom
makes it the length of one provider. A tab carries a dot when that provider is
switched on, so the panel still says which ones are live without the user
opening each one to find out.

**One enable rule, not three.** The old forms disagreed: Zotero checked its
status before enabling and required exactly `ready`, while the two remote
providers enabled first, checked, and reverted if the result was unusable, and
accepted `rate_limited` too. The observable outcomes matched, so the surviving
rule is the simpler one — check, then enable only if usable, with
`rate_limited` usable everywhere. Zotero's local API has no rate limit and its
client never returns that state, so unifying costs Zotero nothing.

MCP's `literature_search` loses its enum: `provider` becomes a string validated
against the registry, and the tool description is generated from the enabled
ids so the agent is told what it may actually name.

## 9. Deliberately excluded, and why

- **`CitationSource` (`references`).** OpenAlex needs set-valued extraction,
  batching, and a second request per batch. The scalar, at-most-four-step
  download resolver deliberately cannot express that loop. Custom citation
  sources wait for a real second case and then get their own bounded batch
  shape — not a scripting engine.
- **`CatalogueSource` (`fetch_all`).** Needs pagination, a dryness heuristic,
  and progress reporting (see `catalogue/providers.rs`). Manifest-expressible
  in principle, but it is a second design, not a corollary of this one.
- **Writes.** §7. Resolving a download is a read; the existing downloader
  remains the only writer.
- **XML responses.** JSON and HTML have concrete cases and typed selectors;
  XML still has neither.
- **Boolean combination**, found by §10 rather than foreseen. OpenAlex's
  `is_open_access` is `open_access.is_oa || best_oa_location.is_oa`, and
  `first_of` is *first present*, not *or*. The divergence is pinned by
  `divergence_on_a_disjunction` rather than papered over: closing it means a
  combinator in the vocabulary, which is a decision to take deliberately if a
  second case appears.

## 10. Proof that the abstraction is right

Done, in `custom/mod.rs`'s tests. Both providers' search paths, re-expressed as
manifests, are run against the same mock server as the Rust clients and asserted
equal record for record. The vocabulary held everywhere except the one case in
§9's last bullet, which is exactly what the exercise was for. The built-ins stay
Rust — they do more than search — but their search path is the test case.

## 11. Order of work

1. ~~Extract `LiteratureSource`; make `literature_search` a registry lookup.~~
   Done; the enum went in the same change.
2. ~~`IntegrationRegistry` with typed lookups.~~ Done, derived rather than held
   — see §4.
3. ~~Manifest schema, parser, selector/coercion engine, projection.~~ Done,
   with §10 as its test suite.
4. ~~Storage, secret refs, export.~~ Done — see §7. Export is *copy the
   manifest*: secrets are stored separately, so the text is already safe to
   share, and no file dialog is involved.
5. ~~Probe command; wire it to enablement.~~ Done.
6. ~~Schema-driven `IntegrationsPanel`; migrate the three built-in forms onto
   it.~~ Done, and the panel is tabbed rather than stacked — see §8. Both
   halves of the duplication this feature set out to remove are now gone: a new
   provider is a row in a Rust registry and a row in a TypeScript table, and no
   new form anywhere.

## 12. Also changed on the way

`ProviderHttpClient` had no request timeout (reqwest's default is none) and no
response size cap. Both now exist, the cap enforced against the advertised
`Content-Length` and again against the bytes actually streamed. The built-in
providers wanted these anyway; a manifest that can point the client at any path
its host serves made them required.
