use super::*;

const CROSSREF: &str = r#"
manifest_version = 2
id = "crossref"
name = "Crossref"

[http]
base_url = "https://api.crossref.org"

[http.query]
mailto = "team@example.com"

[capabilities.health]
path = "/works/10.1145/3801158"

[capabilities.search]
path = "/works?query.bibliographic={query}&rows={limit}"
items = "message.items"

[capabilities.search.fields]
id = "DOI"
title = "title[0]"
doi = "normalize_doi(DOI)"
year = "published.\"date-parts\"[0][0]"
citation_count = "\"is-referenced-by-count\""
pdf_url = "not_null(link[0].URL, resource.primary.URL)"
"#;

const HTML: &str = r#"
manifest_version = 2
id = "html"
name = "HTML"
[http]
base_url = "https://example.test"
[capabilities.search]
path = "/search?q={query}"
response_format = "html"
items = '''article:has(a[href^="/work/"])'''
[capabilities.search.fields]
id = { path = '''a[href^="/work/"]''', attribute = "href", capture = '''^/work/(.+)$''' }
title = "h2"
"#;

#[test]
fn a_manifest_reaches_no_further_than_one_naming_the_same_origins_and_programs() {
    let previous = Manifest::parse(CROSSREF).unwrap();
    let moved = Manifest::parse(&CROSSREF.replace("api.crossref.org", "elsewhere.test")).unwrap();
    let running = Manifest::parse(&format!(
        "{CROSSREF}\n[capabilities.resolve_download]\nrun = {{ program = \"fetcher\", args = [\"{{id}}\"] }}\n"
    ))
    .unwrap();
    assert!(previous.reaches_no_further_than(&previous));
    assert!(!moved.reaches_no_further_than(&previous));
    assert!(!running.reaches_no_further_than(&previous));
    assert!(previous.reaches_no_further_than(&running));
}

#[test]
fn a_request_is_a_path_or_a_run_and_a_path_needs_an_origin() {
    let base = |http: &str, search: &str, rest: &str| {
        format!(
            "manifest_version = 2\nid = \"x\"\nname = \"X\"\n{http}\n[capabilities.search]\n{search}\n[capabilities.search.fields]\nid = \"id\"\ntitle = \"title\"\n{rest}"
        )
    };
    let http = "[http]\nbase_url = \"https://x.test\"";
    let run = r#"run = { program = "fetch", args = ["--q", "{query}"] }"#;

    // A run needs no origin, and its program is named with the others.
    let cli = Manifest::parse(&base(
        "",
        run,
        "[capabilities.lookup]\npaths = [{ program = \"fetch\", args = [\"{doi}\"] }, { program = \"other\", args = [\"{doi}\"] }]\n[capabilities.lookup.fields]\nid = \"id\"",
    ))
    .unwrap();
    assert_eq!(cli.origins(), Vec::<String>::new());
    assert_eq!(cli.programs(), ["fetch", "other"]);

    for (source, expected) in [
        (base(http, &format!("path = \"/s?q={{query}}\"\n{run}"), ""), "search sets both path and run"),
        (base(http, "", ""), "search sets neither path nor run"),
        (
            base("", "path = \"/s?q={query}\"", ""),
            "search.path is requested from http.base_url, but the manifest declares no [http]",
        ),
        (
            base("", r#"run = { program = "fetch", args = [["--q", "{query}"]] }"#, ""),
            "search.run.args[0]: a group of arguments is for browse",
        ),
        (
            base("", run, "[capabilities.lookup]\npaths = [{ program = \"fetch\", args = [\"x\"] }]\n[capabilities.lookup.fields]\nid = \"id\""),
            "lookup.paths[0] must contain {doi}",
        ),
        (
            base("", run, "[capabilities.resolve_download]\nurl = \"{u}\"\nsteps = [{ run = { program = \"fetch\", args = [\"{id}\"] }, headers = { A = \"b\" }, fields = { u = \"url\" } }]"),
            "base_url, headers and query are for a path",
        ),
        (
            base("", run, "[capabilities.browse]\nrun = { program = \"fetch\", args = [\"--f=a,{filters}\"] }\n[capabilities.browse.filters]\noa = { label = \"OA\", clause = \"oa\" }"),
            "browse.run: '--f=a,{filters}' must end with {filters}",
        ),
    ] {
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains(expected), "{source}\n{error}");
    }
}

#[test]
fn parses_a_toml_manifest() {
    let manifest = Manifest::parse(CROSSREF).unwrap();
    assert_eq!(manifest.id, "crossref");
    assert_eq!(manifest.origins(), vec!["https://api.crossref.org"]);
    assert!(manifest.required_secrets().is_empty());
    assert_eq!(
        manifest.http.as_ref().unwrap().query.get("mailto"),
        Some(&ParamValue::Literal("team@example.com".to_string()))
    );
}

#[test]
fn a_version_one_manifest_is_refused_by_number() {
    let error = Manifest::parse(&CROSSREF.replace("version = 2", "version = 1"))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("manifest_version 1 is not supported"),
        "{error}"
    );
}

#[test]
fn resolver_step_origins_are_literal_visible_and_deduplicated() {
    let source = format!(
        "{HTML}\n[capabilities.resolve_download]\nurl = \"{{download_url}}\"\n\
             [[capabilities.resolve_download.steps]]\n\
             base_url = \"https://downloads.example.test/api\"\n\
             path = \"/resolve?id={{id}}\"\n\
             [capabilities.resolve_download.steps.fields]\n\
             download_url = \"url\"\n\
             [[capabilities.resolve_download.steps]]\n\
             base_url = \"https://downloads.example.test/other\"\n\
             path = \"/confirm?url={{download_url}}\"\n\
             [capabilities.resolve_download.steps.fields]\n\
             confirmed_url = \"url\"\n"
    );
    let manifest = Manifest::parse(&source).unwrap();
    assert_eq!(
        manifest.origins(),
        vec!["https://example.test", "https://downloads.example.test"]
    );
}

#[test]
fn rejects_a_templated_or_non_http_resolver_base_url() {
    let template = format!(
        "{HTML}\n[capabilities.resolve_download]\nurl = \"{{download_url}}\"\n\
             [[capabilities.resolve_download.steps]]\n\
             base_url = \"https://{{mirror}}.example.test\"\n\
             path = \"/resolve?id={{id}}\"\n\
             [capabilities.resolve_download.steps.fields]\n\
             download_url = \"url\"\n"
    );
    let error = Manifest::parse(&template).unwrap_err().to_string();
    assert!(
        error.contains("literal URL without placeholders"),
        "{error}"
    );

    let non_http = template.replace(
        "https://{mirror}.example.test",
        "file:///private/download.json",
    );
    let error = Manifest::parse(&non_http).unwrap_err().to_string();
    assert!(error.contains("not http or https"), "{error}");
}

#[test]
fn round_trips_through_json_and_toml() {
    let manifest = Manifest::parse(CROSSREF).unwrap();
    let json = serde_json::to_string(&manifest).unwrap();
    assert_eq!(Manifest::parse(&json).unwrap(), manifest);
    let toml = toml::to_string(&manifest).unwrap();
    assert_eq!(Manifest::parse(&toml).unwrap(), manifest);
}

#[test]
fn rejects_an_unknown_result_field() {
    let source = CROSSREF.replace("citation_count = ", "citations = ");
    let error = Manifest::parse(&source).unwrap_err().to_string();
    assert!(error.contains("citations"), "{error}");
    assert!(error.contains("is not a result field"), "{error}");
}

#[test]
fn rejects_an_unavailable_placeholder() {
    let source = CROSSREF.replace("{query}", "{doi}");
    let error = Manifest::parse(&source).unwrap_err().to_string();
    assert!(error.contains("{doi}"), "{error}");
    assert!(error.contains("not available here"), "{error}");
}

#[test]
fn rejects_an_expression_that_does_not_parse_or_calls_an_unknown_function() {
    let error = Manifest::parse(&CROSSREF.replace(r#"title = "title[0]""#, r#"title = "title[""#))
        .unwrap_err()
        .to_string();
    assert!(error.contains("search.fields.title"), "{error}");
    let error = Manifest::parse(
        &CROSSREF.replace(r#"title = "title[0]""#, r#"title = "shell_out(title)""#),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("shell_out"), "{error}");
}

#[test]
fn rejects_a_missing_required_field() {
    let source = CROSSREF.replace(r#"title = "title[0]""#, "");
    let error = Manifest::parse(&source).unwrap_err().to_string();
    assert!(error.contains("missing 'title'"), "{error}");
}

#[test]
fn rejects_a_non_http_base_url() {
    let source = CROSSREF.replace("https://api.crossref.org", "file:///etc/passwd");
    let error = Manifest::parse(&source).unwrap_err().to_string();
    assert!(error.contains("not http or https"), "{error}");
}

#[test]
fn a_secret_may_be_required_or_optional_and_is_listed_once() {
    let source = CROSSREF.replace(
        r#"mailto = "team@example.com""#,
        r#"mailto = { secret = "email", optional = true, label = "Email", kind = "email" }
token = { secret = "token" }
other = { secret = "email" }"#,
    );
    let manifest = Manifest::parse(&source).unwrap();
    // `email` is used both as optional and as required: required wins.
    assert_eq!(manifest.required_secrets(), vec!["email", "token"]);
    assert!(manifest.optional_secrets().is_empty());
    let slots = manifest.secret_slots();
    assert_eq!(slots.iter().filter(|slot| slot.name == "email").count(), 1);
    assert_eq!(slots[0].kind, SecretKind::Email);
    assert_eq!(slots[0].label, "Email");
}

#[test]
fn an_optional_secret_is_not_required() {
    let source = CROSSREF.replace(
        r#"mailto = "team@example.com""#,
        r#"mailto = { secret = "email", optional = true }"#,
    );
    let manifest = Manifest::parse(&source).unwrap();
    assert!(manifest.required_secrets().is_empty());
    assert_eq!(manifest.optional_secrets(), vec!["email"]);
}

#[test]
fn a_header_map_is_validated_for_names() {
    let source = CROSSREF.replace(
        "[http.query]",
        "[http.headers]\n\"bad name\" = \"x\"\n[http.query]",
    );
    let error = Manifest::parse(&source).unwrap_err().to_string();
    assert!(error.contains("not a valid header name"), "{error}");
}

#[test]
fn reports_every_problem_at_once() {
    let source = CROSSREF
        .replace("citation_count = ", "citations = ")
        .replace("{query}", "{doi}");
    let error = Manifest::parse(&source).unwrap_err().to_string();
    assert!(error.contains("citations"), "{error}");
    assert!(error.contains("{doi}"), "{error}");
}

#[test]
fn validates_html_css_attributes_and_captures() {
    let manifest = Manifest::parse(HTML).unwrap();
    assert_eq!(
        manifest.capabilities.search.unwrap().response_format,
        ResponseFormat::Html
    );
}

#[test]
fn rejects_an_invalid_css_selector_before_network_use() {
    let error = Manifest::parse(&HTML.replace("title = \"h2\"", "title = \"h2[\""))
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid CSS selector"), "{error}");
}

#[test]
fn rejects_html_only_options_on_json_fields() {
    for replacement in [
        r#"title = { path = "title[0]", attribute = "href" }"#,
        r#"title = { path = "title[0]", all = true }"#,
        r#"title = { path = "title[0]", then = "@" }"#,
        r#"title = { path = "title[0]", first_of = ["a"] }"#,
    ] {
        let source = CROSSREF.replace(r#"title = "title[0]""#, replacement);
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(
            error.contains("search.fields.title"),
            "{replacement}: {error}"
        );
    }
}

#[test]
fn resolver_templates_cannot_read_an_undeclared_value() {
    let source =
        format!("{HTML}\n[capabilities.resolve_download]\nurl = \"{{not_from_any_step}}\"\n");
    let error = Manifest::parse(&source).unwrap_err().to_string();
    assert!(error.contains("not_from_any_step"), "{error}");
    assert!(error.contains("not available"), "{error}");
}

#[test]
fn every_capability_but_health_needs_search() {
    let source = r#"
manifest_version = 2
id = "orphan"
name = "Orphan"
[http]
base_url = "https://example.test"
[capabilities.resolve_download]
url = "{id}"
"#;
    let error = Manifest::parse(source).unwrap_err().to_string();
    assert!(
        error.contains("resolve_download requires search"),
        "{error}"
    );
}

#[test]
fn equivalences_validate_names_shapes_uniqueness_and_option_maps() {
    let base = Manifest::parse(include_str!("../../builtin/openalex.toml")).unwrap();
    assert!(base.problems().is_empty());
    for (case, expected) in [
        (0, "unknown equivalence"),
        (1, "needs integer"),
        (2, "duplicate equivalence"),
        (3, "result-only"),
        (4, "is not an option"),
        (5, "maps two options"),
        (6, "maps two options"),
        (7, "unknown equivalence"),
        (8, "is not an option"),
        (9, "requires a tagged options filter"),
    ] {
        let mut manifest = base.clone();
        let browse = manifest.capabilities.browse.as_mut().unwrap();
        match case {
            0 => browse.filters.get_mut("year_from").unwrap().equivalent_to = Some("bogus".into()),
            1 => browse.filters.get_mut("type").unwrap().equivalent_to = Some("year_from".into()),
            2 => {
                browse.filters.get_mut("year_to").unwrap().equivalent_to = Some("year_from".into())
            }
            3 => {
                let f = browse.filters.get_mut("cites").unwrap();
                f.suggest = None;
                f.equivalent_to = Some("author".into());
            }
            4 => {
                browse
                    .filters
                    .get_mut("type")
                    .unwrap()
                    .equivalent_values
                    .insert("absent".into(), "thesis".into());
            }
            5 => {
                let f = browse.filters.get_mut("type").unwrap();
                f.equivalent_values
                    .insert("article".into(), "shared".into());
                f.equivalent_values.insert("review".into(), "shared".into());
            }
            6 => {
                browse
                    .filters
                    .get_mut("type")
                    .unwrap()
                    .equivalent_values
                    .insert("article".into(), "review".into());
            }
            7 => {
                browse
                    .sort
                    .as_mut()
                    .unwrap()
                    .equivalent_to
                    .insert("cited_by_count:desc".into(), "bogus".into());
            }
            8 => {
                browse
                    .sort
                    .as_mut()
                    .unwrap()
                    .equivalent_to
                    .insert("absent".into(), "newest".into());
            }
            9 => {
                let f = browse.filters.get_mut("type").unwrap();
                f.equivalent_to = None;
                f.equivalent_values.insert("article".into(), "paper".into());
            }
            _ => unreachable!(),
        }
        let problems = manifest.problems();
        assert!(
            problems.iter().any(|p| p.contains(expected)),
            "case {case}: {problems:?}"
        );
    }
    let mut manifest = base;
    let f = manifest
        .capabilities
        .browse
        .as_mut()
        .unwrap()
        .filters
        .get_mut("type")
        .unwrap();
    f.equivalent_values
        .insert("dissertation".into(), "thesis".into());
    assert!(manifest.problems().is_empty());
    assert_eq!(
        manifest
            .capabilities
            .browse
            .unwrap()
            .controls()
            .filters
            .iter()
            .find(|f| f.name == "type")
            .unwrap()
            .equivalent_values["dissertation"],
        "thesis"
    );
}

#[test]
fn browse_declares_controls_the_interface_can_render() {
    let manifest = Manifest::parse(AUTHORING_FULL_EXAMPLE).unwrap();
    let controls = manifest.capabilities.browse.unwrap().controls();
    assert!(controls.keyword);
    assert!(controls.total);
    assert_eq!(controls.sort.as_ref().unwrap().options.len(), 3);
    assert_eq!(
        controls.modes,
        vec![
            ControlOption {
                value: "any".into(),
                label: "Any of".into()
            },
            ControlOption {
                value: "none".into(),
                label: "None of".into()
            },
        ]
    );
    // Declaration order, not alphabetical, and each filter's control is
    // what its value source calls for.
    let inputs: Vec<_> = controls
        .filters
        .iter()
        .map(|f| (f.name.as_str(), f.input.as_str()))
        .collect();
    assert_eq!(
        inputs,
        vec![
            ("year_from", "integer"),
            ("year_to", "integer"),
            ("type", "options"),
            ("open_access", "toggle"),
            ("title", "text"),
            ("author", "suggest"),
            ("cites", "result"),
        ]
    );
    assert_eq!(controls.filters[0].group.as_deref(), Some("Year"));
    assert_eq!(controls.filters[2].options.len(), 2);
    assert_eq!(controls.filters[2].modes, vec!["any", "none"]);
    assert_eq!(controls.filters[6].from_result.as_deref(), Some("id"));
}

#[test]
fn browse_is_refused_when_it_cannot_say_where_its_filters_go() {
    let source = AUTHORING_FULL_EXAMPLE.replace("&filter={filters}", "");
    let error = Manifest::parse(&source).unwrap_err().to_string();
    assert!(
        error.contains("browse.filters.year_from is written nowhere"),
        "{error}"
    );
}

/// The path is the one place that says where each thing goes, so what it
/// says and what is declared have to agree.
#[test]
fn a_browse_path_agrees_with_what_is_declared() {
    for (source, from, to, expected) in [
        (
            AUTHORING_FULL_EXAMPLE,
            "&sort={sort}",
            "",
            "browse.sort is declared but browse.path has no {sort}",
        ),
        (
            AUTHORING_FULL_EXAMPLE,
            "&filter={filters}",
            "&filter=type:article,{filters}",
            "a parameter whose whole value is {filters}",
        ),
        (
            AUTHORING_FULL_EXAMPLE,
            "/works?search={query}&filter",
            "/works/{query}?filter",
            "'{query}' belongs in the query string",
        ),
        (
            AUTHORING_FULL_EXAMPLE,
            "&page={page}\"\nfilter_separator",
            "&page={page}&from={offset}\"\nfilter_separator",
            "names both {page} and {offset}",
        ),
        (
            AUTHORING_FULL_EXAMPLE,
            "&page={page}\"\nfilter_separator",
            "&page={page}&x={nope}\"\nfilter_separator",
            "'{nope}' is not available here",
        ),
        (
            AUTHORING_FULL_EXAMPLE,
            "title       = {",
            "sort        = {",
            "'sort' is a name browse.path already gives a meaning",
        ),
        (
            AUTHORING_PARAMETERS_EXAMPLE,
            "/search?q={query}&year=",
            "/search?year=",
            "browse.query_required needs {query}",
        ),
        (
            AUTHORING_PARAMETERS_EXAMPLE,
            "&order={sort}",
            "&order={sort}&f={filters}",
            "every filter is written by name",
        ),
        (
            AUTHORING_PARAMETERS_EXAMPLE,
            r#"any = { label = "Any of", join = "|" }"#,
            r#"any = { label = "Any of", repeat = true }"#,
            "takes only modes that join",
        ),
        (
            AUTHORING_REPEATED_KEYS_EXAMPLE,
            "/stories?title={query}&",
            "/stories?{query}&",
            "'{query}' cannot be a parameter by itself",
        ),
        (
            AUTHORING_REPEATED_KEYS_EXAMPLE,
            "&limit={limit}&offset={offset}\"\ntotal",
            "&{limit}&offset={offset}\"\ntotal",
            "'{limit}' cannot be a parameter by itself",
        ),
        (
            AUTHORING_REPEATED_KEYS_EXAMPLE,
            "&{filters}&",
            "&x{filters}&",
            "must be {filters} by itself or a parameter whose whole value is {filters}",
        ),
        (
            AUTHORING_REPEATED_KEYS_EXAMPLE,
            r#"clause = "status[]={value}""#,
            r#"clause = "{value}""#,
            "browse.filters.status.clause is a query parameter of its own, so {value} goes after its first '='",
        ),
        (
            AUTHORING_REPEATED_KEYS_EXAMPLE,
            r#"clause = "year={value}""#,
            r#"clause = "=y{value}""#,
            "browse.filters.year.clause is a query parameter of its own and must name it before '='",
        ),
        (
            AUTHORING_REPEATED_KEYS_EXAMPLE,
            r#"["order[year]=asc", "Oldest"]"#,
            r#"["=asc", "Oldest"]"#,
            "browse.sort option '=asc' is a query parameter of its own and must name it before '='",
        ),
    ] {
        assert!(source.contains(from), "{from}");
        let error = Manifest::parse(&source.replace(from, to))
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

#[test]
fn a_keyword_is_offered_when_the_path_names_it_and_needed_when_declared() {
    let controls = |source: &str| {
        Manifest::parse(source)
            .unwrap()
            .capabilities
            .browse
            .unwrap()
            .controls()
    };
    let optional = controls(AUTHORING_FULL_EXAMPLE);
    assert!(optional.keyword && !optional.keyword_required);
    let required = controls(AUTHORING_PARAMETERS_EXAMPLE);
    assert!(required.keyword && required.keyword_required);
    let none = controls(&AUTHORING_FULL_EXAMPLE.replace(
        "/works?search={query}&filter={filters}&sort={sort}",
        "/works?filter={filters}&sort={sort}",
    ));
    assert!(!none.keyword);
}

#[test]
fn a_browse_clause_carries_a_value_exactly_when_the_filter_has_one() {
    let source = AUTHORING_FULL_EXAMPLE.replace("type:{value}", "type:fixed");
    let error = Manifest::parse(&source).unwrap_err().to_string();
    assert!(error.contains("must contain {value}"), "{error}");
    let source = AUTHORING_FULL_EXAMPLE.replace("is_oa:true", "is_oa:{value}");
    let error = Manifest::parse(&source).unwrap_err().to_string();
    assert!(
        error.contains("declares none of options, value, suggest"),
        "{error}"
    );
}

/// The filter kinds are gone, and a manifest still written with them is
/// told so by name rather than read as something else.
#[test]
fn a_filter_written_with_a_kind_is_refused_by_name() {
    let source = AUTHORING_FULL_EXAMPLE.replace(
        r#"open_access = { label"#,
        r#"open_access = { kind = "flag", label"#,
    );
    let error = format!("{:#}", Manifest::parse(&source).unwrap_err());
    assert!(error.contains("kind"), "{error}");
}

#[test]
fn a_filter_says_where_its_value_comes_from_once() {
    for (from, to, expected) in [
        (
            r#"value = "text" }"#,
            r#"value = "text", options = [["a", "A"]] }"#,
            "more than one of options, value and suggest",
        ),
        (
            r#"from_result = "id" }"#,
            r#"from_result = "id", value = "text" }"#,
            "from_result stands alone or with suggest",
        ),
        (
            r#"from_result = "id" }"#,
            r#"from_result = "nope" }"#,
            "is not a search result field",
        ),
        (
            r#"suggest = "/suggest/authors?q={query}" }"#,
            r#"suggest = "/suggest/authors" }"#,
            "suggest must contain {query}",
        ),
        (
            r#"open_access = { label = "Open access only""#,
            r#"open_access = { modes = ["any"], label = "Open access only""#,
            "modes needs a filter that takes a value",
        ),
        (
            r#"modes = ["any", "none"], options"#,
            r#"modes = ["any", "every"], options"#,
            "names 'every', which browse.modes does not declare",
        ),
    ] {
        assert!(AUTHORING_FULL_EXAMPLE.contains(from), "{from}");
        let source = AUTHORING_FULL_EXAMPLE.replace(from, to);
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

#[test]
fn a_suggesting_filter_needs_the_response_described() {
    let start = AUTHORING_FULL_EXAMPLE
        .find("[capabilities.browse.suggest]")
        .unwrap();
    let end = AUTHORING_FULL_EXAMPLE
        .find("[capabilities.browse.modes]")
        .unwrap();
    let source = format!(
        "{}{}",
        &AUTHORING_FULL_EXAMPLE[..start],
        &AUTHORING_FULL_EXAMPLE[end..]
    );
    let error = Manifest::parse(&source).unwrap_err().to_string();
    assert!(error.contains("needs browse.suggest"), "{error}");
}

#[test]
fn a_mode_says_one_way_of_combining_values() {
    for (from, to, expected) in [
        (
            r#"any  = { label = "Any of", join = "|" }"#,
            r#"any  = { label = "Any of" }"#,
            "neither how its values are joined nor to repeat",
        ),
        (
            r#"any  = { label = "Any of", join = "|" }"#,
            r#"any  = { label = "Any of", join = "|", repeat = true }"#,
            "sets both join and repeat",
        ),
        (
            r#"value = "!{value}" }"#,
            r#"value = "!" }"#,
            "must contain {value}",
        ),
    ] {
        assert!(AUTHORING_FULL_EXAMPLE.contains(from), "{from}");
        let source = AUTHORING_FULL_EXAMPLE.replace(from, to);
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

/// What a free value may not carry is the service's own filter syntax,
/// read off its declaration.
#[test]
fn the_reserved_strings_are_the_declared_syntax() {
    let manifest = Manifest::parse(AUTHORING_FULL_EXAMPLE).unwrap();
    let reserved = manifest.capabilities.browse.unwrap().reserved("title");
    assert_eq!(reserved.within, vec![",", "|"]);
    assert_eq!(reserved.leading, vec!["!"]);
    // The separator is syntax only among the clauses it separates: a
    // filter written under its own parameter is not one of them.
    let manifest = Manifest::parse(AUTHORING_PARAMETERS_EXAMPLE).unwrap();
    let reserved = manifest.capabilities.browse.unwrap().reserved("language");
    assert_eq!(reserved.within, vec!["|"]);
}

#[test]
fn a_browse_default_sort_must_be_one_of_its_options() {
    let source = AUTHORING_FULL_EXAMPLE.replace(
        r#"default = "cited_by_count:desc""#,
        r#"default = "nonsense""#,
    );
    let error = Manifest::parse(&source).unwrap_err().to_string();
    assert!(error.contains("not one of its options"), "{error}");
}

#[test]
fn lookup_is_bounded_and_must_use_the_doi() {
    let too_many = AUTHORING_FULL_EXAMPLE.replace(
        r#"  "/works?filter=landing_page:https://doi.org/{doi}","#,
        r#"  "/b?d={doi}",
  "/c?d={doi}",
  "/d?d={doi}","#,
    );
    let error = Manifest::parse(&too_many).unwrap_err().to_string();
    assert!(error.contains("the limit is 3"), "{error}");

    let no_doi =
        AUTHORING_FULL_EXAMPLE.replace("/works?filter=doi:https://doi.org/{doi}\",", "/works\",");
    let error = Manifest::parse(&no_doi).unwrap_err().to_string();
    assert!(error.contains("must contain {doi}"), "{error}");
}

#[test]
fn reference_fields_are_named_from_the_reference_vocabulary() {
    let parsed = Manifest::parse(AUTHORING_FULL_EXAMPLE).unwrap();
    let fields = &parsed.capabilities.references.unwrap().resolve.fields;
    assert_eq!(
        fields.keys().map(String::as_str).collect::<Vec<_>>(),
        ["authors", "title", "venue", "year"]
    );

    let from = r#"fields = { title = "display_name","#;
    assert!(AUTHORING_FULL_EXAMPLE.contains(from));
    let unknown = AUTHORING_FULL_EXAMPLE.replace(
        from,
        r#"fields = { abstract = "x", title = "display_name","#,
    );
    let error = Manifest::parse(&unknown).unwrap_err().to_string();
    assert!(
        error.contains("references.resolve.fields.abstract is not a reference field"),
        "{error}"
    );

    let missing = AUTHORING_FULL_EXAMPLE.replace(
        from,
        r#"fields = { title = { path = "display_name", missing = "unavailable" },"#,
    );
    let error = Manifest::parse(&missing).unwrap_err().to_string();
    assert!(
        error.contains("references.resolve.fields.title.missing"),
        "{error}"
    );
}

#[test]
fn references_chunk_is_bounded() {
    let source = AUTHORING_FULL_EXAMPLE.replace("chunk = 50", "chunk = 500");
    let error = Manifest::parse(&source).unwrap_err().to_string();
    assert!(error.contains("outside 1-100"), "{error}");
    let source = AUTHORING_FULL_EXAMPLE.replace("chunk = 50", "chunk = 0");
    assert!(Manifest::parse(&source).is_err());
}

#[test]
fn a_resolver_url_is_one_template_or_a_short_list_of_them() {
    let with_url = |url: &str| {
        let from = r#"url = "{download_url}""#;
        assert!(AUTHORING_HTML_EXAMPLE.contains(from));
        Manifest::parse(&AUTHORING_HTML_EXAMPLE.replace(from, &format!("url = {url}")))
    };
    let listed = with_url(r#"["{pdf_url}", "{download_url}"]"#).unwrap();
    let url = listed.capabilities.resolve_download.unwrap().url.unwrap();
    assert!(url.chooses());
    assert_eq!(url.alternatives(), ["{pdf_url}", "{download_url}"]);

    for (url, expected) in [
        (r#"[]"#, "lists 0 templates"),
        (
            r#"["{download_url}"]"#,
            "one template is written as a string",
        ),
        (
            r#"["{id}", "{doi}", "{title}", "{pdf_url}", "{download_url}"]"#,
            "lists 5 templates",
        ),
        (
            r#"["{pdf_url}", "{nowhere}"]"#,
            "resolve_download.url: '{nowhere}' is not available here",
        ),
        (
            r#"["{pdf_url}", ""]"#,
            "resolve_download.url cannot be empty",
        ),
    ] {
        let error = with_url(url).unwrap_err().to_string();
        assert!(error.contains(expected), "{url}: {error}");
    }
}

#[test]
fn a_resolver_runs_a_program_it_names_literally_with_arguments_it_lists() {
    let run = |line: &str| {
        let from = r#"filename = "{title}.pdf"
run = { program = "archive-get", args = ["{id}", "--format", "pdf", "--out", "{output_dir}/{filename}"], env = { ARCHIVE_TOKEN = { secret = "archive_token", label = "Archive token" } } }"#;
        assert!(AUTHORING_RUN_EXAMPLE.contains(from));
        Manifest::parse(&AUTHORING_RUN_EXAMPLE.replace(from, line))
    };
    let manifest =
        run(r#"run = { program = "/opt/bin/fetch", args = ["--into={output_dir}/{id}"] }"#)
            .unwrap();
    assert_eq!(manifest.programs(), vec!["/opt/bin/fetch"]);
    // Secrets reach a program as whole arguments or as variables, and the
    // settings form asks for each.
    let keyed = run(r#"run = { program = "fetch", args = ["--key", { secret = "api_key", label = "API key" }], env = { TOKEN = { secret = "token", optional = true }, MODE = "batch" } }"#)
        .unwrap();
    let slots: Vec<(String, bool)> = keyed
        .secret_slots()
        .into_iter()
        .map(|slot| (slot.name, slot.optional))
        .collect();
    assert_eq!(
        slots,
        [("api_key".to_string(), false), ("token".to_string(), true)]
    );
    // A program that takes the name of what it writes is given one.
    run(
        r#"run = { program = "fetch", args = ["{id}", "-o", "{output_dir}/{filename}"] }
filename = "{title}.pdf""#,
    )
    .unwrap();

    for (line, expected) in [
        (
            r#"run = { program = "", args = [] }"#,
            "run.program cannot be empty",
        ),
        (
            r#"run = { program = "{id}", args = [] }"#,
            "must be a literal name, without placeholders",
        ),
        (
            r#"run = { program = "bin/fetch", args = [] }"#,
            "must be a command name or an absolute path",
        ),
        (
            r#"run = { program = " fetch", args = [] }"#,
            "surrounding space or a control character",
        ),
        (
            r#"run = { program = "fetch", args = ["{nowhere}"] }"#,
            "resolve_download.run.args[0]: '{nowhere}' is not available here",
        ),
        (
            r#"run = { program = "fetch", args = [] }
filename = "{title}.pdf""#,
            "no run argument names {filename}",
        ),
        (
            r#"run = { program = "fetch", args = ["--out", "{filename}"] }"#,
            "resolve_download.run.args[1]: '{filename}' is not available here",
        ),
        (
            r#"run = { program = "fetch", args = ["{output_dir}"] }
steps = [ { path = "/manga/{id}", fields = { output_dir = "data.id" } } ]"#,
            "a step field named 'output_dir' would be read as the run's own {output_dir}",
        ),
        (
            r#"run = { program = "fetch", args = [] }
url = "{pdf_url}""#,
            "sets both url and run",
        ),
        ("", "sets neither url nor run"),
        (
            r#"run = { program = "fetch", args = ["--key", { secret = "api_key", optional = true }] }"#,
            "args[1]: an argument cannot be left off when its secret is unset",
        ),
        (
            r#"run = { program = "fetch", args = [], env = { "1BAD" = "x" } }"#,
            "run.env.1BAD: a variable name is letters",
        ),
    ] {
        let error = run(line).unwrap_err().to_string();
        assert!(error.contains(expected), "{line}: {error}");
    }
    let too_many = format!(
        "run = {{ program = \"fetch\", args = [{}] }}",
        vec!["\"x\""; MAX_RUN_ARGS + 1].join(", ")
    );
    let error = run(&too_many).unwrap_err().to_string();
    assert!(
        error.contains(&format!("the limit is {MAX_RUN_ARGS}")),
        "{error}"
    );
}

#[test]
fn only_a_resolver_step_field_may_say_what_its_absence_means() {
    let step =
        r#"fields = { download_url = { path = "absolute_url(url)", missing = "unavailable" } }"#;
    let source =
        AUTHORING_HTML_EXAMPLE.replace(r#"fields = { download_url = "absolute_url(url)" }"#, step);
    assert!(source.contains(step), "the step was not rewritten");
    let manifest = Manifest::parse(&source).unwrap();
    let resolve = manifest.capabilities.resolve_download.unwrap();
    assert_eq!(
        resolve.steps[0].fields["download_url"].missing(),
        Missing::Unavailable
    );

    // A search or lookup field that matches nothing is already left empty.
    for (from, to, label) in [
        (
            "title = \"name\"\ndoi",
            "title = { path = \"name\", missing = \"unavailable\" }\ndoi",
            "search.fields.title.missing is only for resolve_download step fields",
        ),
        (
            "year = \"year\"",
            "year = { path = \"year\", missing = \"unavailable\" }",
            "lookup.fields.year.missing is only for resolve_download step fields",
        ),
    ] {
        assert!(AUTHORING_FULL_EXAMPLE.contains(from), "{from}");
        let error = Manifest::parse(&AUTHORING_FULL_EXAMPLE.replace(from, to))
            .unwrap_err()
            .to_string();
        assert!(error.contains(label), "{error}");
    }

    // There is no third thing an absence can mean.
    assert!(Manifest::parse(&source.replace("\"unavailable\"", "\"skip\"")).is_err());
}

/// What the prompt teaches is what gets written. It shows one line per
/// entry, and says so, because a model copies the layout it is shown.
#[test]
fn the_authoring_prompt_teaches_one_line_per_entry() {
    assert_eq!(layout_problems(&authoring_prompt()), Vec::<String>::new());
}

#[test]
fn a_header_per_entry_and_a_header_per_list_item_are_both_named() {
    let verbose = "[capabilities.browse.filters]\nyear = { kind = \"flag\" }\n\n\
                       [capabilities.browse.filters.year]\nkind = \"range\"\n\n\
                       [[capabilities.resolve_download.steps]]\npath = \"/a\"\n\
                       options = [\n  [\"a\", \"A\"],\n[\"b\", \"B\"],\n]\n";
    let problems = layout_problems(verbose);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems[0].contains("line 4"), "{problems:?}");
    assert!(problems[0].contains("capabilities.browse.filters.year"));
    assert!(problems[1].contains("[[capabilities.resolve_download.steps]]"));
}

#[test]
fn authoring_prompt_examples_are_manifests_the_parser_accepts() {
    Manifest::parse(AUTHORING_JSON_EXAMPLE).unwrap();
    Manifest::parse(AUTHORING_HTML_EXAMPLE).unwrap();
    Manifest::parse(AUTHORING_FULL_EXAMPLE).unwrap();
    Manifest::parse(AUTHORING_PARAMETERS_EXAMPLE).unwrap();
    Manifest::parse(AUTHORING_REPEATED_KEYS_EXAMPLE).unwrap();
    let run = Manifest::parse(AUTHORING_RUN_EXAMPLE).unwrap();
    assert_eq!(run.programs(), vec!["archive-get"]);
    let cli = Manifest::parse(AUTHORING_CLI_EXAMPLE).unwrap();
    assert!(cli.http.is_none());
    assert_eq!(cli.programs(), vec!["catalog"]);
    // The one that extends is read where there is something to extend.
    let resolved = Manifest::resolve(
        AUTHORING_EXTENDS_EXAMPLE,
        &crate::types::IntegrationsSettings::default(),
    )
    .unwrap();
    assert_eq!(
        resolved.manifest.capabilities.declared(),
        vec!["search", "browse", "health", "resolve_download"]
    );
}

#[test]
fn authoring_prompt_is_derived_from_the_complete_validation_vocabulary() {
    let prompt = authoring_prompt();
    for (field, field_type) in SEARCH_FIELDS {
        assert!(
            prompt.contains(&format!("- {field}: {}", field_type.name())),
            "prompt omitted search field {field}"
        );
    }
    for (field, field_type) in LOOKUP_FIELDS {
        assert!(
            prompt.contains(&format!("- {field}: {}", field_type.name())),
            "prompt omitted lookup field {field}"
        );
    }
    for (name, _) in expr::registered_function_hints() {
        assert!(prompt.contains(name), "prompt omitted function {name}");
    }
    for format in ResponseFormat::ALL {
        assert!(prompt.contains(format.name()));
    }
    for kind in SecretKind::ALL {
        assert!(prompt.contains(kind.name()));
    }
    for missing in Missing::ALL {
        assert!(prompt.contains(missing.name()));
    }
    for required in REQUIRED_SEARCH_FIELDS {
        let field_type = search_field_type(required).unwrap();
        assert!(prompt.contains(&format!("- {required}: {} (required)", field_type.name())));
    }
    assert!(prompt.contains(AUTHORING_JSON_EXAMPLE));
    assert!(prompt.contains(AUTHORING_HTML_EXAMPLE));
    assert!(prompt.contains(AUTHORING_FULL_EXAMPLE));
    assert!(prompt.contains(AUTHORING_EXTENDS_EXAMPLE));
    assert!(prompt.contains(AUTHORING_RUN_EXAMPLE));
    assert!(prompt.contains(AUTHORING_CLI_EXAMPLE));
    assert!(prompt.contains(&format!("at most {MAX_RUN_ARGS} arguments")));
    assert!(prompt.contains("base_url = \"https://other-origin.test\""));
    assert!(prompt.contains("never copied to a cross-origin step"));
    assert!(prompt.contains(&format!("limited to {MAX_RESOLVER_STEPS}")));
    assert!(prompt.ends_with("before sending the prompt to the model.\n"));
}
