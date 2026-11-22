use super::*;

#[tokio::test]
async fn a_download_through_the_browser_is_stored_like_any_other_and_its_staging_goes() {
    let root = tempdir().unwrap();
    let staging = tempdir().unwrap();
    let mcp = mcp_with_browser(root.path(), staging.path());

    let reply = mcp
        .download(Parameters(browser_download("https://dl.example/paper.pdf")))
        .await;
    assert_ne!(reply.is_error, Some(true), "{reply:?}");
    let saved = root.path().join("Signed In.pdf");
    assert_eq!(std::fs::read(&saved).unwrap(), b"%PDF-1.7 signed in");
    assert_eq!(
        reply.structured_content.unwrap()["downloads"][0]["files"][0]["path"],
        saved.display().to_string()
    );
    assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);

    let refused = mcp
        .download(Parameters(browser_download("https://dl.example/login")))
        .await;
    assert_eq!(refused.is_error, Some(true));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn through_browser_is_read_however_a_client_spells_the_boolean() {
    for value in [serde_json::json!(true), serde_json::json!("true")] {
        let params: DownloadParams = serde_json::from_value(serde_json::json!({
            "downloads": [{
                "url": "https://dl.example/paper.pdf",
                "through_browser": value,
            }],
        }))
        .unwrap();
        assert_eq!(params.downloads[0].through_browser, Some(true));
    }
}

/// A site that refuses this client gets one more try in the agent
/// browser; anything else it answers is the answer.
#[tokio::test]
async fn a_download_refused_access_is_retried_once_through_the_browser() {
    let mut server = mockito::Server::new_async().await;
    let refused = server
        .mock("GET", "/paper.pdf")
        .with_status(403)
        .expect(1)
        .create_async()
        .await;
    let _gone = server
        .mock("GET", "/gone/paper.pdf")
        .with_status(404)
        .create_async()
        .await;
    let _login = server
        .mock("GET", "/login")
        .with_status(401)
        .create_async()
        .await;
    let root = tempdir().unwrap();
    let staging = tempdir().unwrap();
    let mcp = mcp_with_browser(root.path(), staging.path());

    let reply = mcp
        .download(Parameters(direct_download(format!(
            "{}/paper.pdf",
            server.url()
        ))))
        .await;
    assert_ne!(reply.is_error, Some(true), "{reply:?}");
    refused.assert_async().await;
    assert_eq!(
        std::fs::read(root.path().join("Signed In.pdf")).unwrap(),
        b"%PDF-1.7 signed in"
    );

    // Not a refusal of access: no second try.
    let reply = mcp
        .download(Parameters(direct_download(format!(
            "{}/gone/paper.pdf",
            server.url()
        ))))
        .await;
    let text = format!("{reply:?}");
    assert!(text.contains("404"), "{text}");
    assert!(!text.contains("agent browser"), "{text}");

    // The browser tried and failed too: both are reported.
    let reply = mcp
        .download(Parameters(direct_download(format!(
            "{}/login",
            server.url()
        ))))
        .await;
    let text = format!("{reply:?}");
    assert!(text.contains("401"), "{text}");
    assert!(text.contains("Retried through the agent browser"), "{text}");
    assert!(text.contains("sign in"), "{text}");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);

    // Without a browser, a refusal is just a refusal.
    let plain = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(fake_search_with_root(root.path().into()))),
    );
    let reply = plain
        .download(Parameters(direct_download(format!(
            "{}/login",
            server.url()
        ))))
        .await;
    let text = format!("{reply:?}");
    assert!(text.contains("401"), "{text}");
    assert!(!text.contains("agent browser"), "{text}");
}

/// Several files in one call are fetched together: each server answer
/// waits until every request has arrived, so fetching them one after
/// another would never finish.
#[tokio::test]
async fn the_downloads_of_one_call_are_fetched_at_the_same_time_and_answered_in_order() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let files = 3;
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        let mut waiting = Vec::new();
        for _ in 0..files {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let read = stream.read(&mut request).unwrap();
            let line = String::from_utf8_lossy(&request[..read]).to_string();
            waiting.push((stream, line));
        }
        for (mut stream, line) in waiting {
            if line.starts_with("GET /missing.pdf") {
                stream
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    )
                    .unwrap();
                continue;
            }
            let name = line.split_whitespace().nth(1).unwrap().to_string();
            let body = format!("%PDF-1.7 {name}");
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/pdf\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .unwrap();
        }
    });
    let root = tempdir().unwrap();
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(fake_search_with_root(root.path().into()))),
    );
    let url = |path: &str| format!("http://{address}/{path}");

    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        mcp.download(Parameters(batch(vec![
            direct_item(url("a.pdf")),
            direct_item(url("missing.pdf")),
            direct_item(url("b.pdf")),
        ]))),
    )
    .await
    .expect("downloads fetched one at a time never get their answers");

    // One failing leaves the others in the library, and says why.
    assert_ne!(reply.is_error, Some(true), "{reply:?}");
    let downloads = reply.structured_content.unwrap()["downloads"].clone();
    assert_eq!(downloads[0]["url"], url("a.pdf"));
    assert_eq!(
        downloads[0]["files"][0]["path"],
        root.path().join("a.pdf").display().to_string()
    );
    assert_eq!(downloads[1]["url"], url("missing.pdf"));
    assert!(downloads[1]["error"].as_str().unwrap().contains("404"));
    assert!(downloads[1].get("files").is_none());
    assert_eq!(downloads[2]["url"], url("b.pdf"));
    assert_eq!(
        std::fs::read(root.path().join("b.pdf")).unwrap(),
        b"%PDF-1.7 /b.pdf"
    );
}

#[tokio::test]
async fn a_call_whose_every_download_failed_is_an_error_naming_each() {
    let root = tempdir().unwrap();
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(fake_search_with_root(root.path().into()))),
    );
    let reply = mcp
        .download(Parameters(batch(vec![
            direct_item("ftp://dl.example/a.pdf".to_string()),
            browser_item("https://dl.example/b.pdf"),
        ])))
        .await;
    assert_eq!(reply.is_error, Some(true));
    let text = format!("{reply:?}");
    assert!(
        text.contains("ftp://dl.example/a.pdf: Download URL must use HTTP"),
        "{text}"
    );
    assert!(text.contains("https://dl.example/b.pdf: "), "{text}");
    assert!(text.contains(BROWSER_UNAVAILABLE), "{text}");

    for size in [0, MAX_DOWNLOADS_PER_CALL + 1] {
        let downloads = (0..size)
            .map(|n| direct_item(format!("https://dl.example/{n}.pdf")))
            .collect();
        let reply = mcp.download(Parameters(batch(downloads))).await;
        assert_eq!(reply.is_error, Some(true));
        assert!(
            format!("{reply:?}").contains("downloads takes 1 to 20"),
            "{reply:?}"
        );
    }
}

#[tokio::test]
async fn browser_tools_say_so_where_there_is_no_browser() {
    let root = tempdir().unwrap();
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(fake_search_with_root(root.path().into()))),
    );
    let reply = mcp
        .download(Parameters(browser_download("https://dl.example/paper.pdf")))
        .await;
    assert!(
        format!("{reply:?}").contains(BROWSER_UNAVAILABLE),
        "{reply:?}"
    );
    let reply = mcp
        .read_web_page(Parameters(ReadWebPageParams {
            url: "https://dl.example/".to_string(),
            max_chars: None,
        }))
        .await;
    assert!(
        format!("{reply:?}").contains(BROWSER_UNAVAILABLE),
        "{reply:?}"
    );

    let mut by_result = browser_item("https://dl.example/paper.pdf");
    by_result.url = None;
    by_result.provider = Some("openalex".to_string());
    by_result.result_id = Some("W1".to_string());
    let reply = mcp.download(Parameters(batch(vec![by_result]))).await;
    assert!(
        format!("{reply:?}").contains("through_browser takes a url"),
        "{reply:?}"
    );
}

#[tokio::test]
async fn a_page_read_in_the_browser_is_cut_by_characters() {
    let root = tempdir().unwrap();
    let staging = tempdir().unwrap();
    let mcp = mcp_with_browser(root.path(), staging.path());
    let reply = mcp
        .read_web_page(Parameters(ReadWebPageParams {
            url: "https://dl.example/article".to_string(),
            max_chars: Some(4),
        }))
        .await;
    let content = reply.structured_content.unwrap();
    assert_eq!(content["text"], "éééé");
    assert_eq!(content["truncated"], true);
    assert_eq!(content["title"], "Full text");
}

#[cfg(unix)]
#[tokio::test]
async fn downloading_a_result_its_manifest_runs_a_program_for_puts_the_file_in_the_root() {
    let root = tempdir().unwrap();
    let mut integrations = IntegrationsSettings::default();
    integrations.allowed_programs.push("/bin/sh".to_string());
    integrations
        .custom
        .push(wilkes_core::types::CustomIntegrationConfig {
            id: "runner".to_string(),
            name: None,
            enabled: true,
            manifest: r#"manifest_version = 2
id = "runner"
name = "Runner"

[http]
base_url = "https://runner.test"

[capabilities.search]
path = "/search?q={query}"

[capabilities.search.fields]
id = "id"
title = "title"

[capabilities.resolve_download]
run = { program = "/bin/sh", args = ["-c", "printf manga > \"$1/{id}.pdf\"", "sh", "{output_dir}"] }
"#
            .to_string(),
            secrets: Default::default(),
        });
    let service = fake_search_with_root(root.path().into());
    *service.integrations.lock().unwrap() = integrations;
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(service)),
    );
    mcp.literature_results.lock().unwrap().insert(
        ("custom:runner".to_string(), "M1".to_string()),
        LiteratureSearchResult {
            id: "M1".to_string(),
            doi: None,
            title: Some("A manga".to_string()),
            year: None,
            publication_date: None,
            venue: None,
            citation_count: None,
            is_open_access: false,
            pdf_url: None,
            landing_page_url: None,
            image_url: None,
            open_access_status: None,
            license: None,
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
            acquisition: LiteratureAcquisition::Provider,
        },
    );

    let reply = mcp
        .download(Parameters(batch(vec![DownloadItem {
            url: None,
            provider: Some("custom:runner".to_string()),
            result_id: Some("M1".to_string()),
            filename: None,
            through_browser: None,
        }])))
        .await;

    assert_ne!(reply.is_error, Some(true), "{reply:?}");
    let saved = root.path().join("M1.pdf");
    assert_eq!(std::fs::read(&saved).unwrap(), b"manga");
    assert_eq!(
        reply.structured_content.unwrap()["downloads"][0]["files"][0]["path"],
        saved.display().to_string()
    );
    // The run's own directory is not in the library.
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}
