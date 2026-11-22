use super::*;
use crate::integrations::builtin;
use crate::types::FilterValue;
use mockito::Matcher;

// ── Fixtures ─────────────────────────────────────────────────────────────

/// The bodies the two native clients' own tests used, so the shipped
/// manifests are being asked about the same bytes the Rust clients were.
const OPENALEX_BODY: &str = r#"{"results":[{"id":"https://openalex.org/W1","display_name":"T","ids":{"doi":"https://doi.org/10.1/example"},"open_access":{"is_oa":true,"oa_status":"gold","oa_url":"https://example.test/article"},"best_oa_location":{"is_oa":true,"pdf_url":"https://example.test/paper.pdf","landing_page_url":"https://example.test/article","license":"cc-by"}}]}"#;
const SEMANTIC_SCHOLAR_BODY: &str = r#"{"data":[{"paperId":"p1","externalIds":{"DOI":"10.1/example"},"title":"T","citationCount":3,"isOpenAccess":true,"openAccessPdf":{"url":"https://example.test/paper.pdf","status":"GOLD","license":"CCBY"}}]}"#;

/// A JSON service that needs a credential, shaped like Semantic Scholar's
/// search. The probe and error tests use it because a *required* secret is
/// what they are about; the shipped providers' are optional.
const KEYED_MANIFEST: &str = r#"
manifest_version = 2
id = "keyed"
name = "Keyed"

[http]
base_url = "BASE_URL"

[http.headers]
x-api-key = { secret = "api_key" }

[capabilities.search]
path = "/graph/v1/paper/search?query={query}&limit={limit}&fields=title,citationCount"
items = "data"

[capabilities.search.fields]
id = "paperId"
title = "title"
doi = "externalIds.DOI"
year = "year"
publication_date = "publicationDate"
venue = "venue"
citation_count = "citationCount"
is_open_access = "isOpenAccess"
pdf_url = "openAccessPdf.url"
open_access_status = "openAccessPdf.status"
license = "openAccessPdf.license"
"#;

const ANNA_MANIFEST: &str = r#"
manifest_version = 2
id = "anna"
name = "Anna"

[http]
base_url = "BASE_URL"

[http.headers]
User-Agent = "Mozilla/5.0 Wilkes"

[capabilities.search]
path = "/search?q={query}&content=journal"
response_format = "html"
items = '''div:has(> a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"])'''

[capabilities.search.fields]
id = { path = '''a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"]''', attribute = "href", capture = '''^/md5/([0-9a-f]+)$''' }
title = '''div.max-w-full a[href^="/md5/"]'''
authors = { path = '''div.max-w-full a[href^="/search"]:has(span[class="icon-[mdi--user-edit]"])''', all = true, then = "join(', ', @)" }
publisher = '''div.max-w-full a[href^="/search"]:has(span[class="icon-[mdi--company]"])'''
venue = '''div.max-w-full a[href^="/search"]:has(span[class="icon-[mdi--company]"])'''
language = { path = "div.text-gray-800", capture = '''^\s*✅?\s*([^\[]+?)\s*\[''' }
file_format = { path = "div.text-gray-800", capture = '''(?i)\b(EPUB|PDF|MOBI|AZW3|AZW|DJVU|CBZ|CBR|FB2|DOCX?|TXT)\b''' }
file_size = { path = "div.text-gray-800", capture = '''(?i)(\d+(?:\.\d+)?\s*(?:MB|KB|GB|TB))''' }
landing_page_url = { path = '''a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"]''', attribute = "href", then = "absolute_url(@)" }

[capabilities.resolve_download]
url = "{download_url}"
filename = "{title}.{file_format}"

steps = [
  { path = "/dyn/api/fast_download.json?md5={id}", response_format = "json", query = { key = { secret = "anna_key" } }, fields = { download_url = "download_url" } },
]
"#;

const ANNA_BODY: &str = r#"<!doctype html><html><body>
<div class="result flex">
  <a href="/md5/deadbeef" class="custom-a block mr-2 sm:mr-4 hover:opacity-80">cover</a>
  <div class="max-w-full">
    <a href="/md5/deadbeef">Graph: Networks</a>
    <a href="/search?q=Ada"><span class="icon-[mdi--user-edit]"></span> Ada Lovelace </a>
    <a href="/search?q=Grace"><span class="icon-[mdi--user-edit]"></span> Grace Hopper </a>
    <a href="/search?q=Journal"><span class="icon-[mdi--company]"></span> Example Journal </a>
    <div class="text-gray-800">✅ English [en] · PDF · 2.4MB · 2024</div>
  </div>
</div>
</body></html>"#;

const ANNA_DOI_MANIFEST: &str = r#"
manifest_version = 2
id = "anna-doi"
name = "Anna DOI"

[http]
base_url = "BASE_URL"

[capabilities.search]
path = "/scidb/{query}"
response_format = "html"
items = '''div:has(> a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"])'''

[capabilities.search.fields]
id = { path = '''a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"]''', attribute = "href", capture = '''^/md5/([0-9a-f]+)$''' }
doi = { input = "query", then = "normalize_doi(@)" }
title = '''div.max-w-full a[href^="/md5/"]'''

[capabilities.resolve_download]
url = "/scidb?doi={doi}"
filename = "{title}"
"#;

fn source(manifest: &str, base_url: &str, secrets: &[(&str, &str)]) -> CustomSource {
    let manifest = Manifest::parse(&manifest.replace("BASE_URL", base_url)).unwrap();
    CustomSource::new(manifest, secret_map(secrets)).unwrap()
}

fn secret_map(secrets: &[(&str, &str)]) -> HashMap<String, String> {
    secrets
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn fetched(plan: DownloadPlan) -> ResolvedLiteratureDownload {
    match plan {
        DownloadPlan::Fetch(download) => download,
        DownloadPlan::Run(run) => panic!("expected a URL, got a run of {}", run.program),
    }
}

#[tokio::test]
async fn a_resolver_that_runs_a_program_writes_each_argument_from_the_result() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", Matcher::Regex("^/scidb/".to_string()))
        .with_status(200)
        .with_body(ANNA_BODY)
        .create_async()
        .await;
    let from = "url = \"/scidb?doi={doi}\"\nfilename = \"{title}\"";
    assert!(ANNA_DOI_MANIFEST.contains(from));
    let manifest = ANNA_DOI_MANIFEST.replace(
        from,
        r#"run = { program = "fetch", args = ["{doi}", "--out", "{output_dir}/{id}", "--name={filename}"] }
filename = "{title}.pdf""#,
    );
    let anna = source(&manifest, &server.url(), &[]);
    let book = anna
        .search("10.1234/example", 1, 1)
        .await
        .unwrap()
        .remove(0);

    let Some(DownloadPlan::Run(run)) = anna.resolve_download(&book).await.unwrap() else {
        panic!("expected a run");
    };
    assert_eq!(run.program, "fetch");
    assert_eq!(
        run.args
            .iter()
            .map(|arg| arg.write("/staging/1"))
            .collect::<Vec<_>>(),
        [
            "10.1234/example",
            "--out",
            "/staging/1/deadbeef",
            // Sanitized as a download's name would be.
            "--name=Graph_ Networks.pdf",
        ]
    );
}

/// A run's arguments written by a source holding `secrets`.
fn plan(
    args: Vec<RunArg>,
    env: &[(&str, ParamValue)],
    values: &[(&str, &str)],
    secrets: &[(&str, &str)],
) -> anyhow::Result<ProgramRun> {
    let anna = source(ANNA_DOI_MANIFEST, "https://anna.test", secrets);
    let run = RunSpec {
        program: "fetch".to_string(),
        args,
        env: env
            .iter()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect(),
    };
    let values = values
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    anna.run_plan(&run, &values, Unset::Refused)
}

fn template(arg: &str) -> RunArg {
    RunArg::Template(arg.to_string())
}

fn secret(name: &str, optional: bool) -> manifest::SecretRef {
    manifest::SecretRef {
        secret: name.to_string(),
        optional,
        label: None,
        kind: None,
    }
}

#[test]
fn a_run_refuses_a_value_that_would_be_read_as_an_option() {
    // Inside an argument a leading dash is the value's own text.
    let inside = plan(
        vec![template("--name={title}")],
        &[],
        &[("title", "-rf")],
        &[],
    )
    .unwrap();
    assert_eq!(inside.args[0].write("/out"), "--name=-rf");

    let error = plan(
        vec![template("{title}")],
        &[],
        &[("title", "--exec=sh")],
        &[],
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("args[0]") && error.contains("starts with '-'"),
        "{error}"
    );
}

#[test]
fn a_value_that_reads_output_dir_is_not_where_the_program_writes() {
    let run = plan(
        vec![template("{output_dir}/{title}")],
        &[],
        &[("title", "{output_dir}")],
        &[],
    )
    .unwrap();
    assert_eq!(run.args[0].write("/out"), "/out/{output_dir}");
}

#[test]
fn a_run_is_given_secrets_as_whole_arguments_and_as_variables() {
    let run = plan(
        vec![template("--key"), RunArg::Secret(secret("api_key", false))],
        &[
            ("API_TOKEN", ParamValue::Secret(secret("token", false))),
            ("MODE", ParamValue::Literal("batch".to_string())),
            ("PROXY", ParamValue::Secret(secret("proxy", true))),
        ],
        &[],
        &[("api_key", " key-1 "), ("token", "tok-2")],
    )
    .unwrap();
    // A secret is passed as the user set it, trimmed as a header's is.
    assert_eq!(run.args[1].write("/out"), "key-1");
    // An optional secret with no value leaves its variable unset.
    assert_eq!(
        run.env,
        [
            ("API_TOKEN".to_string(), "tok-2".to_string()),
            ("MODE".to_string(), "batch".to_string()),
        ]
    );
    // Both are redacted from what the program says; the literal is not.
    assert_eq!(run.secrets, ["key-1", "tok-2"]);
    // And neither is in what the plan says of itself.
    assert!(!format!("{run:?}").contains("key-1"));

    let error = plan(
        vec![RunArg::Secret(secret("api_key", false))],
        &[],
        &[],
        &[],
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("secret 'api_key' has no value"), "{error}");
}

/// A manifest with no [http], whose every request is `/bin/sh` printing
/// a JSON record titled by what it was given.
#[cfg(unix)]
const CLI_MANIFEST: &str = r##"
manifest_version = 2
id = "cli"
name = "CLI"

[capabilities.health]
run = { program = "/bin/sh", args = ["-c", "exit 0"] }

[capabilities.search]
run = { program = "/bin/sh", args = ["-c", "printf '%s%s%s' \"$PREFIX\" \"$*\" \"$SUFFIX\"", "sh", "{query}", "{limit}"], env = { PREFIX = '{"results":[{"id":"1","title":"', SUFFIX = '"}]}' } }
items = "results"

[capabilities.search.fields]
id = "id"
title = "title"

[capabilities.browse]
run = { program = "/bin/sh", args = ["-c", "printf '%s%s%s' \"$PREFIX\" \"$*\" \"$SUFFIX\"", "sh", ["--sort", "{sort}"], ["--year", "{year}"], "--page={page}"], env = { PREFIX = '{"results":[{"id":"1","title":"', SUFFIX = '"}]}' } }

[capabilities.browse.sort]
default = "cited"
options = [["cited", "Most cited"], ["new", "Newest"]]

[capabilities.browse.filters]
year = { label = "Year", clause = "{value}", value = "integer" }

[capabilities.lookup]
paths = [
  { program = "/bin/sh", args = ["-c", "exit 0", "sh", "{doi}"] },
  { program = "/bin/sh", args = ["-c", "printf '%s%s%s' \"$PREFIX\" \"$1\" \"$SUFFIX\"", "sh", "{doi}"], env = { PREFIX = '{"id":"', SUFFIX = '"}' } },
]

[capabilities.lookup.fields]
id = "id"
"##;

#[cfg(unix)]
fn cli_source(allowed: &[&str]) -> CustomSource {
    CustomSource::new(Manifest::parse(CLI_MANIFEST).unwrap(), HashMap::new())
        .unwrap()
        .with_allowed_programs(allowed.iter().map(|name| name.to_string()).collect())
}

#[cfg(unix)]
#[tokio::test]
async fn a_manifest_with_no_origin_searches_and_checks_health_by_running_programs() {
    let cli = cli_source(&["/bin/sh"]);
    assert_eq!(cli.base_url(), "");
    let found = cli.search("graph theory", 5, 1).await.unwrap();
    // Each value is one argument, however many words it holds.
    assert_eq!(found[0].title.as_deref(), Some("graph theory 5"));

    let status = cli.status(true).await.unwrap();
    assert_eq!(status.state, IntegrationState::Ready, "{}", status.message);

    let probe = cli.probe("graphs").await;
    assert!(probe.ok, "{:?}", probe.error);
    assert_eq!(
        probe.request_url,
        "/bin/sh -c printf '%s%s%s' \"$PREFIX\" \"$*\" \"$SUFFIX\" sh graphs 3"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_request_that_is_a_run_runs_only_when_its_program_is_allowed() {
    let cli = cli_source(&[]);
    let error = cli.search("graphs", 5, 1).await.unwrap_err().to_string();
    assert!(error.contains("'/bin/sh' is not allowed to run"), "{error}");
    let status = cli.status(true).await.unwrap();
    assert_eq!(status.state, IntegrationState::RemoteApiDown);
    assert!(!cli.probe("graphs").await.ok);
}

#[cfg(unix)]
#[tokio::test]
async fn browse_leaves_off_an_argument_group_that_has_nothing_to_say() {
    let cli = cli_source(&["/bin/sh"]);
    let listing = |year: Option<&str>| BrowseRequest {
        query: None,
        filters: year
            .map(|year| ("year".to_string(), FilterValue::One(year.to_string())))
            .into_iter()
            .collect(),
        modes: Default::default(),
        sort: Some("new".to_string()),
        page: Some(2),
        limit: Some(10),
    };
    let title = |page: BrowsePage| page.results[0].title.clone().unwrap();
    assert_eq!(
        title(cli.browse(&listing(None)).await.unwrap()),
        "--sort new --page=2"
    );
    assert_eq!(
        title(cli.browse(&listing(Some("2020"))).await.unwrap()),
        "--sort new --year 2020 --page=2"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_lookup_run_that_prints_nothing_has_no_record_and_the_next_is_asked() {
    let cli = cli_source(&["/bin/sh"]);
    let work = cli.lookup("10.1234/example").await.unwrap();
    assert_eq!(work.work_id, "10.1234/example");
}

#[tokio::test]
async fn optional_description_and_image_are_projected_without_losing_sparse_results() {
    let manifest = r#"manifest_version = 2
id = "media"
name = "Media"
[http]
base_url = "BASE_URL"
[capabilities.search]
path = "/search?q={query}"
items = "data"
[capabilities.search.fields]
id = "id"
title = "title"
description = "description"
image_url = "image"
[capabilities.browse]
path = "/works?page={page}&limit={limit}"
"#;
    let provider = source(manifest, "https://example.test", &[]);
    let search = provider.manifest.capabilities.search.as_ref().unwrap();
    let body = serde_json::json!({"data": [
        {"id": "1", "title": "A", "description": "A synopsis.", "image": "https://cdn.example.test/cover.jpg"},
        {"id": "2", "title": "B"},
        {"id": "3", "title": "C", "description": null, "image": null},
        {"id": "4", "title": "D", "description": "Still readable.", "image": "javascript:alert(1)"}
    ]});
    let projection = provider
        .project_json(search, &body, "", 10, Some(10))
        .unwrap();
    assert_eq!(projection.results.len(), 4);
    let first = &projection.results[0];
    assert_eq!(first.summary.as_deref(), Some("A synopsis."));
    assert_eq!(
        first.image_url.as_deref(),
        Some("https://cdn.example.test/cover.jpg")
    );
    let mut serialized = serde_json::to_value(first).unwrap();
    serialized.as_object_mut().unwrap().remove("image_url");
    let older: LiteratureSearchResult = serde_json::from_value(serialized).unwrap();
    assert_eq!(older.image_url, None);
    assert_eq!(older.summary, first.summary);
    let values = result_values(first);
    assert_eq!(values["description"], "A synopsis.");
    assert_eq!(values["image_url"], "https://cdn.example.test/cover.jpg");
    for result in &projection.results[1..3] {
        assert_eq!(result.summary, None);
        assert_eq!(result.image_url, None);
    }
    assert_eq!(
        projection.results[3].summary.as_deref(),
        Some("Still readable.")
    );
    assert_eq!(projection.results[3].image_url, None);
    assert_eq!(projection.issues.len(), 1);
    assert_eq!(projection.issues[0].field, "image_url");

    let mut server = mockito::Server::new_async().await;
    let search_mock = server
        .mock("GET", "/search")
        .match_query(Matcher::Any)
        .with_body(body.to_string())
        .create_async()
        .await;
    let browse_mock = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_body(body.to_string())
        .create_async()
        .await;
    let remote = source(manifest, &server.url(), &[]);
    assert_eq!(
        remote.search("test", 10, 1).await.unwrap(),
        projection.results
    );
    assert_eq!(
        remote
            .browse(&BrowseRequest {
                limit: Some(10),
                ..BrowseRequest::default()
            })
            .await
            .unwrap()
            .results,
        projection.results
    );
    search_mock.assert_async().await;
    browse_mock.assert_async().await;
}

#[tokio::test]
async fn an_unpaged_listing_is_read_whole_and_has_no_second_page() {
    let manifest = r#"manifest_version = 2
id = "links"
name = "Links"
[http]
base_url = "BASE_URL"
[capabilities.search]
path = "/search?q={query}"
items = "data"
[capabilities.search.fields]
id = "id"
title = "title"
[capabilities.browse]
path = "/all"
"#;
    let records: Vec<_> = (0..15)
        .map(|index| serde_json::json!({"id": index.to_string(), "title": format!("T{index}")}))
        .collect();
    let mut server = mockito::Server::new_async().await;
    let listing = server
        .mock("GET", "/all")
        .with_body(serde_json::json!({ "data": records }).to_string())
        .expect(1)
        .create_async()
        .await;
    let remote = source(manifest, &server.url(), &[]);
    assert!(!remote.capabilities().browse.unwrap().paged);
    let first = remote
        .browse(&BrowseRequest {
            limit: Some(10),
            ..BrowseRequest::default()
        })
        .await
        .unwrap();
    assert_eq!(first.results.len(), 15);
    let second = remote
        .browse(&BrowseRequest {
            limit: Some(10),
            page: Some(2),
            ..BrowseRequest::default()
        })
        .await
        .unwrap_err()
        .to_string();
    assert!(second.contains("has one page"), "{second}");
    listing.assert_async().await;
}

#[test]
fn html_description_and_image_use_the_same_result_fields() {
    let manifest = r#"manifest_version = 2
id = "html_media"
name = "HTML Media"
[http]
base_url = "https://example.test"
[capabilities.search]
path = "/search?q={query}"
response_format = "html"
items = "article"
[capabilities.search.fields]
id = { path = "h2", attribute = "data-id" }
title = "h2"
description = "p"
image_url = { path = "img", attribute = "src", then = "absolute_url(@)" }
"#;
    let provider = source(manifest, "https://example.test", &[]);
    let search = provider.manifest.capabilities.search.as_ref().unwrap();
    let body = br#"<article><h2 data-id="1">Title</h2><p>A <b>description</b>.</p><img src="/cover.jpg"></article>"#;
    let projection = provider
        .project_response(search, body, "", 10, Some(10))
        .unwrap();
    assert!(projection.issues.is_empty(), "{:?}", projection.issues);
    assert_eq!(projection.results.len(), 1);
    assert_eq!(
        projection.results[0].summary.as_deref(),
        Some("A description.")
    );
    assert_eq!(
        projection.results[0].image_url.as_deref(),
        Some("https://example.test/cover.jpg")
    );
}

/// A shipped provider, pointed at a mock server in place of its own origin.
fn shipped(id: &str, base_url: &str, secrets: &[(&str, &str)]) -> CustomSource {
    let mut manifest = builtin::manifests()
        .into_iter()
        .find(|manifest| manifest.id == id)
        .unwrap_or_else(|| panic!("no shipped manifest '{id}'"));
    manifest.http.as_mut().unwrap().base_url = base_url.to_string();
    CustomSource::build(manifest, secret_map(secrets), None, Standing::BuiltIn).unwrap()
}

// ── Identity ─────────────────────────────────────────────────────────────

/// Renaming is a label on this installation's copy, so it must not need
/// the manifest touched — the manifest describes the service, and editing
/// it to change a row's name would mean re-reading and re-probing it.
#[test]
fn a_config_name_overrides_the_manifests_own() {
    let config = CustomIntegrationConfig {
        id: "keyed".into(),
        name: Some("My Keyed".into()),
        enabled: false,
        manifest: KEYED_MANIFEST.replace("BASE_URL", "https://example.test"),
        secrets: HashMap::new(),
    };
    let source = CustomSource::from_config(&config, &crate::types::IntegrationsSettings::default())
        .expect("loads");
    assert_eq!(source.name(), "My Keyed");
    // The id is the manifest's and is untouched by a rename: it is what
    // every stored selection and every log line names.
    assert_eq!(source.id(), "custom:keyed");
}

/// A cleared rename box is no override, not a nameless provider.
#[test]
fn a_blank_config_name_leaves_the_manifests_own() {
    for name in [None, Some(String::new()), Some("   ".into())] {
        let config = CustomIntegrationConfig {
            id: "keyed".into(),
            name,
            enabled: false,
            manifest: KEYED_MANIFEST.replace("BASE_URL", "https://example.test"),
            secrets: HashMap::new(),
        };
        let source =
            CustomSource::from_config(&config, &crate::types::IntegrationsSettings::default())
                .expect("loads");
        assert_eq!(source.name(), "Keyed");
    }
}

#[test]
fn a_built_in_is_not_namespaced_and_a_custom_provider_is() {
    assert_eq!(
        shipped("openalex", "https://example.test", &[]).id(),
        "openalex"
    );
    assert_eq!(
        source(KEYED_MANIFEST, "https://example.test", &[]).id(),
        "custom:keyed"
    );
}

// ── The shipped providers: search ────────────────────────────────────────

/// What the native OpenAlex client made of its own fixture, said as the
/// record it produced. The shipped manifest must produce the same one.
fn expected_openalex_record() -> LiteratureSearchResult {
    LiteratureSearchResult {
        id: "https://openalex.org/W1".into(),
        doi: Some("10.1/example".into()),
        title: Some("T".into()),
        year: None,
        publication_date: None,
        venue: None,
        citation_count: None,
        is_open_access: true,
        pdf_url: Some("https://example.test/paper.pdf".into()),
        landing_page_url: Some("https://example.test/article".into()),
        image_url: None,
        open_access_status: Some("gold".into()),
        license: Some("cc-by".into()),
        authors: None,
        publisher: None,
        language: None,
        file_format: None,
        file_size: None,
        summary: None,
        subject: None,
        kind: None,
        pages: None,
        recall_score: None,
        acquisition: LiteratureAcquisition::Direct,
    }
}

#[tokio::test]
async fn the_openalex_manifest_sends_query_limit_and_api_key_and_reads_the_record() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/works")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("search".into(), "graph neural networks".into()),
            Matcher::UrlEncoded("per-page".into(), "2".into()),
            Matcher::UrlEncoded("api_key".into(), "team-key".into()),
            Matcher::Regex("select=id".into()),
        ]))
        .with_status(200)
        .with_body(OPENALEX_BODY)
        .create_async()
        .await;

    let results = shipped("openalex", &server.url(), &[("api_key", " team-key ")])
        .search(" graph neural networks ", 2, 1)
        .await
        .unwrap();

    assert_eq!(results, vec![expected_openalex_record()]);
    mock.assert_async().await;
}

#[tokio::test]
async fn openalex_search_and_browse_request_and_reconstruct_the_abstract() {
    let mut server = mockito::Server::new_async().await;
    let mut body: Value = serde_json::from_str(OPENALEX_BODY).unwrap();
    body["results"][0]["abstract_inverted_index"] = serde_json::json!({
        "café": [3, 1], "研究": [2], "A": [0]
    });
    let mock = server
        .mock("GET", "/works")
        .match_query(Matcher::Regex("abstract_inverted_index".into()))
        .with_status(200)
        .with_body(body.to_string())
        .expect(2)
        .create_async()
        .await;
    let provider = shipped("openalex", &server.url(), &[]);
    let mut expected = expected_openalex_record();
    expected.summary = Some("A café 研究 café".into());
    assert_eq!(
        provider.search("coffee", 10, 1).await.unwrap(),
        vec![expected.clone()]
    );
    assert_eq!(
        provider
            .browse(&BrowseRequest::default())
            .await
            .unwrap()
            .results,
        vec![expected]
    );
    mock.assert_async().await;
}

/// Reading on is one declared placeholder, and each service says where to
/// start in its own terms: OpenAlex by page, Semantic Scholar by offset.
#[tokio::test]
async fn a_later_page_of_a_search_starts_where_the_service_expects() {
    let mut server = mockito::Server::new_async().await;
    let openalex = server
        .mock("GET", "/works")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("per-page".into(), "10".into()),
            Matcher::UrlEncoded("page".into(), "3".into()),
        ]))
        .with_status(200)
        .with_body(OPENALEX_BODY)
        .create_async()
        .await;
    let scholar = server
        .mock("GET", "/graph/v1/paper/search")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("limit".into(), "10".into()),
            Matcher::UrlEncoded("offset".into(), "20".into()),
        ]))
        .with_status(200)
        .with_body(SEMANTIC_SCHOLAR_BODY)
        .create_async()
        .await;

    shipped("openalex", &server.url(), &[])
        .search("graph", 10, 3)
        .await
        .unwrap();
    shipped("semantic_scholar", &server.url(), &[])
        .search("graph", 10, 3)
        .await
        .unwrap();
    openalex.assert_async().await;
    scholar.assert_async().await;
}

/// A service whose path names no page would answer its first again, and
/// the pane would list the same works twice as if they were more.
#[tokio::test]
async fn a_second_page_of_a_search_that_has_one_is_refused() {
    let source = source(
        r#"manifest_version = 2
id = "onepage"
name = "One page"

[http]
base_url = "BASE_URL"

[capabilities.search]
path = "/search?q={query}&limit={limit}"
items = "results"

[capabilities.search.fields]
id = "id"
title = "title"
"#,
        "https://one.test",
        &[],
    );
    assert!(!source.capabilities().paged_search);
    let error = source.search("graph", 10, 2).await.unwrap_err().to_string();
    assert!(error.contains("one page"), "{error}");
    assert!(
        shipped("openalex", "https://api.example.test", &[])
            .capabilities()
            .paged_search
    );
}

/// Single-response providers must not lose records beyond the pane's
/// page size. Paged providers still obey it, even if the server overfills.
#[tokio::test]
async fn search_retains_all_records_only_without_pagination() {
    let mut server = mockito::Server::new_async().await;
    for format in ["json", "html"] {
        let (items, fields, body) = if format == "json" {
            let records: Vec<_> = (0..15)
                .map(|index| {
                    serde_json::json!({
                        "id": format!("work-{index}"),
                        "title": format!("Title {index}"),
                    })
                })
                .collect();
            (
                "results",
                "id = \"id\"\ntitle = \"title\"",
                serde_json::json!({ "results": records }).to_string(),
            )
        } else {
            (
                "article",
                "id = { path = \"a\", attribute = \"href\" }\ntitle = \"a\"",
                (0..15)
                    .map(|index| {
                        format!("<article><a href='work-{index}'>Title {index}</a></article>")
                    })
                    .collect::<String>(),
            )
        };
        for paged in [false, true] {
            let path = if paged {
                "/search?q={query}&limit={limit}&page={page}"
            } else {
                "/search?q={query}&limit={limit}"
            };
            let manifest = format!(
                r#"manifest_version = 2
id = "records"
name = "Records"
[http]
base_url = "BASE_URL"
[capabilities.search]
path = "{path}"
response_format = "{format}"
items = "{items}"
[capabilities.search.fields]
{fields}
"#
            );
            let mock = server
                .mock("GET", "/search")
                .match_query(Matcher::AllOf(vec![
                    Matcher::UrlEncoded("q".into(), "graph".into()),
                    Matcher::UrlEncoded("limit".into(), "10".into()),
                ]))
                .with_status(200)
                .with_body(&body)
                .expect(1)
                .create_async()
                .await;
            let results = source(&manifest, &server.url(), &[])
                .search("graph", 10, 1)
                .await
                .unwrap();
            let expected = if paged { 10 } else { 15 };
            assert_eq!(results.len(), expected, "{format}, paged={paged}");
            for (index, result) in results.iter().enumerate() {
                assert_eq!(result.id, format!("work-{index}"));
                assert_eq!(result.title, Some(format!("Title {index}")));
            }
            mock.assert_async().await;
            mock.remove_async().await;
        }
    }
}

#[tokio::test]
async fn an_unset_optional_secret_is_left_off_the_request() {
    let mut server = mockito::Server::new_async().await;
    // Mocks match in creation order, so a request that carries `api_key`
    // lands here and fails the `expect(0)`.
    let with_key = server
        .mock("GET", "/works")
        .match_query(Matcher::Regex("api_key".into()))
        .expect(0)
        .create_async()
        .await;
    let without = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(OPENALEX_BODY)
        .expect(2)
        .create_async()
        .await;

    // No `api_key` secret at all, and a blank one: both mean "not set".
    for secrets in [vec![], vec![("api_key", "   ")]] {
        let results = shipped("openalex", &server.url(), &secrets)
            .search("graph", 1, 1)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
    }
    with_key.assert_async().await;
    without.assert_async().await;
}

#[tokio::test]
async fn the_semantic_scholar_manifest_sends_its_key_and_reads_the_record() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/graph/v1/paper/search")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("query".into(), "graph neural networks".into()),
            Matcher::UrlEncoded("limit".into(), "2".into()),
            Matcher::Regex("fields=title".into()),
        ]))
        .match_header("x-api-key", "secret")
        .with_status(200)
        .with_body(SEMANTIC_SCHOLAR_BODY)
        .create_async()
        .await;

    let results = shipped(
        "semantic_scholar",
        &server.url(),
        &[("api_key", " secret ")],
    )
    .search(" graph neural networks ", 2, 1)
    .await
    .unwrap();

    assert_eq!(results.len(), 1);
    let paper = &results[0];
    assert_eq!(paper.id, "p1");
    assert_eq!(paper.doi.as_deref(), Some("10.1/example"));
    assert_eq!(paper.citation_count, Some(3));
    assert!(paper.is_open_access);
    assert_eq!(
        paper.pdf_url.as_deref(),
        Some("https://example.test/paper.pdf")
    );
    assert_eq!(paper.open_access_status.as_deref(), Some("GOLD"));
    assert_eq!(paper.license.as_deref(), Some("CCBY"));
    assert_eq!(paper.acquisition, LiteratureAcquisition::Direct);
    mock.assert_async().await;
}

#[tokio::test]
async fn semantic_scholar_works_without_a_key_and_sends_no_header() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/graph/v1/paper/search")
        .match_query(Matcher::Any)
        .match_header("x-api-key", Matcher::Missing)
        .with_status(200)
        .with_body(SEMANTIC_SCHOLAR_BODY)
        .create_async()
        .await;
    let results = shipped("semantic_scholar", &server.url(), &[])
        .search("graph", 1, 1)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    mock.assert_async().await;
}

/// The one case the dotted-path selector could not say: `first_of` is
/// *first present*, not *or*, and OpenAlex's `is_open_access` is an `or`
/// over two fields that are both always present. It used to be the pinned
/// divergence between the manifest and the Rust client; the Rust client is
/// gone and the manifest says it.
#[tokio::test]
async fn openalex_open_access_is_a_disjunction_of_the_work_and_its_best_location() {
    let body = r#"{"results":[{"id":"W1","display_name":"T","ids":{},"open_access":{"is_oa":false},"best_oa_location":{"is_oa":true,"pdf_url":"https://example.test/p.pdf"}}]}"#;
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(body)
        .create_async()
        .await;
    let results = shipped("openalex", &server.url(), &[])
        .search("q", 1, 1)
        .await
        .unwrap();
    assert!(results[0].is_open_access, "false || true is true");
    mock.assert_async().await;
}

#[tokio::test]
async fn openalex_reads_authors_and_publisher_and_treats_no_authors_as_none() {
    let body = r#"{"results":[
            {"id":"W1","display_name":"A","ids":{},"authorships":[{"author":{"display_name":"Ada Lovelace"}},{"author":{"display_name":"Grace Hopper"}}],"primary_location":{"source":{"display_name":"J","host_organization_name":"Pub"}}},
            {"id":"W2","display_name":"B","ids":{},"authorships":[]}
        ]}"#;
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(body)
        .create_async()
        .await;
    let results = shipped("openalex", &server.url(), &[])
        .search("q", 2, 1)
        .await
        .unwrap();
    assert_eq!(
        results[0].authors.as_deref(),
        Some("Ada Lovelace, Grace Hopper")
    );
    assert_eq!(results[0].publisher.as_deref(), Some("Pub"));
    assert_eq!(results[1].authors, None, "an empty list is no authors");
    mock.assert_async().await;
}

// ── Status ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn status_probes_whether_or_not_the_integration_is_enabled() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/graph/v1/paper/DOI:10.1145%2F3801158")
        .match_query(Matcher::UrlEncoded("fields".into(), "paperId".into()))
        .with_status(200)
        .with_body(r#"{"paperId":"p1"}"#)
        .expect(2)
        .create_async()
        .await;

    let source = shipped("semantic_scholar", &server.url(), &[]);
    let disabled = source.status(false).await.unwrap();
    assert_eq!(disabled.state, IntegrationState::Ready);
    assert!(!disabled.enabled);
    let enabled = source.status(true).await.unwrap();
    assert_eq!(enabled.state, IntegrationState::Ready);
    assert!(enabled.enabled);
    mock.assert_async().await;
}

#[tokio::test]
async fn status_reports_a_down_service() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_status(400)
        .create_async()
        .await;
    let status = shipped("openalex", &server.url(), &[])
        .status(true)
        .await
        .unwrap();
    assert_eq!(status.state, IntegrationState::RemoteApiDown);
    mock.assert_async().await;
}

// ── Lookup ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn semantic_scholar_lookup_normalizes_the_doi_and_sends_the_key() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/graph/v1/paper/DOI:10.1145%2F3801158")
        .match_query(Matcher::Regex("fields=title".into()))
        .match_header("x-api-key", "secret")
        .with_status(200)
        .with_body(
            r#"{"paperId":"p1","externalIds":{"DOI":"10.1145/3801158","CorpusId":7},"title":"T","year":2026,"citationCount":3}"#,
        )
        .create_async()
        .await;

    let work = shipped("semantic_scholar", &server.url(), &[("api_key", "secret")])
        .lookup("https://doi.org/10.1145/3801158")
        .await
        .unwrap();

    assert_eq!(work.doi, "10.1145/3801158");
    assert_eq!(work.work_id, "p1");
    assert_eq!(work.title.as_deref(), Some("T"));
    assert_eq!(work.year, Some(2026));
    assert_eq!(work.citation_count, Some(3));
    assert_eq!(work.external_ids["CorpusId"], serde_json::json!(7));
    assert!(work.cached_at_ms > 0);
    mock.assert_async().await;
}

#[tokio::test]
async fn a_rate_limited_lookup_is_retried_once() {
    let mut server = mockito::Server::new_async().await;
    let first = server
        .mock("GET", "/graph/v1/paper/DOI:10.1145%2F3801158")
        .match_query(Matcher::Any)
        .with_status(429)
        .with_header("Retry-After", "0")
        .expect(1)
        .create_async()
        .await;
    let second = server
        .mock("GET", "/graph/v1/paper/DOI:10.1145%2F3801158")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(r#"{"paperId":"p1","externalIds":{"DOI":"10.1145/3801158"},"citationCount":9}"#)
        .expect(1)
        .create_async()
        .await;

    let work = shipped("semantic_scholar", &server.url(), &[])
        .lookup("10.1145/3801158")
        .await
        .unwrap();

    assert_eq!(work.citation_count, Some(9));
    first.assert_async().await;
    second.assert_async().await;
}

#[tokio::test]
async fn a_missing_paper_is_no_record_not_a_failure_to_read() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/graph/v1/paper/DOI:10.1145%2F3801158")
        .match_query(Matcher::Any)
        .with_status(404)
        .create_async()
        .await;
    let error = shipped("semantic_scholar", &server.url(), &[])
        .lookup("10.1145/3801158")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("no record for DOI 10.1145/3801158"),
        "{error}"
    );
    mock.assert_async().await;
}

#[tokio::test]
async fn an_invalid_doi_is_refused_before_any_request() {
    let error = shipped("openalex", "http://127.0.0.1:1", &[])
        .lookup("not-a-doi")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("Invalid DOI"), "{error}");
}

const OPENALEX_RECORD: &str = r#"{"results":[{"id":"https://openalex.org/W3145166639","doi":"https://doi.org/10.1109/msr52588.2021.00074","display_name":"Sampling Projects in GitHub","publication_year":2021,"publication_date":"2021-05-01","cited_by_count":12,"ids":{"openalex":"https://openalex.org/W3145166639","doi":"https://doi.org/10.1109/msr52588.2021.00074"},"primary_location":{"source":{"display_name":"MSR"}}}]}"#;

/// The DOI index first; when it holds nothing, the landing-page index,
/// which knows works the first does not. This is the bounded alternative
/// the lookup capability exists to say.
#[tokio::test]
async fn lookup_moves_to_the_next_request_only_when_a_response_holds_no_record() {
    let mut server = mockito::Server::new_async().await;
    let by_doi = server
        .mock("GET", "/works")
        .match_query(Matcher::UrlEncoded(
            "filter".into(),
            "doi:https://doi.org/10.1109/msr52588.2021.00074".into(),
        ))
        .with_status(200)
        .with_body(r#"{"results":[]}"#)
        .expect(1)
        .create_async()
        .await;
    let by_landing_page = server
        .mock("GET", "/works")
        .match_query(Matcher::UrlEncoded(
            "filter".into(),
            "locations.landing_page_url:https://doi.org/10.1109/msr52588.2021.00074".into(),
        ))
        .with_status(200)
        .with_body(OPENALEX_RECORD)
        .expect(1)
        .create_async()
        .await;

    let work = shipped("openalex", &server.url(), &[])
        .lookup("https://doi.org/10.1109/MSR52588.2021.00074")
        .await
        .unwrap();

    assert_eq!(work.doi, "10.1109/msr52588.2021.00074");
    assert_eq!(work.work_id, "https://openalex.org/W3145166639");
    assert_eq!(work.title.as_deref(), Some("Sampling Projects in GitHub"));
    assert_eq!(work.year, Some(2021));
    assert_eq!(work.venue.as_deref(), Some("MSR"));
    assert_eq!(work.citation_count, Some(12));
    assert_eq!(
        work.external_ids["openalex"],
        serde_json::json!("https://openalex.org/W3145166639")
    );
    by_doi.assert_async().await;
    by_landing_page.assert_async().await;
}

#[tokio::test]
async fn lookup_that_finds_a_record_first_does_not_ask_again() {
    let mut server = mockito::Server::new_async().await;
    let first = server
        .mock("GET", "/works")
        .match_query(Matcher::Regex("filter=doi".into()))
        .with_status(200)
        .with_body(OPENALEX_RECORD)
        .expect(1)
        .create_async()
        .await;
    let second = server
        .mock("GET", "/works")
        .match_query(Matcher::Regex("landing_page_url".into()))
        .expect(0)
        .create_async()
        .await;
    shipped("openalex", &server.url(), &[])
        .lookup("10.1109/msr52588.2021.00074")
        .await
        .unwrap();
    first.assert_async().await;
    second.assert_async().await;
}

/// Only "no record here" moves on. A request that *failed* says nothing
/// about whether the next would find one, and asking anyway would paper
/// over the failure.
#[tokio::test]
async fn lookup_stops_at_a_failure_instead_of_trying_the_next_request() {
    let mut server = mockito::Server::new_async().await;
    let failing = server
        .mock("GET", "/works")
        .match_query(Matcher::Regex("filter=doi".into()))
        .with_status(400)
        .expect(1)
        .create_async()
        .await;
    let never = server
        .mock("GET", "/works")
        .match_query(Matcher::Regex("landing_page_url".into()))
        .expect(0)
        .create_async()
        .await;
    let error = shipped("openalex", &server.url(), &[])
        .lookup("10.1109/msr52588.2021.00074")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("400"), "{error}");
    failing.assert_async().await;
    never.assert_async().await;
}

#[tokio::test]
async fn lookup_with_no_record_anywhere_says_so() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(r#"{"results":[]}"#)
        .expect(2)
        .create_async()
        .await;
    let error = shipped("openalex", &server.url(), &[])
        .lookup("10.1109/msr52588.2021.00074")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("has no record for DOI"), "{error}");
    mock.assert_async().await;
}

#[tokio::test]
async fn a_provider_without_lookup_says_so_by_name() {
    let error = source(KEYED_MANIFEST, "https://example.test", &[("api_key", "k")])
        .lookup("10.1/x")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("does not offer lookup"), "{error}");
}

// ── References ───────────────────────────────────────────────────────────

#[tokio::test]
async fn references_resolve_a_works_identifiers_to_cited_works_in_one_batch() {
    let mut server = mockito::Server::new_async().await;
    let work = server
        .mock("GET", "/works")
        .match_query(Matcher::UrlEncoded(
            "filter".into(),
            "doi:https://doi.org/10.1/cites".into(),
        ))
        .with_status(200)
        .with_body(
            r#"{"results":[{"id":"https://openalex.org/W1","referenced_works":["https://openalex.org/W10","https://openalex.org/W11","https://openalex.org/W12"]}]}"#,
        )
        .expect(1)
        .create_async()
        .await;
    // The separator is the service's OR operator and stays literal; the
    // mock sees the decoded query either way, so this checks the value.
    let resolve = server
        .mock("GET", "/works")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("filter".into(), "openalex_id:W10|W11|W12".into()),
            Matcher::UrlEncoded("per-page".into(), "3".into()),
            Matcher::UrlEncoded(
                "select".into(),
                "id,doi,display_name,publication_year,authorships,primary_location".into(),
            ),
        ]))
        .with_status(200)
        .with_body(
            r#"{"results":[
                    {"id":"https://openalex.org/W12","doi":"https://doi.org/10.5/B","display_name":"Cited B","publication_year":2020,"authorships":[{"author":{"display_name":"Ada Lovelace"}},{"author":{"display_name":"Charles Babbage"}}],"primary_location":{"source":{"display_name":"Venue B"}}},
                    {"id":"https://openalex.org/W10","doi":"https://doi.org/10.5/ref","display_name":"A reference","publication_year":2019,"authorships":[],"primary_location":null},
                    {"id":"https://openalex.org/W11","display_name":"No DOI"},
                    {"id":"W13","doi":"https://doi.org/10.5/ref"}
                ]}"#,
        )
        .expect(1)
        .create_async()
        .await;

    let cited = shipped("openalex", &server.url(), &[])
        .references("https://doi.org/10.1/CITES")
        .await
        .unwrap();

    // Sorted, deduplicated, normalized; the record without a DOI is not an
    // edge a DOI-keyed library can join. Each cited work carries the
    // fields the same batch returned for it, and what the service did not
    // report stays unknown.
    assert_eq!(
        cited,
        vec![
            CitationReference {
                doi: "10.5/b".into(),
                title: Some("Cited B".into()),
                authors: Some("Ada Lovelace, Charles Babbage".into()),
                year: Some(2020),
                venue: Some("Venue B".into()),
            },
            CitationReference {
                doi: "10.5/ref".into(),
                title: Some("A reference".into()),
                authors: None,
                year: Some(2019),
                venue: None,
            },
        ]
    );
    work.assert_async().await;
    resolve.assert_async().await;
}

#[tokio::test]
async fn references_are_resolved_in_declared_chunks() {
    const CHUNKED: &str = r#"
manifest_version = 2
id = "chunked"
name = "Chunked"
[http]
base_url = "BASE_URL"
[capabilities.search]
path = "/s?q={query}"
items = "r"
[capabilities.search.fields]
id = "id"
title = "t"
[capabilities.references]
path = "/w/{doi}"
ids = "refs"
[capabilities.references.resolve]
path = "/batch?ids={ids}&n={count}"
join = ","
chunk = 2
items = "r"
doi = "doi"
"#;
    let mut server = mockito::Server::new_async().await;
    let _work = server
        .mock("GET", "/w/10.1%2Fx")
        .with_status(200)
        .with_body(r#"{"refs":["a","b","c"]}"#)
        .create_async()
        .await;
    let first = server
        .mock("GET", "/batch")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("ids".into(), "a,b".into()),
            Matcher::UrlEncoded("n".into(), "2".into()),
        ]))
        .with_status(200)
        .with_body(r#"{"r":[{"doi":"10.9/a"}]}"#)
        .expect(1)
        .create_async()
        .await;
    let second = server
        .mock("GET", "/batch")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("ids".into(), "c".into()),
            Matcher::UrlEncoded("n".into(), "1".into()),
        ]))
        .with_status(200)
        .with_body(r#"{"r":[{"doi":"10.9/c"}]}"#)
        .expect(1)
        .create_async()
        .await;

    let cited = source(CHUNKED, &server.url(), &[])
        .references("10.1/x")
        .await
        .unwrap();
    // A manifest that maps no fields yields bare DOIs.
    assert_eq!(
        cited,
        vec![
            CitationReference {
                doi: "10.9/a".into(),
                ..CitationReference::default()
            },
            CitationReference {
                doi: "10.9/c".into(),
                ..CitationReference::default()
            },
        ]
    );
    first.assert_async().await;
    second.assert_async().await;
}

#[tokio::test]
async fn an_unreadable_reference_field_fails_the_references_rather_than_storing_half() {
    const TYPED: &str = r#"
manifest_version = 2
id = "typed"
name = "Typed"
[http]
base_url = "BASE_URL"
[capabilities.search]
path = "/s?q={query}"
items = "r"
[capabilities.search.fields]
id = "id"
title = "t"
[capabilities.references]
path = "/w/{doi}"
ids = "refs"
[capabilities.references.resolve]
path = "/batch?ids={ids}"
join = ","
chunk = 10
items = "r"
doi = "doi"
fields = { title = "t", year = "y" }
"#;
    let mut server = mockito::Server::new_async().await;
    let _work = server
        .mock("GET", "/w/10.1%2Fx")
        .with_status(200)
        .with_body(r#"{"refs":["a"]}"#)
        .create_async()
        .await;
    let _batch = server
        .mock("GET", "/batch")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(r#"{"r":[{"doi":"10.9/a","t":"A","y":"soon"}]}"#)
        .create_async()
        .await;

    let error = source(TYPED, &server.url(), &[])
        .references("10.1/x")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("could not read 'year' of 10.9/a"), "{error}");
}

#[tokio::test]
async fn a_work_that_references_nothing_makes_no_second_request() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(r#"{"results":[{"id":"W1","referenced_works":[]}]}"#)
        .expect(1)
        .create_async()
        .await;
    let cited = shipped("openalex", &server.url(), &[])
        .references("10.1/x")
        .await
        .unwrap();
    assert!(cited.is_empty());
    mock.assert_async().await;
}

#[tokio::test]
async fn an_unknown_work_has_no_references_rather_than_an_error() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(r#"{"results":[]}"#)
        .expect(1)
        .create_async()
        .await;
    let cited = shipped("openalex", &server.url(), &[])
        .references("10.1/x")
        .await
        .unwrap();
    assert!(cited.is_empty());
    mock.assert_async().await;
}

#[tokio::test]
async fn a_provider_without_references_says_so_by_name() {
    let error = shipped("semantic_scholar", "https://example.test", &[])
        .references("10.1/x")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("does not offer references"), "{error}");
}

// ── Browse ───────────────────────────────────────────────────────────────

fn openalex_browse() -> BrowseCapability {
    builtin::manifests()
        .into_iter()
        .find(|manifest| manifest.id == "openalex")
        .unwrap()
        .capabilities
        .browse
        .unwrap()
}

fn browse_request(filters: &[(&str, &str)]) -> BrowseRequest {
    BrowseRequest {
        filters: filters
            .iter()
            .map(|(k, v)| (k.to_string(), FilterValue::One(v.to_string())))
            .collect(),
        ..BrowseRequest::default()
    }
}

fn many(name: &str, values: &[&str], mode: Option<&str>) -> BrowseRequest {
    BrowseRequest {
        filters: [(
            name.to_string(),
            FilterValue::Many(values.iter().map(|v| v.to_string()).collect()),
        )]
        .into(),
        modes: mode
            .map(|mode| (name.to_string(), mode.to_string()))
            .into_iter()
            .collect(),
        ..BrowseRequest::default()
    }
}

fn filter_param(request: &BrowseRequest) -> String {
    browse_parameters(&openalex_browse(), request, None)
        .unwrap()
        .into_iter()
        .find(|(name, _)| name == "filters")
        .map(|(_, value)| value)
        .unwrap_or_default()
}

#[test]
fn browse_composes_the_declared_clauses_in_declaration_order() {
    let browse = openalex_browse();
    let request = browse_request(&[
        // Deliberately out of order: the manifest decides the order.
        ("open_access", "true"),
        ("year_to", "2024"),
        ("field", "17"),
        ("year_from", "2020"),
        ("cited_min", "100"),
        ("not_retracted", "false"),
    ]);
    let extra = browse_parameters(&browse, &request, None).unwrap();
    assert_eq!(
        extra,
        vec![
            (
                "filters".to_string(),
                "from_publication_date:2020-01-01,to_publication_date:2024-12-31,\
                     open_access.is_oa:true,cited_by_count:>100,primary_topic.field.id:17"
                    .to_string()
            ),
            ("sort".to_string(), "cited_by_count:desc".to_string()),
        ]
    );
}

/// The three modes are the same three steps with different data: rewrite
/// each value, then one clause or one per value.
#[test]
fn a_mode_decides_how_several_values_are_written() {
    // The filter's first mode is the default.
    assert_eq!(
        filter_param(&many("type", &["article", "review"], None)),
        "type:article|review"
    );
    assert_eq!(
        filter_param(&many("type", &["article", "review"], Some("none"))),
        "type:!article,type:!review"
    );
    assert_eq!(
        filter_param(&many(
            "author",
            &["https://openalex.org/A1", "https://openalex.org/A2"],
            Some("all")
        )),
        "authorships.author.id:https://openalex.org/A1,\
             authorships.author.id:https://openalex.org/A2"
    );
    // One value under a mode is still written by the mode.
    assert_eq!(
        filter_param(&many("language", &["en"], Some("none"))),
        "language:!en"
    );
}

#[test]
fn a_typed_value_is_written_as_its_type() {
    assert_eq!(
        filter_param(&browse_request(&[("impact_min", "2.50")])),
        "fwci:>2.5"
    );
    assert_eq!(
        filter_param(&browse_request(&[("published_from", "2024-03-01")])),
        "from_publication_date:2024-03-01"
    );
    assert_eq!(
        filter_param(&browse_request(&[("title", "  deep learning ")])),
        "title.search:deep learning"
    );
    assert_eq!(
        filter_param(&browse_request(&[("cites", "https://openalex.org/W1")])),
        "cites:https://openalex.org/W1"
    );
}

#[test]
fn browse_refuses_what_the_manifest_did_not_declare() {
    let browse = openalex_browse();
    for (filters, expected) in [
        (vec![("nope", "1")], "'nope' is not a filter"),
        (vec![("type", "novel")], "'novel' is not one of its options"),
        (vec![("open_access", "maybe")], "is on or off"),
        (vec![("year_from", "soon")], "must be a whole number"),
        (vec![("impact_min", "high")], "must be a number"),
        (vec![("published_from", "March 2024")], "YYYY-MM-DD"),
        (vec![("published_from", "2024-13-01")], "YYYY-MM-DD"),
        (vec![("year", "2020")], "'year' is not a filter"),
    ] {
        let error = browse_parameters(&browse, &browse_request(&filters), None).unwrap_err();
        assert!(error.contains(expected), "{filters:?}: {error}");
    }
    let mut request = browse_request(&[]);
    request.sort = Some("whatever".into());
    let error = browse_parameters(&browse, &request, None).unwrap_err();
    assert!(error.contains("is not a sort option"), "{error}");
}

/// A request can be checked without being sent, by the rules that decide
/// whether it would be: nothing reaches the service either way.
#[test]
fn a_browse_request_is_checked_without_a_request_going_out() {
    // An origin nothing listens on: a check that sent anything would fail.
    let source = shipped("openalex", "http://127.0.0.1:9", &[]);
    source
        .check_browse(&browse_request(&[
            ("type", "review"),
            ("year_from", "2020"),
        ]))
        .unwrap();
    let refused = source
        .check_browse(&browse_request(&[("type", "novel")]))
        .unwrap_err();
    assert!(refused
        .to_string()
        .contains("'novel' is not one of its options"));
    // A provider with nothing to browse says so rather than passing.
    let searches_only = Manifest::parse(&KEYED_MANIFEST.replace("BASE_URL", "http://127.0.0.1:9"));
    assert!(
        CustomSource::new(searches_only.unwrap(), secret_map(&[("api_key", "k")]))
            .unwrap()
            .check_browse(&BrowseRequest::default())
            .is_err()
    );
}

/// A value nobody declared in advance cannot carry the service's own
/// filter syntax, or it would write a clause no manifest author did.
#[test]
fn a_free_value_cannot_write_filter_syntax() {
    let browse = openalex_browse();
    for (filters, expected) in [
        (
            vec![("title", "graphs,is_retracted:true")],
            "cannot contain ','",
        ),
        (vec![("title", "graphs|trees")], "cannot contain '|'"),
        (vec![("title", "!graphs")], "cannot start with '!'"),
        (vec![("cites", "W1,type:article")], "cannot contain ','"),
        (vec![("title", "a\nb")], "control character"),
    ] {
        let error = browse_parameters(&browse, &browse_request(&filters), None).unwrap_err();
        assert!(error.contains(expected), "{filters:?}: {error}");
    }
    let long = "x".repeat(201);
    let error = browse_parameters(&browse, &browse_request(&[("title", &long)]), None).unwrap_err();
    assert!(error.contains("longer than 200"), "{error}");
}

#[test]
fn several_values_need_a_filter_that_declares_modes() {
    let browse = openalex_browse();
    let error = browse_parameters(&browse, &many("title", &["a", "b"], None), None).unwrap_err();
    assert!(error.contains("takes one value"), "{error}");
    let error =
        browse_parameters(&browse, &many("type", &["article"], Some("all")), None).unwrap_err();
    assert!(
        error.contains("'all' is not a mode of filter 'type'"),
        "{error}"
    );
    let eleven: Vec<String> = (0..11)
        .map(|i| format!("https://openalex.org/A{i}"))
        .collect();
    let refs: Vec<&str> = eleven.iter().map(String::as_str).collect();
    let error = browse_parameters(&browse, &many("author", &refs, None), None).unwrap_err();
    assert!(error.contains("at most 10 values"), "{error}");
    // A mode named for a filter that does not exist is refused too.
    let mut request = browse_request(&[]);
    request.modes.insert("nope".into(), "any".into());
    let error = browse_parameters(&browse, &request, None).unwrap_err();
    assert!(error.contains("'nope' is not a filter"), "{error}");
}

#[test]
fn a_blank_filter_value_is_no_filter() {
    let browse = openalex_browse();
    let mut request = browse_request(&[("type", ""), ("year_from", ""), ("open_access", "")]);
    request
        .filters
        .insert("author".into(), FilterValue::Many(vec![]));
    let extra = browse_parameters(&browse, &request, None).unwrap();
    assert_eq!(
        extra,
        vec![("sort".to_string(), "cited_by_count:desc".to_string())]
    );
}

#[test]
fn browse_with_a_keyword_defaults_to_the_keyword_sort_and_sends_the_keyword() {
    let browse = openalex_browse();
    let extra = browse_parameters(&browse, &browse_request(&[]), Some("graph")).unwrap();
    assert_eq!(
        extra,
        vec![
            ("sort".to_string(), "relevance_score:desc".to_string()),
            ("query".to_string(), "graph".to_string()),
        ]
    );
    // An explicit sort beats the default of either kind.
    let mut request = browse_request(&[]);
    request.sort = Some("publication_date:desc".into());
    let extra = browse_parameters(&browse, &request, Some("graph")).unwrap();
    assert_eq!(extra[0].1, "publication_date:desc");
}

#[tokio::test]
async fn a_listing_says_how_many_match_in_all() {
    let mut server = mockito::Server::new_async().await;
    let _listing = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(OPENALEX_BODY.replacen('{', r#"{"meta":{"count":12430},"#, 1))
        .create_async()
        .await;
    let page = shipped("openalex", &server.url(), &[])
        .browse(&browse_request(&[("open_access", "true")]))
        .await
        .unwrap();
    assert_eq!(page.total, Some(12430));
    assert_eq!(page.results.len(), 1);
}

/// A count that is missing is absent; one that is there and is not a count
/// is the service having changed shape, and is not read as a number.
#[tokio::test]
async fn a_total_that_is_not_a_count_is_an_error_and_a_missing_one_is_absent() {
    let mut server = mockito::Server::new_async().await;
    let missing = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(OPENALEX_BODY)
        .expect(1)
        .create_async()
        .await;
    let source = shipped("openalex", &server.url(), &[]);
    let page = source.browse(&browse_request(&[])).await.unwrap();
    assert_eq!(page.total, None);
    missing.assert_async().await;
    missing.remove_async().await;

    let _wrong = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(OPENALEX_BODY.replacen('{', r#"{"meta":{"count":"many"},"#, 1))
        .create_async()
        .await;
    let error = source
        .browse(&browse_request(&[]))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("not a count"), "{error}");
}

#[tokio::test]
async fn a_filter_is_suggested_values_by_one_declared_request() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/autocomplete/authors")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("q".into(), "hinton".into()),
            Matcher::UrlEncoded("api_key".into(), "team-key".into()),
        ]))
        .with_status(200)
        .with_body(
            r#"{"results":[
                    {"id":"https://openalex.org/A1","display_name":"Geoffrey Hinton","hint":"University of Toronto"},
                    {"id":"https://openalex.org/A2","display_name":"G. Hinton","hint":null},
                    {"id":"A3,type:article","display_name":"Smuggler","hint":null},
                    {"id":null,"display_name":"Nameless","hint":null}
                ]}"#,
        )
        .create_async()
        .await;
    let source = shipped("openalex", &server.url(), &[("api_key", "team-key")]);
    let suggestions = source.suggest("author", " hinton ").await.unwrap();
    assert_eq!(
        suggestions,
        vec![
            FilterSuggestion {
                value: "https://openalex.org/A1".into(),
                label: "Geoffrey Hinton".into(),
                hint: Some("University of Toronto".into()),
            },
            // A missing hint is absent. The entry whose value would write
            // a clause, and the one with no value, are left out.
            FilterSuggestion {
                value: "https://openalex.org/A2".into(),
                label: "G. Hinton".into(),
                hint: None,
            },
        ]
    );
    mock.assert_async().await;
}

#[tokio::test]
async fn suggestions_are_refused_where_the_manifest_declares_none() {
    let source = shipped("openalex", "https://api.example.test", &[]);
    for (filter, text, expected) in [
        ("type", "art", "has no suggestions"),
        ("nope", "art", "is not a filter"),
        ("author", "h", "at least 2 characters"),
    ] {
        let error = source.suggest(filter, text).await.unwrap_err().to_string();
        assert!(error.contains(expected), "{filter}: {error}");
    }
}

#[tokio::test]
async fn browse_requests_the_page_the_limit_the_filters_and_the_sort() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/works")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("per-page".into(), "5".into()),
            Matcher::UrlEncoded("page".into(), "3".into()),
            Matcher::UrlEncoded(
                "filter".into(),
                "from_publication_date:2020-01-01,open_access.is_oa:true".into(),
            ),
            Matcher::UrlEncoded("sort".into(), "publication_date:desc".into()),
            Matcher::UrlEncoded("api_key".into(), "team-key".into()),
        ]))
        .with_status(200)
        .with_body(OPENALEX_BODY)
        .create_async()
        .await;

    let mut request = browse_request(&[("year_from", "2020"), ("open_access", "true")]);
    request.sort = Some("publication_date:desc".into());
    request.page = Some(3);
    request.limit = Some(5);
    let results = shipped("openalex", &server.url(), &[("api_key", "team-key")])
        .browse(&request)
        .await
        .unwrap();

    assert_eq!(results.results, vec![expected_openalex_record()]);
    mock.assert_async().await;
}

fn semantic_scholar_browse() -> BrowseCapability {
    builtin::manifests()
        .into_iter()
        .find(|manifest| manifest.id == "semantic_scholar")
        .unwrap()
        .capabilities
        .browse
        .unwrap()
}

/// The query string of the listing request a set of filters makes, with
/// the fixed `fields` parameter left out.
fn semantic_scholar_query(request: &BrowseRequest, query: Option<&str>) -> String {
    let browse = semantic_scholar_browse();
    let written = browse_parameters(&browse, request, query).unwrap();
    let path = browse_path(browse.path.as_deref().unwrap(), 10, 3, &written);
    let (_, query) = path.split_once('?').unwrap();
    query
        .split('&')
        .filter(|parameter| !parameter.starts_with("fields="))
        .collect::<Vec<_>>()
        .join("&")
}

/// A filter is written where the path names it, and one that is not set
/// sends nothing at all.
#[test]
fn a_filter_the_path_names_is_written_under_its_own_parameter() {
    assert_eq!(
        semantic_scholar_query(&browse_request(&[]), Some("crispr")),
        "query=crispr&limit=10&offset=20"
    );
    assert_eq!(
        semantic_scholar_query(
            &browse_request(&[("cited_at_least", "200"), ("open_access", "true")]),
            Some("gene editing"),
        ),
        "query=gene%20editing&minCitationCount=200&openAccessPdf&limit=10&offset=20"
    );
    assert_eq!(
        semantic_scholar_query(
            &many("type", &["Review", "JournalArticle"], None),
            Some("crispr"),
        ),
        "query=crispr&publicationTypes=Review%2CJournalArticle&limit=10&offset=20"
    );
}

/// Two filters in one parameter are a range, and it reads right with
/// either end left open.
#[test]
fn a_range_in_one_parameter_is_written_with_the_unset_end_empty() {
    for (filters, expected) in [
        (vec![("year_from", "2016")], "year=2016-"),
        (vec![("year_to", "2020")], "year=-2020"),
        (
            vec![("year_from", "2016"), ("year_to", "2020")],
            "year=2016-2020",
        ),
    ] {
        assert_eq!(
            semantic_scholar_query(&browse_request(&filters), Some("crispr")),
            format!("query=crispr&{expected}&limit=10&offset=20"),
            "{filters:?}"
        );
    }
}

#[test]
fn a_listing_that_needs_a_keyword_is_refused_without_one() {
    let browse = semantic_scholar_browse();
    let error =
        browse_parameters(&browse, &browse_request(&[("year_from", "2016")]), None).unwrap_err();
    assert!(error.contains("needs a keyword"), "{error}");
}

/// The separator joins the clauses of `{filters}` and is syntax nowhere
/// else; what a mode joins values by is syntax wherever the filter is.
#[test]
fn a_named_filter_refuses_only_the_syntax_of_where_it_is_written() {
    let browse = semantic_scholar_browse();
    let error = browse_parameters(
        &browse,
        &browse_request(&[("venue", "Nature,Radiology")]),
        Some("crispr"),
    )
    .unwrap_err();
    assert!(error.contains("cannot contain ','"), "{error}");
    assert_eq!(
        semantic_scholar_query(&browse_request(&[("venue", "N. Engl. J. Med.")]), Some("x")),
        "query=x&venue=N.%20Engl.%20J.%20Med.&limit=10&offset=20"
    );
}

#[test]
fn a_path_with_nothing_left_in_its_query_string_has_none() {
    assert_eq!(
        browse_path("/list?q={query}&f={filters}", 10, 1, &[]),
        "/list"
    );
    assert_eq!(
        browse_path("/list/{page}?q={query}&fixed=1", 10, 2, &[]),
        "/list/2?fixed=1"
    );
}

/// A placeholder standing by itself is written as parameters: each value
/// is one, its name and value encoded apart, so the `&` and `=` between
/// them stay syntax and the brackets in a name reach the service as such.
#[test]
fn a_placeholder_by_itself_writes_a_parameter_per_clause() {
    let written = [
        ("filters", "contentRating[]=safe"),
        ("filters", "contentRating[]=suggestive"),
        ("filters", "includedTags[]=a b&c"),
        ("sort", "order[followedCount]=desc"),
        ("flag", "hasAvailableChapters"),
    ]
    .map(|(name, value)| (name.to_string(), value.to_string()));
    assert_eq!(
        browse_path(
            "/manga?{filters}&{sort}&{flag}&limit={limit}",
            10,
            1,
            &written
        ),
        "/manga?contentRating%5B%5D=safe&contentRating%5B%5D=suggestive\
             &includedTags%5B%5D=a%20b%26c&order%5BfollowedCount%5D=desc\
             &hasAvailableChapters&limit=10"
    );
    assert_eq!(
        browse_path("/manga?{filters}&{sort}&limit={limit}", 10, 1, &[]),
        "/manga?limit=10"
    );
}

const REPEATED_KEYS_MANIFEST: &str = r#"manifest_version = 2
id = "repeated-keys"
name = "Repeated keys"

[http]
base_url = "BASE_URL"

[capabilities.search]
path = "/manga?title={query}&limit={limit}&offset={offset}"
items = "data"

[capabilities.search.fields]
id = "id"
title = "title"

[capabilities.browse]
path = "/manga?title={query}&{filters}&{sort}&{demographic}&limit={limit}&offset={offset}"
total = "total"

[capabilities.browse.sort]
default = "order[followedCount]=desc"
options = [["order[followedCount]=desc", "Most followed"], ["order[year]=asc", "Oldest"]]

[capabilities.browse.modes]
any = { label = "Any of", repeat = true }

[capabilities.browse.filters]
rating      = { label = "Rating", clause = "contentRating[]={value}", modes = ["any"], options = [["safe", "Safe"], ["suggestive", "Suggestive"]] }
year        = { label = "Year", clause = "year={value}", value = "integer" }
demographic = { label = "Demographic", clause = "publicationDemographic[]={value}", modes = ["any"], options = [["seinen", "Seinen"], ["josei", "Josei"]] }
author      = { label = "Author", clause = "author={value}", value = "text" }
"#;

/// The filter separator joins clauses in one value; where each clause is
/// a parameter of its own it separates nothing, so a value may carry it.
#[test]
fn a_value_written_as_its_own_parameter_may_carry_the_separator() {
    let browse = Manifest::parse(&REPEATED_KEYS_MANIFEST.replace("BASE_URL", "https://x.test"))
        .unwrap()
        .capabilities
        .browse
        .unwrap();
    let written =
        browse_parameters(&browse, &browse_request(&[("author", "Smith, J.")]), None).unwrap();
    assert_eq!(
        browse_path(browse.path.as_deref().unwrap(), 10, 1, &written),
        "/manga?author=Smith%2C%20J.&order%5BfollowedCount%5D=desc&limit=10&offset=0"
    );
}

/// Where the path writes parameters, a repeating mode sends the key once
/// per value — for the filters it names as well as for `{filters}` — and
/// the sort option is a parameter of its own.
#[tokio::test]
async fn a_listing_whose_path_writes_parameters_repeats_the_key_per_value() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/manga")
        .match_query(Matcher::Exact(
            "contentRating%5B%5D=safe&contentRating%5B%5D=suggestive&year=2016\
                 &order%5Byear%5D=asc\
                 &publicationDemographic%5B%5D=seinen&publicationDemographic%5B%5D=josei\
                 &limit=5&offset=5"
                .into(),
        ))
        .with_status(200)
        .with_body(r#"{"total": 7, "data": [{"id": "m1", "title": "One"}]}"#)
        .create_async()
        .await;

    let mut request = many("rating", &["safe", "suggestive"], None);
    request.filters.insert(
        "demographic".into(),
        FilterValue::Many(vec!["seinen".into(), "josei".into()]),
    );
    request
        .filters
        .insert("year".into(), FilterValue::One("2016".into()));
    request.sort = Some("order[year]=asc".into());
    request.page = Some(2);
    request.limit = Some(5);
    let page = source(REPEATED_KEYS_MANIFEST, &server.url(), &[])
        .browse(&request)
        .await
        .unwrap();

    assert_eq!(page.total, Some(7));
    assert_eq!(page.results.len(), 1);
    mock.assert_async().await;
}

#[tokio::test]
async fn a_listing_with_a_parameter_per_filter_sends_each_where_it_is_named() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/graph/v1/paper/search")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("query".into(), "crispr".into()),
            Matcher::UrlEncoded("year".into(), "2016-".into()),
            Matcher::UrlEncoded("fieldsOfStudy".into(), "Biology,Medicine".into()),
            Matcher::UrlEncoded("limit".into(), "5".into()),
            Matcher::UrlEncoded("offset".into(), "5".into()),
        ]))
        .with_status(200)
        .with_body(r#"{"total": 42, "data": [{"paperId": "p1", "title": "One"}]}"#)
        .create_async()
        .await;

    let mut request = many("field", &["Biology", "Medicine"], None);
    request
        .filters
        .insert("year_from".into(), FilterValue::One("2016".into()));
    request.query = Some("crispr".into());
    request.page = Some(2);
    request.limit = Some(5);
    let page = shipped("semantic_scholar", &server.url(), &[])
        .browse(&request)
        .await
        .unwrap();

    assert_eq!(page.total, Some(42));
    assert_eq!(page.results.len(), 1);
    mock.assert_async().await;
}

#[tokio::test]
async fn browse_without_a_keyword_sends_no_search_parameter() {
    let mut server = mockito::Server::new_async().await;
    let with_search = server
        .mock("GET", "/works")
        .match_query(Matcher::Regex("search=".into()))
        .expect(0)
        .create_async()
        .await;
    let without = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(OPENALEX_BODY)
        .expect(1)
        .create_async()
        .await;
    shipped("openalex", &server.url(), &[])
        .browse(&browse_request(&[]))
        .await
        .unwrap();
    with_search.assert_async().await;
    without.assert_async().await;
}

#[tokio::test]
async fn a_provider_that_declares_no_browse_refuses_by_name() {
    let error = source(KEYED_MANIFEST, "https://example.test", &[("api_key", "k")])
        .browse(&BrowseRequest::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("does not offer browse"), "{error}");
}

// ── Search: HTML and download resolution ─────────────────────────────────

#[tokio::test]
async fn anna_html_search_and_credentialed_download_resolution_are_manifest_expressible() {
    let mut server = mockito::Server::new_async().await;
    let search = server
        .mock("GET", "/search")
        .match_query(Matcher::Any)
        .match_header("user-agent", "Mozilla/5.0 Wilkes")
        .with_status(200)
        .with_body(ANNA_BODY)
        .create_async()
        .await;
    let resolve = server
        .mock("GET", "/dyn/api/fast_download.json")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("md5".into(), "deadbeef".into()),
            Matcher::UrlEncoded("key".into(), "secret".into()),
        ]))
        .with_status(200)
        .with_body(r#"{"download_url":"https://files.example.test/book"}"#)
        .create_async()
        .await;

    let anna = source(ANNA_MANIFEST, &server.url(), &[("anna_key", "secret")]);
    let results = anna.search("graph networks", 1, 1).await.unwrap();
    assert_eq!(results.len(), 1);
    let book = &results[0];
    assert_eq!(book.id, "deadbeef");
    assert_eq!(book.title.as_deref(), Some("Graph: Networks"));
    assert_eq!(book.authors.as_deref(), Some("Ada Lovelace, Grace Hopper"));
    assert_eq!(book.publisher.as_deref(), Some("Example Journal"));
    assert_eq!(book.language.as_deref(), Some("English"));
    assert_eq!(book.file_format.as_deref(), Some("PDF"));
    assert_eq!(book.file_size.as_deref(), Some("2.4MB"));
    assert_eq!(book.acquisition, LiteratureAcquisition::Provider);

    let download = fetched(anna.resolve_download(book).await.unwrap().unwrap());
    assert_eq!(download.url, "https://files.example.test/book");
    assert_eq!(download.filename.as_deref(), Some("Graph_ Networks.PDF"));
    search.assert_async().await;
    resolve.assert_async().await;
}

/// Anna, resolving through a page that links the file when there is one.
fn page_resolver(md5_field: &str) -> String {
    let start = ANNA_MANIFEST
        .find("[capabilities.resolve_download]")
        .unwrap();
    format!(
        "{}[capabilities.resolve_download]\nurl = \"/get/{{md5}}\"\nsteps = [\n  {{ path = \"/scidb?id={{id}}\", response_format = \"html\", fields = {{ md5 = {md5_field} }} }},\n]\n",
        &ANNA_MANIFEST[..start]
    )
}

const MD5_OR_UNAVAILABLE: &str = r#"{ path = "a[href*='md5=']", attribute = "href", capture = "(?i)md5=([a-f0-9]{32})", missing = "unavailable" }"#;

/// One Anna result, and what its resolver makes of the page it is served.
async fn resolve_through_page(
    md5_field: &str,
    page: &str,
) -> anyhow::Result<Option<ResolvedLiteratureDownload>> {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/search")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(ANNA_BODY)
        .create_async()
        .await;
    server
        .mock("GET", "/scidb")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(page)
        .create_async()
        .await;
    let anna = source(&page_resolver(md5_field), &server.url(), &[]);
    let book = anna.search("graph networks", 1, 1).await.unwrap().remove(0);
    anna.resolve_download(&book)
        .await
        .map(|plan| plan.map(fetched))
}

const PAGE_WITH_FILE: &str =
    r#"<a href="/dl?md5=0123456789abcdef0123456789ABCDEF&x=1">Download</a>"#;
const PAGE_WITHOUT_FILE: &str = r#"<p>Not held.</p><a href="/about">About</a>"#;

#[tokio::test]
async fn a_field_the_manifest_calls_unavailable_when_missing_is_no_file_not_a_failure() {
    let held = resolve_through_page(MD5_OR_UNAVAILABLE, PAGE_WITH_FILE)
        .await
        .unwrap()
        .expect("the page links a file");
    assert!(
        held.url.ends_with("/get/0123456789abcdef0123456789ABCDEF"),
        "{}",
        held.url
    );

    let absent = resolve_through_page(MD5_OR_UNAVAILABLE, PAGE_WITHOUT_FILE)
        .await
        .unwrap();
    assert_eq!(absent, None);
}

/// Only the manifest can say an absence is an answer. Unsaid, a selector
/// that finds nothing is a manifest that no longer describes the page.
#[tokio::test]
async fn a_missing_field_is_an_error_unless_the_manifest_says_otherwise() {
    let unsaid = MD5_OR_UNAVAILABLE.replace(r#", missing = "unavailable""#, "");
    let error = resolve_through_page(&unsaid, PAGE_WITHOUT_FILE)
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        "resolver step 1 response: field 'md5' matched nothing"
    );
}

/// The link is there and is not what the manifest says a link looks like:
/// that is the manifest being wrong about the page, whatever it says about
/// a link that is absent.
#[tokio::test]
async fn a_value_that_is_found_and_does_not_fit_is_an_error_even_where_missing_is_unavailable() {
    let error = resolve_through_page(
        MD5_OR_UNAVAILABLE,
        r#"<a href="/dl?md5=renamed-scheme">Download</a>"#,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("field 'md5'"), "{error}");
    assert!(error.contains("capture group 1 matched nothing"), "{error}");
}

/// Anna's one result resolved under `url`, and how often the resolver's
/// page was asked for.
async fn resolve_with_url(
    url: &str,
    page_requests: usize,
) -> anyhow::Result<Option<ResolvedLiteratureDownload>> {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/search")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(ANNA_BODY)
        .create_async()
        .await;
    let page = server
        .mock("GET", "/scidb")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(PAGE_WITH_FILE)
        .expect(page_requests)
        .create_async()
        .await;
    let manifest = page_resolver(MD5_OR_UNAVAILABLE);
    assert!(manifest.contains(r#"url = "/get/{md5}""#));
    let manifest = manifest.replace(r#"url = "/get/{md5}""#, &format!("url = {url}"));
    let anna = source(&manifest, &server.url(), &[]);
    let book = anna.search("graph networks", 1, 1).await.unwrap().remove(0);
    let resolved = anna.resolve_download(&book).await;
    page.assert_async().await;
    resolved.map(|plan| plan.map(fetched))
}

/// The result already holds a place the file may be, so nothing is asked.
#[tokio::test]
async fn a_listed_url_the_result_already_holds_costs_no_request() {
    let download = resolve_with_url(r#"["{landing_page_url}", "/get/{md5}"]"#, 0)
        .await
        .unwrap()
        .unwrap();
    assert!(download.url.ends_with("/md5/deadbeef"), "{}", download.url);
}

/// Anna's results carry no `pdf_url`, so the step is what finds the file.
#[tokio::test]
async fn steps_run_while_no_listed_url_can_be_written() {
    let download = resolve_with_url(r#"["{pdf_url}", "/get/{md5}"]"#, 1)
        .await
        .unwrap()
        .unwrap();
    assert!(
        download
            .url
            .ends_with("/get/0123456789abcdef0123456789ABCDEF"),
        "{}",
        download.url
    );
}

/// One template is no choice: its steps run although it names nothing
/// they find, as they always have.
#[tokio::test]
async fn a_single_url_runs_every_step() {
    let download = resolve_with_url(r#""{landing_page_url}""#, 1)
        .await
        .unwrap()
        .unwrap();
    assert!(download.url.ends_with("/md5/deadbeef"), "{}", download.url);
}

#[tokio::test]
async fn a_list_none_of_which_can_be_written_says_so() {
    let error = resolve_with_url(r#"["{pdf_url}", "{doi}"]"#, 1)
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        "resolve_download.url: none of its templates can be written from what is known: {pdf_url}, {doi}"
    );
}

#[tokio::test]
async fn a_probe_whose_example_has_no_file_says_the_resolver_was_not_checked() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/search")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(ANNA_BODY)
        .create_async()
        .await;
    server
        .mock("GET", "/scidb")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(PAGE_WITHOUT_FILE)
        .create_async()
        .await;
    let report = source(&page_resolver(MD5_OR_UNAVAILABLE), &server.url(), &[])
        .probe("graph networks")
        .await;
    // Not clean — no step reached a URL — and not called a failure either.
    assert!(!report.ok);
    let said = report.error.unwrap();
    assert!(said.contains("was not checked"), "{said}");
    assert!(said.contains("has no file for the first result"), "{said}");
    assert!(!said.contains("failed"), "{said}");
}

#[tokio::test]
async fn cross_origin_resolver_uses_its_own_base_and_does_not_receive_global_headers() {
    let mut search_server = mockito::Server::new_async().await;
    let mut resolver_server = mockito::Server::new_async().await;
    let search = search_server
        .mock("GET", "/search")
        .match_query(Matcher::Any)
        .match_header("user-agent", "Mozilla/5.0 Wilkes")
        .with_status(200)
        .with_body(ANNA_BODY)
        .create_async()
        .await;
    let resolve = resolver_server
        .mock("GET", "/dyn/api/fast_download.json")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("md5".into(), "deadbeef".into()),
            Matcher::UrlEncoded("key".into(), "secret".into()),
        ]))
        .match_header("user-agent", Matcher::Missing)
        .with_status(200)
        .with_body(r#"{"download_url":"/files/book.pdf"}"#)
        .create_async()
        .await;

    let manifest = ANNA_MANIFEST
        .replace(
            "{ path = \"/dyn/api/fast_download.json?md5={id}\"",
            &format!(
                "{{ base_url = {:?}, path = \"/dyn/api/fast_download.json?md5={{id}}\"",
                resolver_server.url()
            ),
        )
        .replace(
            "download_url = \"download_url\"",
            "download_url = \"absolute_url(download_url)\"",
        );
    assert!(
        manifest.contains("base_url = \"http"),
        "the step was not moved"
    );
    let anna = source(&manifest, &search_server.url(), &[("anna_key", "secret")]);
    let book = anna.search("graph networks", 1, 1).await.unwrap().remove(0);
    let download = fetched(anna.resolve_download(&book).await.unwrap().unwrap());

    assert_eq!(
        download.url,
        format!("{}/files/book.pdf", resolver_server.url())
    );
    search.assert_async().await;
    resolve.assert_async().await;
}

#[tokio::test]
async fn a_same_origin_step_inherits_global_headers_and_may_replace_one() {
    let manifest = ANNA_MANIFEST.replace(
        "query = { key = { secret = \"anna_key\" } }",
        "headers = { user-agent = \"Step/1\" }, query = { key = { secret = \"anna_key\" } }",
    );
    assert!(manifest.contains("Step/1"), "the step header was not added");
    let mut server = mockito::Server::new_async().await;
    let _search = server
        .mock("GET", "/search")
        .match_query(Matcher::Any)
        .match_header("user-agent", "Mozilla/5.0 Wilkes")
        .with_status(200)
        .with_body(ANNA_BODY)
        .create_async()
        .await;
    // The step's header of the same name replaces the primary one rather
    // than being sent beside it.
    let resolve = server
        .mock("GET", "/dyn/api/fast_download.json")
        .match_query(Matcher::Any)
        .match_header("user-agent", "Step/1")
        .with_status(200)
        .with_body(r#"{"download_url":"https://files.example.test/book"}"#)
        .expect(1)
        .create_async()
        .await;
    let anna = source(&manifest, &server.url(), &[("anna_key", "secret")]);
    let book = anna.search("graph networks", 1, 1).await.unwrap().remove(0);
    anna.resolve_download(&book).await.unwrap().unwrap();
    resolve.assert_async().await;
}

#[tokio::test]
async fn anna_doi_input_can_flow_to_the_scidb_paper_download() {
    let mut server = mockito::Server::new_async().await;
    let search = server
        .mock("GET", "/scidb/10.1234%2Fexample")
        .with_status(200)
        .with_body(ANNA_BODY)
        .create_async()
        .await;

    let anna = source(ANNA_DOI_MANIFEST, &server.url(), &[]);
    let result = anna
        .search("10.1234/example", 1, 1)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(result.doi.as_deref(), Some("10.1234/example"));

    let download = fetched(anna.resolve_download(&result).await.unwrap().unwrap());
    assert_eq!(
        download.url,
        format!("{}/scidb?doi=10.1234%2Fexample", server.url())
    );
    assert_eq!(download.filename.as_deref(), Some("Graph_ Networks"));
    search.assert_async().await;
}

// ── Secrets and the pinned origin ────────────────────────────────────────

#[tokio::test]
async fn a_required_secret_is_needed_before_a_request_is_made() {
    let error = source(KEYED_MANIFEST, "https://example.test", &[])
        .search("q", 1, 1)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("secret 'api_key'"), "{error}");
}

/// The host is pinned by two independent things, and this checks both.
///
/// Validation refuses a path that does not start with `/`, which is what
/// stops `@evil.test/` from being read as userinfo and moving the host.
/// Concatenation then keeps a `//evil.test` path a *path* — the origin
/// check in `request` is the backstop that would catch it if either of
/// those ever stopped holding.
#[tokio::test]
async fn the_host_is_pinned_against_a_path_that_tries_to_move_it() {
    let escaping = KEYED_MANIFEST.replace(
        "path = \"/graph/v1/paper/search?query={query}",
        "path = \"@evil.test/graph/v1/paper/search?query={query}",
    );
    let error = Manifest::parse(&escaping.replace("BASE_URL", "https://api.example.test"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("must start with '/'"), "{error}");

    let doubled = KEYED_MANIFEST.replace(
        "path = \"/graph/v1/paper/search?query={query}",
        "path = \"//evil.test/graph/v1/paper/search?query={query}",
    );
    let pinned = source(&doubled, "https://api.example.test", &[("api_key", "k")]);
    let request = pinned.request("//evil.test/works").unwrap();
    assert!(
        request
            .url
            .starts_with("https://api.example.test//evil.test/works"),
        "{}",
        request.url
    );
}

// ── Probe ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn probe_reports_an_unmapped_field_by_name_and_refuses_to_pass() {
    // `citationCount` arrives as a string where the manifest promised an
    // integer: present, unusable, and reported rather than nulled.
    let body = r#"{"data":[{"paperId":"p1","title":"T","citationCount":"many"}]}"#;
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/graph/v1/paper/search")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(body)
        .create_async()
        .await;

    let report = source(KEYED_MANIFEST, &server.url(), &[("api_key", "secret")])
        .probe("graph neural networks")
        .await;

    assert!(!report.ok, "a probe with unmapped fields is not clean");
    assert_eq!(report.issues.len(), 1);
    assert_eq!(report.issues[0].field, "citation_count");
    assert!(report.issues[0].problem.contains("integer"));
    // The record still projected: the failure is per field, not per record.
    assert_eq!(report.results.len(), 1);
    assert!(report.raw_response.contains("many"));
    mock.assert_async().await;
}

#[tokio::test]
async fn probe_redacts_a_secret_sent_as_a_query_parameter() {
    let manifest = KEYED_MANIFEST.replace("[http.headers]", "[http.query]");
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/graph/v1/paper/search")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(SEMANTIC_SCHOLAR_BODY)
        .create_async()
        .await;

    let report = source(&manifest, &server.url(), &[("api_key", "hunter2")])
        .probe("custom probe terms")
        .await;

    assert!(report.ok, "{:?}", report.error);
    assert!(
        !report.request_url.contains("hunter2"),
        "{}",
        report.request_url
    );
    assert!(report.request_url.contains("***"), "{}", report.request_url);
    let request_url = Url::parse(&report.request_url).unwrap();
    assert!(request_url
        .query_pairs()
        .any(|(name, value)| name == "query" && value == "custom probe terms"));
    mock.assert_async().await;
}

#[tokio::test]
async fn a_blank_probe_query_is_refused_before_any_request() {
    let report = source(KEYED_MANIFEST, "https://example.test", &[("api_key", "k")])
        .probe("   ")
        .await;

    assert!(!report.ok);
    assert_eq!(report.error.as_deref(), Some("probe query cannot be empty"));
    assert!(report.request_url.is_empty());
}

#[test]
fn surfaced_errors_redact_every_configured_secret() {
    let source = source(
        KEYED_MANIFEST,
        "https://example.test",
        &[("api_key", "hunter2"), ("shorter", "hunter")],
    );

    let error = source.redact_error(
        "request failed with hunter2; nested cause repeated hunter2 and hunter".to_string(),
    );

    assert_eq!(
        error,
        "request failed with ***; nested cause repeated *** and ***"
    );
}

#[tokio::test]
async fn a_record_without_an_id_is_skipped_and_said_so() {
    let body = r#"{"data":[{"title":"no id here"},{"paperId":"p2","title":"T"}]}"#;
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/graph/v1/paper/search")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(body)
        .create_async()
        .await;

    let report = source(KEYED_MANIFEST, &server.url(), &[("api_key", "secret")])
        .probe("graph neural networks")
        .await;

    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].id, "p2");
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.record == 0 && issue.problem.contains("skipped")));
    mock.assert_async().await;
}

#[tokio::test]
async fn an_items_expression_that_matches_nothing_names_itself() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/graph/v1/paper/search")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(r#"{"papers":[]}"#)
        .create_async()
        .await;

    let report = source(KEYED_MANIFEST, &server.url(), &[("api_key", "secret")])
        .probe("graph neural networks")
        .await;

    assert!(!report.ok);
    let error = report.error.unwrap();
    assert!(error.contains("expression 'data'"), "{error}");
    mock.assert_async().await;
}

// ── Capabilities ─────────────────────────────────────────────────────────

#[test]
fn capabilities_report_what_the_manifest_declares() {
    let capabilities = shipped("openalex", "https://example.test", &[]).capabilities();
    assert!(capabilities.lookup && capabilities.references);
    assert!(!capabilities.resolve_download);
    let controls = capabilities.browse.expect("openalex lists works");
    assert!(controls.keyword);
    assert!(controls
        .filters
        .iter()
        .any(|filter| filter.name == "year_from"));

    let capabilities = source(KEYED_MANIFEST, "https://example.test", &[]).capabilities();
    assert!(capabilities.browse.is_none() && !capabilities.lookup && !capabilities.references);
}
