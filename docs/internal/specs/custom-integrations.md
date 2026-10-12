# Custom integrations — Design

Status: implemented. A literature provider is a manifest; there is no other
kind. OpenAlex and Semantic Scholar are manifests embedded in the binary
(§13), `search`, `browse`, `lookup`, `references`, `health`, HTML/JSON
projection and bounded download resolution are capabilities of the one engine,
and the UI is drawn from what a manifest declares. §9 lists the remaining
exclusions. Several decisions changed under implementation and are marked
**revised**; manifest format version 2 (§5, §13) supersedes version 1
throughout.
Depends on: `core/src/integrations`, `core/src/network::ProviderHttpClient`,
`Settings.integrations`, `agent/src/mcp.rs::literature_search`
Premise: the mapping from a service's JSON or HTML to our result types is the feature.
The transport is secondary, and the code that used to exist was almost entirely
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

Custom ids are namespaced `custom:<slug>`; a provider shipped with Wilkes keeps
its plain id (`openalex`), because that string is what stored selections, cache
rows and agent arguments already name it by. A manifest can never shadow
`openalex`, and every log line and error message stays unambiguous about which
kind of provider produced it. (§4 describes the registry as first built; §13
is what it holds now: manifests, and only manifests.)

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
manifest_version = 2
id = "crossref"
name = "Crossref"

[http]
base_url = "https://api.crossref.org"

# Identification sent to the primary origin and same-origin resolver steps.
# A plain string travels with the manifest; `{ secret = "name" }` is only a
# name, whose value is stored separately — see §7. `optional = true` leaves the
# header or parameter off the request while the user has set no value, which is
# what a service that works anonymously and better with a key needs.
[http.headers]
Crossref-Plus-API-Token = { secret = "crossref_token", optional = true, label = "Plus token" }

[http.query]
mailto = { secret = "email", optional = true, label = "Email", kind = "email" }

[capabilities.health]
path = "/works/10.1145/3801158"

[capabilities.search]
path = "/works?query.bibliographic={query}&rows={limit}"
items = "message.items"

[capabilities.search.fields]
id             = "DOI"
doi            = "normalize_doi(DOI)"
title          = "title[0]"
year           = "published.\"date-parts\"[0][0]"
venue          = "\"container-title\"[0]"
citation_count = "\"is-referenced-by-count\""
pdf_url        = "link[?\"content-type\" == 'application/pdf'].URL | [0]"
```

Three rules keep this a description rather than a general program:

**Templates are typed substitution, never unchecked concatenation.** `{query}`
and `{limit}` are the placeholders `search` supplies, with `{page}` (1-based)
or `{offset}` (`(page - 1) * limit`) when the service can be read past its
first page — a search path naming neither is asked for one page, and a second
is refused (`health` supplies none);
`browse` supplies `{limit}` and `{page}`, `lookup` and `references` supply
`{doi}`, and download-resolution steps receive the selected result plus values
declared by earlier steps. The engine owns percent-encoding. An unknown
placeholder, or one the capability does not yet supply, is a save-time error.
Every request host and scheme come from a literal `base_url`: the primary one
under `http`, or an optional override declared on a resolver step. A resolver's
final URL may be external because it is returned to the existing downloader
rather than fetched by the manifest engine.

**The field map is a projection.** **Revised in version 2:** the selector
language is JMESPath (<https://jmespath.org>) — a specified, side-effect-free
query language for JSON with a conformance suite, no assignment, no recursion
and no loops beyond its own projections — and not the dotted-path grammar the
first version carried. The reason was not taste. The grammar had no predicate,
no disjunction and no string operation, so each new need (pick the PDF link out
of a list; `a || b`; strip a prefix from an id) was a vocabulary entry to
design, document and defend, and `coerce` was a closed list that grew the same
way. What JMESPath lacks is registered as *functions*, so reading an expression
still says everything it can do: `normalize_doi`, `year_from_date`,
`strip_html`, `absolute_url`, `to_int`, `to_bool`, `split`, `replace`, beside
JMESPath's own. Every one maps `null` to `null`, so `normalize_doi(ids.doi)`
describes an optional field; a value that is present and the wrong shape is an
error naming both sides, never a guess. The final value is type-checked
strictly against the field's type. `first_of` and `coerce` are gone for JSON
(`not_null(a, b)` and the functions replace them); HTML keeps CSS selectors,
`first_of`, `attribute`, `capture`, and gains `all` and `then` (a JMESPath
expression applied to the extracted value). Anything a manifest cannot say, it
says loudly by failing to load, not by producing a plausible-looking wrong
record.

**Sequencing is finite and purpose-specific.** Search declares one GET.
Resolving a download after selection may declare at most four ordered GETs,
each pinned to its statically declared origin. A lookup may declare at most
three alternative GETs, tried in order until one's response holds a record
(§14). References are one GET for the work and one batched GET per
fixed-size chunk (§14). There are no branches, loops, arbitrary expressions,
or writes.

### 5.3 Contract per capability

Each capability names the required fields of its output type. `search` requires
`id` and `title`; everything else in `LiteratureSearchResult` may be null. A
manifest whose field map cannot supply a required field is invalid at save
time, not empty at query time.

### 5.4 HTML responses and on-demand download resolution

`search.response_format = "html"` changes `items` and every field `path` or
`first_of` entry from JMESPath to CSS. A field reads the
matched elements' normalized text unless its object form names `attribute`.
`capture` plus `capture_group` is the bounded escape hatch for a service that
puts several facts in one line: Rust's linear-time regular-expression engine
runs it, and a missing capture is a mapping error rather than an empty value.
For repeated elements such as author links, `all = true` collects every CSS
match into an array and `then = "join(', ', @)"` joins it; `then` is a JMESPath
expression whose `@` is the extracted value, which is how an HTML field says
what a JSON field says in its own `path` (`then = "absolute_url(@)"`).
`input = "query"` (or `"limit"`) projects a typed search input instead of a
response selector. This covers DOI lookup pages whose returned HTML identifies
the matching hash but does not repeat the DOI needed by the download endpoint.

Download resolution is separate from search. A result advertises `provider`
acquisition, and only selecting it runs `resolve_download`. Its finite list of
GET steps is capped at four. A step inherits `http.base_url` or declares its
own literal HTTP(S) `base_url`, may add `headers` and `query` of its own (a
step's header of the same name replaces the primary one rather than being sent
beside it), and publishes named scalar values for later steps. Relative values
passed through `absolute_url` resolve against that step's effective base.
Global `http.headers` and `http.query` are inherited only when the step has the
primary origin; they are never copied to a cross-origin step, so a header both
origins need is declared on both, explicitly. Redirects are likewise restricted to the request's
declared origin. The final URL and optional filename are handed to the existing
catalogue downloader. The manifest never writes a file and search never spends
one authenticated request per displayed result.

**An absence that is an answer.** Every field of a step must match: a
selector that finds nothing is a manifest that no longer describes the page,
and is an error naming the step and the field. But a page that links a file
when the service holds one, and links none when it does not, answers "no file"
with exactly that absence, and the engine cannot tell the two apart. The author
can, so the field says so:

```toml
fields = { md5 = { path = "a[href*='md5=']", attribute = "href", capture = "(?i)md5=([a-f0-9]{32})", missing = "unavailable" } }
```

`missing` is `error` (the default) or `unavailable`. With `unavailable`, the
selector finding nothing ends resolution with no download — the outcome
`resolve_download` already had for a result with no file, logged at info with
the step and the field. It covers only the selector finding nothing. A value
that is found and does not fit — the capture does not match, the type is wrong
— is the manifest being wrong about the page and is an error whatever
`missing` says; so is every other field of the step, which is read before the
absence is believed. It is refused on a search or lookup field, where a field
that matches nothing is already left empty. A probe whose example result has
no file says the resolver was not checked: no step reached a URL, which is
neither a fault nor evidence.

An Anna-style provider is therefore expressible without code:

```toml
manifest_version = 2
id = "anna-journals"
name = "Anna journal search"

[http]
base_url = "https://annas-archive.example"

[http.headers]
User-Agent = "Mozilla/5.0"

[capabilities.search]
path = "/search?q={query}&content=journal"
response_format = "html"
items = '''div:has(> a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"])'''

[capabilities.search.fields]
id = { path = '''a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"]''', attribute = "href", capture = '''^/md5/([0-9a-f]+)$''' }
title = '''div.max-w-full a[href^="/md5/"]'''
authors = { path = '''a[href^="/search"]:has(span[class="icon-[mdi--user-edit]"])''', all = true, then = "join(', ', @)" }
publisher = '''a[href^="/search"]:has(span[class="icon-[mdi--company]"])'''
language = { path = "div.text-gray-800", capture = '''^\s*✅?\s*([^\[]+?)\s*\[''' }
file_format = { path = "div.text-gray-800", capture = '''(?i)\b(EPUB|PDF|MOBI|AZW3|AZW|DJVU|CBZ|CBR|FB2|DOCX?|TXT)\b''' }
file_size = { path = "div.text-gray-800", capture = '''(?i)(\d+(?:\.\d+)?\s*(?:MB|KB|GB|TB))''' }
landing_page_url = { path = '''a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"]''', attribute = "href", then = "absolute_url(@)" }

[capabilities.resolve_download]
url = "{download_url}"
filename = "{title}.{file_format}"

# One inline table per step. Add base_url = "https://annas-archive.example" to
# a step when an intermediate API is on another declared origin.
steps = [
  { path = "/dyn/api/fast_download.json?md5={id}", response_format = "json", query = { key = { secret = "anna_key" } }, fields = { download_url = "download_url" } },
]
```

## 6. Validation is a probe, and it is evidence

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

**Revised: the probe informs the decision, it does not hold it.** Enabling was
gated on a clean probe of search and the declared download resolver. The gate
was wrong about what a failed probe means: a service that is down, rate-limited,
or simply holding nothing for the chosen example query produces a failed probe
and says nothing about whether the manifest is right — and refusing to save made
the user's work hostage to someone else's uptime, with no way to keep a draft
until the service came back. Steps 1 and 2 above are unchanged and step 3 is the
point of the whole feature; what changed is that the report is shown beside the
save buttons rather than in front of them, and a manifest that has not probed —
or probed badly — says so in a line of text. Parsing still gates: a manifest the
backend cannot load back is refused on save, which is a fact about the manifest
rather than about the network.

A health check still runs through status rather than the projection probe
because its response body has no contract.
At runtime a selector that stops matching (the service changed shape) is a
logged warning per field per response, not a silent null and not a hard failure
of the whole search — that classification lives in one place in the engine.

## 7. Storage, secrets, trust

**Naming is not manifest editing.** `CustomIntegrationConfig.name` is an
optional override of the manifest's own `name`, resolved once in
`CustomSource::named` so that every caller — a pane heading, a status line, the
text of a failed request — says the same thing. It is held beside the manifest
rather than inside it because the manifest describes the *service* and is the
thing a user is handed, copies and shares, while this names their copy of it;
and because rewriting a manifest to change a label would drag the read and the
probe along behind it. Blank is no override, not a nameless provider. The id is
untouched by a rename: it is what a stored provider filter and every log line
name it by, and it stays visible on the row beside whatever the user called it.

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
bespoke form per provider would move the cost, not remove it. So a provider's
settings tab is a *row* (`ui/src/lib/integrations/providers.ts`) — its fields
and their kinds (`url`, a nullable `password`, `email`, a `select`), how to
read and write them in settings, how to check it, what to say when it cannot be
reached — and `ProviderForm` renders any row.

**Revised: the rows are built from the backend, not written out.** Zotero is
the one row written by hand, because Zotero is a local library with a settings
object of its own. Every literature provider shipped with Wilkes is a row built
from `literature_providers` (§13): its name, its origin and the secrets its
manifest names (`label`, `kind`, `optional`) are all read off the manifest, so
the panel names no provider but Zotero and a provider added tomorrow is a tab
with no change to the UI. The literature pane's browse panel (§14) and the
metadata-source selector are drawn from the same list the same way.

Providers the *user* wrote are deliberately not rows. Their fields are not
known until a user writes them, which is the whole point of them, so they are
described by their manifest and rendered by `CustomIntegrations`.

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

- ~~**`CitationSource` (`references`).**~~ No longer excluded: the second real
  case (a built-in that must reach the citation graph) arrived, and it got the
  bounded batch shape this entry promised rather than a scripting engine —
  §14.
- **`CatalogueSource` (`fetch_all`).** Needs pagination, a dryness heuristic,
  and progress reporting (see `catalogue/providers.rs`). Manifest-expressible
  in principle, but it is a second design, not a corollary of this one. (A
  catalogue has since become a `LiteratureSource` — `MirroredSource`, see
  `catalogue-in-the-ui.md` §9 — and this trait its fetch half,
  `CatalogueFetch`; fetching one by manifest is still excluded.)
- **Writes.** §7. Resolving a download is a read; the existing downloader
  remains the only writer.
- **XML responses.** JSON and HTML have concrete cases and typed selectors;
  XML still has neither.
- ~~**Boolean combination**~~ Closed by version 2: OpenAlex's `is_open_access`
  is `open_access.is_oa || best_oa_location.is_oa`, which JMESPath says and the
  dotted-path grammar could not. The test that pinned the divergence
  (`divergence_on_a_disjunction`) is now
  `openalex_open_access_is_a_disjunction_of_the_work_and_its_best_location`.
- **Browse paging by cursor.** `browse` pages by a 1-based `{page}` and
  `{limit}`. A service that pages by an opaque token (Semantic Scholar's bulk
  search) cannot say so, and declares no `browse`. Keyword `search` pages by
  `{page}` or `{offset}`, which is how Semantic Scholar's relevance search is
  read on.
- **Semantic Scholar `references`.** Its endpoint lists the cited papers'
  external ids in one request, which the two-stage `references` shape does not
  describe; it never fed the citation graph and still does not.

## 10. Proof that the abstraction is right

First done by running both providers' search paths, re-expressed as manifests,
against the same mock server as the Rust clients and asserting them equal
record for record; the vocabulary held everywhere except the disjunction in
§9. That test could not outlive its subject: the clients are gone (§13), so
the shipped manifests are now the only implementation, tested against the same
fixture bodies the clients used, with the expected records written out, and
against the live services by hand (search, browse with every filter kind,
paging, lookup including the second-request alternative, references).

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
7. ~~Converge the built-ins onto the manifest.~~ Done — §13.
8. ~~Manifest version 2: header/query maps, JMESPath, optional secrets.~~ Done.
9. ~~`browse`, `lookup`, `references` capabilities; the browse panel.~~ Done —
   §14.

## 12. Also changed on the way

`ProviderHttpClient` had no request timeout (reqwest's default is none) and no
response size cap. Both now exist, the cap enforced against the advertised
`Content-Length` and again against the bytes actually streamed. The built-in
providers wanted these anyway; a manifest that can point the client at any path
its host serves made them required.

## 13. The built-ins are manifests

The decision this section records: **a literature provider is a manifest, and
no OpenAlex-specific or Semantic Scholar-specific code is compiled in.** It
reverses §10's earlier position that the built-ins "stay Rust — they do more
than search". What they did beyond searching — look a DOI up for metadata
enrichment, list a work's references for the citation graph — became
capabilities (§14), after which nothing was left that a manifest could not say.

- `core/src/integrations/builtin/{openalex,semantic_scholar}.toml` are the
  providers, embedded with `include_str!` and parsed by `builtin::manifests()`
  (a test asserts every shipped manifest is valid). `CustomSource` is the only
  `LiteratureSource` implementation; `CitationSource` is a method on it.
- **Per-installation state** is `integrations.builtin[id]` — `enabled`, a
  `base_url` override for a mirror or proxy, and `secrets` (the values for the
  secrets the manifest names). The manifest itself ships with the build and is
  never stored, so it cannot go stale beside it. Custom providers stay in
  `integrations.custom` with their manifests.
- **Migration, once, on read.** `IntegrationsSettings` deserializes through a
  wire type that accepts the old `openalex` / `semantic_scholar` objects (a
  base URL equal to the shipped one is no override; the credential moves to the
  secret the manifest names) and runs `custom::legacy::upgrade` over every
  stored custom manifest. `Manifest::parse` reads version 2 and refuses
  everything else; version 1 exists only inside the converter. The next save
  writes only the new shape. A manifest that cannot be converted is left as
  stored and reported by name — never guessed at.
- **Metadata** is keyed by provider id: `DocumentMetadata.providers` is a map,
  `MetadataSource::Provider(id)` is the cache's provenance, and
  `MetadataSourcePreference` serializes as the bare string it always was. The
  stored strings *are* the provider ids, so no row was migrated. A provider's
  upsert writes only that provider's record. When providers disagree, shipped
  ones are preferred in the order they are listed, then any other by id
  (`builtin::precedence`).
- **One settings-change rule.** `IntegrationRegistry::fingerprints` says which
  providers a settings edit touched (enabled, secrets, origin, a custom
  manifest — not its label); their cached records are dropped and any that can
  look a work up are asked again. It replaced two copies of the same method,
  one per compiled-in provider, and gave custom providers the behaviour they
  never had.
- **Transport.** `openalex_*`, `semantic_scholar_*` and
  `custom_integration_status` are replaced by `literature_provider_status`,
  `literature_provider_lookup` and `literature_browse`
  (`/api/literature/{status,lookup,browse}`), one question for every provider.

## 14. Capabilities beyond search

**`browse`** is a filtered, sorted, paged listing of the records `search`
returns (it reuses `search`'s projection: a service lists answers in the shape
it searches in). The manifest declares what may be varied — a `path` with
`{limit}` and `{page}`, an optional `query_param` for a keyword, a `sort`
(options as `[value, label]` pairs, a `default`, and an optional
`keyword_default` for when a keyword is given), and `filters`, each a `choice`,
a `flag` or a `range` with a clause template carrying `{value}`. The engine
composes the request from those pieces, so a control can only produce a clause
the author wrote; anything else — an undeclared filter, a value outside the
options, a non-integer bound — is refused by name. The interface renders its
filter panel from `ProviderCapabilities.browse` and knows nothing about which
service it is talking to. Filter order is declaration order (the `toml` crate
reads maps in document order; `indexmap` keeps it).

**`lookup`** is DOI in, one bibliographic record out (`id`, `title`, `year`,
`publication_date`, `venue`, `citation_count`, `external_ids`), for metadata
enrichment. It declares one to three request paths tried **in order**: the
first whose response holds a record wins. A response that holds none — an empty
result, a 404 — moves on to the next; any other failure stops the lookup,
because a request that *failed* says nothing about whether the next would find
one, and asking anyway would paper over the failure. That is the one branch the
manifest has, and it is bounded by count and by what may trigger it. OpenAlex
uses it for the DOI index, then the landing-page index.

**`references`** is DOI in, the DOIs the work cites out, in two declared
stages: one request for the work and a JMESPath expression over it yielding
the identifiers it references, then one request per chunk of
`chunk` (1–100) identifiers to turn them back into DOIs. `{ids}` is the
chunk's identifiers, each percent-encoded and joined by the declared `join`,
which stays literal because for most services it is the OR operator. A record
whose DOI expression yields nothing, or a non-DOI, is not an edge a DOI-keyed
library can join, and is left out — counted in the log, not silent. There is no
loop a manifest controls.

**Every capability but `health` requires `search`**, for the reason
`resolve_download` always did: each works on what search returns or on a work a
search result names. A manifest that extends another provider (§17) may take its
`search` rather than declare one.


## 15. Layout: one header per map, one line per entry

A manifest Wilkes ships, generates or teaches writes each entry of a map — a
filter, a field, a header — as one line under one header, and a list of steps
as one inline table per step. A header per entry
(`[capabilities.browse.filters.year]`), or a header per list item followed by a
header per part of it (`[[capabilities.resolve_download.steps]]`, then
`[…steps.query]`), restates the whole path for every entry. It was removed
once, from the header and step parameters, and came back in the browse filters.

Both layouts are valid TOML and parse to the same manifest, so the parser does
not refuse a user's own manifest over it. `manifest::layout_problems` is the
rule — no `[[…]]` header, none deeper than three segments — and tests hold the
shipped manifests, the whole authoring prompt and the interface's starter
manifest to it. A long value stays inline: a TOML array may span lines inside
an inline table, which is how a 26-entry option list fits on one entry.

A design that seems to need a sub-table per entry is a sign the schema is
wrong, not that the rule needs an exception.

## 16. Filters, generalised

**The invariant.** A filter can only produce a clause its manifest author
wrote, with a value whose shape the manifest declared. §14's three kinds held
that by enumerating shapes — a closed list, an on/off, two integer bounds — and
each kind was an arm in four places: validation, request building, the control
description, and the interface. Covering OpenAlex that way would have been a
new arm in all four for entities, dates, decimals, free text, multi-select and
negation.

**The change.** A filter is not a *kind*. It is two independent facts — **one
clause** and **what a value may be** — and everything else is the same for
every filter.

```toml
[capabilities.browse.filters]
open_access = { label = "Open access only", clause = "open_access.is_oa:true" }
type        = { label = "Type", clause = "type:{value}", modes = ["any", "none"], options = [["article", "Article"], ["review", "Review"]] }
year_from   = { label = "From", group = "Year", clause = "from_publication_date:{value}-01-01", value = "integer" }
year_to     = { label = "To",   group = "Year", clause = "to_publication_date:{value}-12-31",   value = "integer" }
impact_min  = { label = "Field-weighted impact above", clause = "fwci:>{value}", value = "number" }
title       = { label = "Title mentions", clause = "title.search:{value}", value = "text" }
author      = { label = "Author", clause = "authorships.author.id:{value}", modes = ["any", "all", "none"], suggest = "/autocomplete/authors?q={query}" }
cites       = { label = "Works citing", clause = "cites:{value}", from_result = "id", suggest = "/autocomplete/works?q={query}" }
```

| Key | Meaning |
| --- | --- |
| `label` | Shown to the user. |
| `clause` | Added when the filter is set. With no `{value}` the filter is on/off. |
| `options` | The value is one of a closed list. |
| `value` | The value is typed: `integer`, `number`, `date` (`YYYY-MM-DD`) or `text`. |
| `suggest` | The value is chosen from the service's own suggestions for what was typed. |
| `from_result` | The value is a field of a search result the user is looking at. |
| `modes` | Several values are allowed, combined by one of these `browse.modes`; the first is the default. |
| `group` | Filters sharing a group are drawn together under its name. |

When the clause has `{value}`, exactly one of `options`, `value` and `suggest`
says where it comes from, or `from_result` does, alone or beside `suggest`.
That is the whole vocabulary; `kind` is gone and a manifest that still writes
it is refused by name.

**A range is two filters.** `year_from` and `year_to` are two clauses with two
values; `group` puts them on one row.

**Modes are declared once.** How several values of one attribute are written is
the service's filter syntax, not a filter's:

```toml
[capabilities.browse.modes]
any  = { label = "Any of",  join = "|" }
all  = { label = "All of",  repeat = true }
none = { label = "None of", repeat = true, value = "!{value}" }
```

Every mode is the same three steps: rewrite each value by `value`, then either
put them all in one clause joined by `join`, or `repeat` the clause once per
value and let the filter separator AND them. `type:article|review`,
`institutions.id:I1,institutions.id:I2` and `type:!article,type:!review` are
those three rows. Negation and intersection are therefore data, and a service
that writes them differently declares different rows.

**Suggestions are declared once.** How a suggestion response is read is a
property of the service:

```toml
[capabilities.browse.suggest]
items = "results"
value = "id"
label = "display_name"
hint  = "hint"
```

One GET on the provider's own origin, `{query}` and `{limit}` as placeholders,
the same headers and query parameters as every other request, at most ten
results, refused under two characters. A filter names only its path. A
suggestion with no value or label, or whose value the filter would refuse, is
left out and counted in the log.

**A value from a row.** `from_result = "id"` makes the filter an action on
every result that holds that field: "works citing this one" sets `cites` to the
row's id and lists the provider by it. The row knows nothing about citations.

**`total`** is one expression over the listing response (`meta.count`), so a
page of ten says it is ten of twelve thousand. Absent from a response, it is
absent and logged; present and not a count, it is an error.

**What keeps the invariant.** `ValueRule::admit`, the same call for every
filter:

- an `options` value is one of the options; an `integer`, `number` or `date`
  parses as one;
- a `text`, suggested or row value is free data, so it may not contain the
  filter separator or any mode's `join`, may not start with what a mode writes
  before a value (`!`), may not hold a control character, and is at most 200
  characters — refused by name rather than escaped, because a silently
  rewritten search is a different search;
- several values need a mode the filter declares, and at most ten are accepted;
- a filter the manifest does not declare is refused by name.

The engine does not check that a suggested value was really suggested: that
needs state, and the value cannot add a clause either way.

**Why this is not a switch chain.** The value rule is the only thing that
varies, so `admit` is the only match on it. Request building is one loop —
admit each value, combine by the mode, write the clause — in declaration order.
Validation and the control description are one path each. The interface holds
one table from value source to control, and every control has the same props:
the filter, its values as a list, a change handler. A new value type is one
`admit` arm and one table row.

**The request.** `filters` maps a name to a list of strings (a single string is
accepted for one value) and `modes` maps a name to the mode chosen, where it is
not the filter's first.

**The interface.** A provider with more than eight filters shows the ones in
use and offers the rest from an "Add a filter" menu. A chip says a value as it
was called when it was chosen, which the pane keeps beside the value because
the request carries only the value.

**What OpenAlex declares.** Forty-eight filters, each checked against the live
API in every mode it declares: year and exact dates; type, language,
open-access status and licence; citations, field-weighted impact and top-cited
standing; domain, field, topic and keyword; author, institution, institution
type, country, continent and number of authors; journal, publisher, source
type, funder and index; text in the title, abstract, full text or affiliation;
presence of an abstract, DOI or references; and the citation graph (citing,
cited by, related to). Two were dropped on the way: OpenAlex's created-date and
updated-date filters need a paid plan.

**Not covered, deliberately.**

- **Nested boolean logic.** A filter panel is a conjunction of filters.
- **Paired constraints** ("this author while at this institution").
- **Facets** — counts per value, and options read from the service rather than
  listed in the manifest. One more request per filter opened; a follow-up.
- **Countries beyond the forty-four listed**, and subfields: OpenAlex has no
  suggestion endpoint for either, so they would be long closed lists.
- **The MCP tool**, which still searches by keyword only.

**Migration.** There is none: `kind` is refused, with no fallback. `browse`
shipped one commit earlier, so this reached the shipped OpenAlex manifest and
whatever a user had written since.

## 17. Extending a provider

**The invariant.** A capability has exactly one author. Wanting OpenAlex's
filters with a download step of one's own used to mean pasting OpenAlex's
manifest into a second one, which is a second author for forty-eight filters
and is out of date at the next release that adds a forty-ninth.

**The change.** A manifest may name one provider and the capabilities it takes
from it, and declare the rest:

```toml
manifest_version = 2
id = "openalex-files"
name = "OpenAlex with downloads"
extends = { provider = "openalex", capabilities = ["health", "search", "browse"] }

[capabilities.resolve_download]
url = "{download_url}"
steps = [
  { base_url = "https://files.example", path = "/by-doi?doi={doi}", query = { key = { secret = "files_key" } }, fields = { download_url = "url" } },
]
```

**One place knows.** `Manifest::resolve` reads the written form, asks a `Bases`
for the provider named, and returns an ordinary `Manifest`: the base's `http`,
the capabilities taken from it, the ones declared. The engine, validation, the
probe, `origins()`, the filter panel and the agent tool read that and nothing
else, so none of them changed and none can tell a derived provider from one
written out in full. `Manifest::parse` reads a manifest that stands on its own
and refuses one that extends. `IntegrationsSettings` is the `Bases` every
caller uses — the registry, the save check, the summary and the probe.

**The rules, and what each is for.**

- **Whole capabilities.** `browse` is a path, a separator, modes and filters
  that mean something only together. A capability is taken or declared;
  naming one in both is refused, and there is no merging into one.
- **Named, not implied.** Only the capabilities listed are taken. Taking all of
  them would make a derived OpenAlex one more provider asked for every
  document's DOI, and a base that gains a capability would hand it to manifests
  that never asked. The draft the interface starts from leaves `lookup` and
  `references` out for the first reason and says so.
- **No `[http]`.** Every path taken is relative to the base's origin, so the
  origin and what identifies a request to it are the base's. What the manifest
  adds runs against that origin too; a resolver step reaches another through
  its own `base_url`, `headers` and `query`, as it always could.
- **The base's installation state stays the base's.** The origin is the one the
  user moved the base to, if they did, and the secrets are the base's own
  values, sent where the base would send them. A derived provider stores and
  is asked for only the secrets it names itself, and naming one the base names
  is refused. Otherwise an OpenAlex behind a proxy would have a twin that went
  around it.
- **One level.** A base stands on its own. A manifest that extends cannot be
  extended, so there is no chain to follow and no cycle to detect.
- **A description, not a provider.** Whether the base is switched on is not
  asked. Switching the shipped OpenAlex off and its derived twin on is how one
  list of results is kept from being two.

**What falls out of what was already there.** The save check loads every
custom manifest, so removing a provider that another extends is refused by
name. `IntegrationRegistry::fingerprints` includes the resolved manifest and
the base's secrets, so moving the base or changing its secret drops the derived
provider's cached records exactly as it drops the base's. A shipped manifest
that changes in a release changes every manifest that extends it; one that
loses a capability a manifest takes leaves that manifest unloadable, named in
the log, like any other the build can no longer read.

**The interface.** `literature_providers` reports `extendable`, the
capabilities a manifest may take from each provider — answered by the same
`Bases` lookup, so what is offered and what is accepted cannot disagree. An
"Extend" button on a shipped provider's tab, or a custom provider's row, opens
the editor on a manifest that takes them. The summary names the provider
extended, what is taken, and the result fields its search fills, which are what
a download step can count on.

**Not covered, deliberately.**

- **Taking from two providers** — OpenAlex's search with another provider's
  resolver. A resolver whose steps name no `base_url` runs against its own
  provider's origin with that provider's identification, so taking it means
  rewriting it; a second design.
- **Changing part of a capability** — one more filter, one field mapped
  differently. That is a second author for the capability.
- **A result that already has a `pdf_url`.** A provider that declares
  `resolve_download` resolves every result through it, as before.
