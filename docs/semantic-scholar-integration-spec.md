# Semantic Scholar Integration Spec

## Invariant

External bibliographic providers in Wilkes must be accessed through typed provider integrations, and repeat DOI lookups must be served from the existing application cache boundary instead of issuing a network request every time. Semantic Scholar adds citation-count lookup data; it does not become a second source for file identity metadata or Zotero library metadata.

## Goals

- Look up a paper by DOI through the Semantic Scholar Graph API.
- Return the paper title, year, publication date, venue, external IDs, and citation count.
- Cache lookup results on the existing `file_metadata` row for files whose DOI resolves through Semantic Scholar.
- Expose the integration through the same Rust command, desktop, HTTP, and TypeScript API layers used by other integrations.
- Let users enable/disable and test Semantic Scholar in the Integrations settings panel.
- Surface Semantic Scholar citation counts through the file-list display-field mechanism.

## Non-Goals

- Do not create a standalone Semantic Scholar cache table.
- Do not let Semantic Scholar override Zotero/file title, author, or publication-date ownership.
- Do not add a citation formatter or bibliographic reference generator.
- Do not silently fall back to OpenAlex, Crossref, or any other provider.

## Settings

Add `integrations.semantic_scholar`:

- `enabled: bool`, default `false`
- `base_url: String`, default `https://api.semanticscholar.org`
- `api_key: Option<String>`, default `None`

The API key is optional. If present, requests send it as `x-api-key`.

## API Shape

Rust and TypeScript expose:

- `semantic_scholar_status() -> IntegrationStatus`
- `semantic_scholar_lookup(doi: String) -> SemanticScholarPaper`

`SemanticScholarPaper` contains:

- `doi`
- `paper_id`
- `title`
- `year`
- `publication_date`
- `venue`
- `citation_count`
- `external_ids`
- `cached_at_ms`

Lookup requires the integration to be enabled. Disabled lookup returns an error instead of falling back to live calls.

## Provider Contract

Semantic Scholar endpoint:

`GET /graph/v1/paper/DOI:{doi}?fields=title,citationCount,externalIds,year,venue,publicationDate`

Behavior:

- `200`: parse and cache the result.
- `404`: return a not-found error; do not fabricate a zero-citation result.
- `429`: return a rate-limit error; keep any existing cached row untouched.
- Other non-success status: return an explicit provider error.

DOIs are normalized with Wilkes' existing DOI normalizer before cache lookup or network access.

## Cache Design

The existing SQLite metadata cache database extends `file_metadata` with Semantic Scholar columns:

- `semantic_scholar_paper_id TEXT`
- `semantic_scholar_title TEXT`
- `semantic_scholar_year INTEGER`
- `semantic_scholar_publication_date TEXT`
- `semantic_scholar_venue TEXT`
- `semantic_scholar_citation_count INTEGER`
- `semantic_scholar_external_ids_json TEXT`
- `semantic_scholar_cached_at_ms INTEGER`

`file_metadata.doi` is indexed so an explicit DOI lookup can reuse any cached file row for that DOI, and can update all cached file rows with the same DOI after a provider fetch.

No TTL is added in this pass. Cached citation counts are intentionally stable until the user/app performs another lookup path that explicitly refreshes in a future change. This avoids adding a second freshness policy without a UI decision.

## Validation

- Unit tests cover DOI normalization for lookups, file-row cache upsert/get, client URL construction/API-key header behavior, and disabled lookup rejection.
- Existing integration panel tests are extended for the Semantic Scholar toggle.
- `cargo test -p wilkes-core -p wilkes-api` and UI tests should pass.

## Completion State

Completed:

- Typed Semantic Scholar client and models.
- Semantic Scholar data cached on `file_metadata` rows, not a duplicated provider table.
- Backend commands and desktop/HTTP API routes.
- TypeScript API types, settings panel controls, and optional file-list citation display field.

Still duplicated:

- Citation counts from Semantic Scholar, OpenAlex, Crossref, and DataCite can disagree. Wilkes exposes Semantic Scholar's value as provider-specific data and does not treat it as a canonical merged citation count.
