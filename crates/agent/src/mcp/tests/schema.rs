use super::*;

#[test]
fn structured_response_uses_yaml_text_and_preserves_structured_content() {
    let response = structured(serde_json::json!({
        "document": {
            "title": "Readable MCP responses",
            "pages": 12
        },
        "tags": ["mcp", "yaml"]
    }));
    let serialized = serde_json::to_value(response).unwrap();

    assert_eq!(
        serialized["structuredContent"]["document"]["title"],
        "Readable MCP responses"
    );
    assert_eq!(serialized["structuredContent"]["tags"][1], "yaml");

    let text = serialized["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("document:\n"), "unexpected YAML: {text}");
    assert!(text.contains("  title: Readable MCP responses\n"));
    assert!(text.contains("tags:\n- mcp\n- yaml\n"));
    assert!(!text.trim_start().starts_with('{'));
}

#[test]
fn search_tool_schema_keeps_optional_parameters_typed() {
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        None,
    );
    let search = &mcp.tool_router.map.get("search").unwrap().attr.input_schema;
    let properties = search
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .unwrap();

    assert_eq!(properties["max_results"]["type"], "integer");
    assert_eq!(properties["context_lines"]["type"], "integer");
    assert_eq!(properties["case_sensitive"]["type"], "boolean");
    assert_eq!(properties["is_regex"]["type"], "boolean");
    assert_eq!(properties["root"]["type"], "string");
    let scope_ref = properties["scope"]["$ref"].as_str().unwrap();
    let search_value = serde_json::Value::Object(search.as_ref().clone());
    let scope_schema = search_value
        .pointer(scope_ref.strip_prefix('#').unwrap())
        .unwrap();
    assert_eq!(scope_schema["type"], "string");
    assert_eq!(scope_schema["enum"], serde_json::json!(["all"]));

    // Only the query is required. `mode` defaults to combined, so a caller
    // that has no reason to prefer wording over meaning does not have to
    // decide between them to search at all.
    let required = search
        .get("required")
        .and_then(serde_json::Value::as_array)
        .unwrap();
    assert_eq!(
        required
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>(),
        vec!["query"]
    );
    let mode_ref = properties["mode"]["$ref"].as_str().unwrap();
    let mode_schema = search_value
        .pointer(mode_ref.strip_prefix('#').unwrap())
        .unwrap();
    assert_eq!(
        mode_schema["enum"],
        serde_json::json!(["combined", "exact", "semantic"])
    );
}

#[test]
fn document_text_schema_describes_page_range_format() {
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        None,
    );
    let schema = &mcp
        .tool_router
        .map
        .get("get_document_text")
        .unwrap()
        .attr
        .input_schema;
    let properties = schema["properties"].as_object().unwrap();

    assert!(properties["page"]["description"]
        .as_str()
        .unwrap()
        .contains("1-based PDF page"));
    assert!(properties["page_range"]["description"]
        .as_str()
        .unwrap()
        .contains("\"N-M\""));
    assert_eq!(properties["page_range"]["type"], "string");
    assert!(properties["max_chars"]["description"]
        .as_str()
        .unwrap()
        .contains("default 24000"));
}

#[test]
fn all_tool_schemas_omit_nullable_unions() {
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        None,
    );
    for route in mcp.tool_router.map.values() {
        assert_schema_has_no_null_union(
            &serde_json::Value::Object(route.attr.input_schema.as_ref().clone()),
            route.name(),
        );
    }
}

#[test]
fn search_params_coerce_stringified_scalars() {
    let params: SearchParams = serde_json::from_value(serde_json::json!({
        "query": "test",
        "mode": "semantic",
        "scope": "all",
        "max_results": "5",
        "case_sensitive": "false",
        "is_regex": "true",
        "context_lines": "2"
    }))
    .unwrap();
    assert_eq!(params.max_results, Some(5));
    assert_eq!(params.case_sensitive, Some(false));
    assert_eq!(params.is_regex, Some(true));
    assert_eq!(params.context_lines, Some(2));
}

#[test]
fn document_text_params_accept_page_range_string() {
    let params: GetDocumentTextParams = serde_json::from_value(serde_json::json!({
        "path": "paper.pdf",
        "page_range": "7-10"
    }))
    .unwrap();
    assert_eq!(params.page_range.as_deref(), Some("7-10"));

    assert!(
        serde_json::from_value::<GetDocumentTextParams>(serde_json::json!({
            "path": "paper.pdf",
            "page_range": { "start": 7, "end": 10 }
        }))
        .is_err()
    );
}

#[test]
fn parses_page_range_string() {
    assert_eq!(parse_page_range("7-10").unwrap(), (7, 10));
    assert_eq!(parse_page_range(" 10 - 7 ").unwrap(), (7, 10));
}

#[test]
fn rejects_invalid_page_range_strings() {
    for value in ["", "7", "0-2", "1-0", "1-2-3", "one-two"] {
        let error = parse_page_range(value).unwrap_err();
        assert!(error.contains("Use \"N-M\""), "unexpected error: {error}");
    }
}

#[test]
fn mcp_scalar_coercion_rejects_invalid_strings() {
    assert!(serde_json::from_value::<SearchParams>(serde_json::json!({
        "query": "test",
        "mode": "exact",
        "scope": "all",
        "max_results": "5.0"
    }))
    .is_err());
    assert!(serde_json::from_value::<SearchParams>(serde_json::json!({
        "query": "test",
        "mode": "exact",
        "scope": "all",
        "case_sensitive": "yes"
    }))
    .is_err());
}

/// A client runs calls side by side only when a tool says it changes
/// nothing, and the in-app chat lets exactly the tools on
/// `WILKES_MCP_TOOL_NAMES` run without asking: the two must agree.
#[test]
fn the_tools_that_say_they_change_nothing_are_the_read_only_ones() {
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        None,
    );
    let mut read_only: Vec<String> = mcp
        .tool_router
        .list_all()
        .into_iter()
        .filter(|tool| {
            tool.annotations
                .as_ref()
                .and_then(|annotations| annotations.read_only_hint)
                == Some(true)
        })
        .map(|tool| tool.name.to_string())
        .collect();
    read_only.sort();
    let mut expected: Vec<String> = WILKES_MCP_TOOL_NAMES
        .iter()
        .map(|name| name.to_string())
        .collect();
    expected.sort();
    assert_eq!(read_only, expected);
}
