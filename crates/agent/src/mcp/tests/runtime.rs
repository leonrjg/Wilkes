use super::*;

#[tokio::test]
async fn external_context_reads_only_configured_library_roots() {
    let library = tempdir().unwrap();
    let inside = library.path().join("inside.txt");
    std::fs::write(&inside, "inside library").unwrap();
    let outside_dir = tempdir().unwrap();
    let outside = outside_dir.path().join("outside.txt");
    std::fs::write(&outside, "outside library").unwrap();
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(fake_search_with_root(
            library.path().to_path_buf(),
        ))),
    );

    let response = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: Some(inside.to_string_lossy().into_owned()),
            page: None,
            page_range: None,
            max_chars: None,
            workspace: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(response.text, "inside library");

    let error = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: Some(outside.to_string_lossy().into_owned()),
            page: None,
            page_range: None,
            max_chars: None,
            workspace: None,
        },
    )
    .await
    .unwrap_err();
    assert!(error.contains("not in a configured Wilkes library root"));
}

#[tokio::test]
async fn external_context_requires_explicit_document_path() {
    let library = tempdir().unwrap();
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(fake_search_with_root(
            library.path().to_path_buf(),
        ))),
    );

    let error = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: None,
            page: None,
            page_range: None,
            max_chars: None,
            workspace: None,
        },
    )
    .await
    .unwrap_err();
    assert!(error.contains("pass path explicitly"));
}

#[tokio::test]
async fn external_search_rejects_root_outside_library() {
    let library = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(fake_search_with_root(
            library.path().to_path_buf(),
        ))),
    );

    let error = search_documents_for_mcp(
        &mcp,
        SearchParams {
            query: "needle".to_string(),
            mode: SearchModeParam::Exact,
            scope: None,
            root: Some(outside.path().to_string_lossy().into_owned()),
            file: None,
            max_results: None,
            case_sensitive: None,
            is_regex: None,
            context_lines: None,
            collection_id: None,
            workspace: None,
        },
    )
    .await
    .unwrap_err();
    assert!(error.contains("not in a configured Wilkes library root"));
}

#[tokio::test]
async fn bearer_middleware_requires_current_token() {
    use axum::{body::Body, http::Request, routing::get};
    use tower::ServiceExt;

    let token = Arc::new(RwLock::new(Some("first-token".to_string())));
    let app = Router::new()
        .route("/", get(|| async { StatusCode::OK }))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&token),
            require_bearer_token,
        ));

    let unauthorized = app
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let authorized = app
        .clone()
        .oneshot(
            Request::get("/")
                .header("Authorization", "Bearer first-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::OK);

    *token.write().unwrap() = Some("second-token".to_string());
    let old = app
        .clone()
        .oneshot(
            Request::get("/")
                .header("Authorization", "Bearer first-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(old.status(), StatusCode::UNAUTHORIZED);
    let current = app
        .clone()
        .oneshot(
            Request::get("/")
                .header("Authorization", "Bearer second-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(current.status(), StatusCode::OK);

    // The browser's form of the same credential.
    let cookie = app
        .clone()
        .oneshot(
            Request::get("/")
                .header("Cookie", format!("theme=dark; {TOKEN_COOKIE}=second-token"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cookie.status(), StatusCode::OK);
    let stale_cookie = app
        .oneshot(
            Request::get("/")
                .header("Cookie", format!("{TOKEN_COOKIE}=first-token"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stale_cookie.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn bearer_middleware_allows_requests_without_configured_token() {
    use axum::{body::Body, http::Request, routing::get};
    use tower::ServiceExt;

    let app = Router::new()
        .route("/", get(|| async { StatusCode::OK }))
        .layer(middleware::from_fn_with_state(
            Arc::new(RwLock::new(None)),
            require_bearer_token,
        ));
    let response = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn tokenless_external_server_initializes_and_lists_tools() {
    let library = tempdir().unwrap();
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let external_host = format!("192.168.1.6:{port}");
    let runtime = start_external(
        "127.0.0.1".parse().unwrap(),
        port,
        None,
        single_workspace(fake_search_with_root(library.path().to_path_buf())),
        ExternalMcpContext::default(),
        None,
    )
    .await
    .unwrap();
    let client = reqwest::Client::new();

    // A raw GET is not a valid request for this stateless MCP transport,
    // but an external Host must reach the transport instead of being
    // rejected by rmcp's loopback-only DNS-rebinding default.
    let get = client
        .get(runtime.url())
        .header("Host", &external_host)
        .send()
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::METHOD_NOT_ALLOWED);

    let send = |body: serde_json::Value| {
        client
            .post(runtime.url())
            .header("Host", &external_host)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&body)
            .send()
    };

    let initialize = send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "wilkes-test", "version": "1.0" }
        }
    }))
    .await
    .unwrap();
    assert!(initialize.status().is_success());
    let initialize: serde_json::Value = initialize.json().await.unwrap();
    assert_eq!(initialize["result"]["serverInfo"]["name"], "wilkes");

    let tools = send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }))
    .await
    .unwrap();
    assert!(tools.status().is_success());
    let tools: serde_json::Value = tools.json().await.unwrap();
    let names: Vec<_> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"search"));
    assert!(names.contains(&"get_document_text"));
    assert!(names.contains(&"get_document_outline"));
    assert!(names.contains(&"download"));
    let search = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "search")
        .unwrap();
    assert_eq!(
        search["inputSchema"]["properties"]["max_results"]["type"],
        "integer"
    );
    assert_eq!(
        search["inputSchema"]["properties"]["case_sensitive"]["type"],
        "boolean"
    );
}

#[tokio::test]
async fn authenticated_external_server_accepts_external_host_only_with_bearer_token() {
    let library = tempdir().unwrap();
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let external_host = format!("192.168.1.6:{port}");
    let runtime = start_external(
        "127.0.0.1".parse().unwrap(),
        port,
        Some("test-token".to_string()),
        single_workspace(fake_search_with_root(library.path().to_path_buf())),
        ExternalMcpContext::default(),
        None,
    )
    .await
    .unwrap();
    let client = reqwest::Client::new();
    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "wilkes-test", "version": "1.0" }
        }
    });
    let request = || {
        client
            .post(runtime.url())
            .header("Host", &external_host)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&initialize)
    };

    let unauthorized = request().send().await.unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let authorized = request()
        .header("Authorization", "Bearer test-token")
        .send()
        .await
        .unwrap();
    assert!(authorized.status().is_success());
}

#[tokio::test]
async fn external_document_outline_requires_an_explicit_path() {
    let library = tempdir().unwrap();
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(fake_search_with_root(
            library.path().to_path_buf(),
        ))),
    );

    let error = get_document_outline_for_mcp(
        &mcp,
        GetDocumentOutlineParams {
            path: None,
            workspace: None,
        },
    )
    .await
    .unwrap_err();

    assert!(error.contains("pass path explicitly"));
}

#[tokio::test]
async fn external_context_reports_active_document_without_defaulting_get_tools() {
    let library = tempdir().unwrap();
    let active = library.path().join("active.pdf");
    std::fs::write(&active, b"active document").unwrap();
    let context = ExternalMcpContext::default();
    context.set_active_document(Some(active.to_string_lossy().into_owned()), Some(4));
    let mcp = WilkesMcp::new(
        McpContext::Library(context),
        PathBuf::new(),
        Some(single_workspace(fake_search_with_root(
            library.path().to_path_buf(),
        ))),
    );

    let scope = mcp.scope(None).await.unwrap();
    let snapshot = mcp.context_snapshot(&scope).await;
    let active_doc = snapshot.active_doc.expect("active document");
    assert_eq!(active_doc.path, active.to_string_lossy());
    assert_eq!(active_doc.page, Some(4));

    let text_error = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: None,
            page: None,
            page_range: None,
            max_chars: None,
            workspace: None,
        },
    )
    .await
    .unwrap_err();
    assert!(text_error.contains("pass path explicitly"));

    let related_error = get_related_documents_for_mcp(
        &mcp,
        GetRelatedDocumentsParams {
            path: None,
            scope: None,
            root: None,
            limit: None,
            collection_id: None,
            workspace: None,
        },
    )
    .await
    .unwrap_err();
    assert!(related_error.contains("pass path explicitly"));

    let metadata_error = get_file_metadata_for_mcp(
        &mcp,
        GetFileMetadataParams {
            path: None,
            workspace: None,
        },
    )
    .await
    .unwrap_err();
    assert!(metadata_error.contains("pass path explicitly"));
}

#[tokio::test]
async fn shared_api_mcp_initializes_and_rejects_browser_origins() {
    use axum::body::{to_bytes, Body};
    use tower::ServiceExt;
    let root = tempdir().unwrap();
    let router = api_router(single_workspace(fake_search_with_root(root.path().into())));
    let request = |origin: bool| {
        let mut builder = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("host", "127.0.0.1")
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json");
        if origin {
            builder = builder.header("origin", "https://example.com");
        }
        builder.body(Body::from(serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"eos","version":"test"}}}).to_string())).unwrap()
    };
    assert_eq!(
        router
            .clone()
            .oneshot(request(true))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let reply = router.oneshot(request(false)).await.unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    let json: serde_json::Value =
        serde_json::from_slice(&to_bytes(reply.into_body(), 100000).await.unwrap()).unwrap();
    assert_eq!(json["result"]["serverInfo"]["name"], "wilkes");
}
