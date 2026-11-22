fn counted(count: Option<i64>) -> ProviderWork {
    ProviderWork {
        doi: "10.1/x".into(),
        work_id: "w".into(),
        title: None,
        year: None,
        publication_date: None,
        venue: None,
        citation_count: count,
        external_ids: Default::default(),
        cached_at_ms: 0,
    }
}

/// The count comes from the first provider in precedence *that reported
/// one*. One that did not is skipped, not read as zero, and none reporting
/// means no count at all.
#[test]
fn the_citation_count_is_the_first_reported_not_the_first_recorded() {
    let metadata = DocumentMetadata {
        providers: [
            ("semantic_scholar".to_string(), counted(None)),
            ("openalex".to_string(), counted(Some(12))),
        ]
        .into(),
        ..DocumentMetadata::default()
    };
    assert_eq!(metadata.citation_count(), Some(12));

    let none = DocumentMetadata {
        providers: [("semantic_scholar".to_string(), counted(None))].into(),
        ..DocumentMetadata::default()
    };
    assert_eq!(none.citation_count(), None);
}

use super::*;
use std::path::Path;

#[test]
fn bookmark_cluster_query_defaults_to_balanced_granularity() {
    let query: BookmarkClustersQuery =
        serde_json::from_str(r#"{"bookmark_ids":["bookmark-1"]}"#).unwrap();
    assert_eq!(query.granularity, BookmarkClusterGranularity::Balanced);
    assert_eq!(
        serde_json::to_value(BookmarkClusterGranularity::MuchMore).unwrap(),
        serde_json::json!("much_more")
    );
}

#[test]
fn chunk_topic_query_defaults_to_minimal_granularity() {
    let query: ChunkTopicsQuery = serde_json::from_str(r#"{"root":"/library"}"#).unwrap();
    assert_eq!(query.granularity, BookmarkClusterGranularity::MuchFewer);
    assert!(query.path.is_none());
    assert_eq!(
        SemanticSettings::default().topic_cloud_input_cap,
        SemanticSettings::default_topic_cloud_input_cap()
    );
}

#[test]
fn test_file_type_detect() {
    let extensions = vec!["txt".to_string()];

    assert_eq!(
        FileType::detect(Path::new("test.txt"), &extensions),
        Some(FileType::PlainText)
    );
    assert_eq!(
        FileType::detect(Path::new("test.pdf"), &extensions),
        Some(FileType::Paged)
    );
    assert_eq!(
        FileType::detect(Path::new("Makefile"), &extensions),
        Some(FileType::PlainText)
    );
    assert_eq!(FileType::detect(Path::new("test.exe"), &extensions), None);
}

#[test]
fn test_bounding_box_merge() {
    let b1 = BoundingBox {
        x: 0.0,
        y: 0.0,
        width: 10.0,
        height: 10.0,
    };
    let b2 = BoundingBox {
        x: 5.0,
        y: 5.0,
        width: 10.0,
        height: 10.0,
    };
    let merged = b1.merge(&b2);

    assert_eq!(merged.x, 0.0);
    assert_eq!(merged.y, 0.0);
    assert_eq!(merged.width, 15.0);
    assert_eq!(merged.height, 15.0);
}

#[test]
fn test_source_map_resolve() {
    let map = SourceMap {
        segments: vec![
            SourceSegment {
                text_range: ByteRange { start: 0, end: 10 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
                provenance: Default::default(),
            },
            SourceSegment {
                text_range: ByteRange { start: 10, end: 20 },
                origin: SourceOrigin::TextFile { line: 2, col: 1 },
                provenance: Default::default(),
            },
        ],
    };

    match map.resolve(5).unwrap() {
        SourceOrigin::TextFile { line, .. } => assert_eq!(line, 1),
        _ => panic!("Expected TextFile origin"),
    }

    match map.resolve(15).unwrap() {
        SourceOrigin::TextFile { line, .. } => assert_eq!(line, 2),
        _ => panic!("Expected TextFile origin"),
    }
}

#[test]
fn test_source_map_resolve_range_pdf() {
    let map = SourceMap {
        segments: vec![
            SourceSegment {
                text_range: ByteRange { start: 0, end: 10 },
                origin: SourceOrigin::PdfPage {
                    page: 1,
                    bbox: Some(BoundingBox {
                        x: 0.0,
                        y: 0.0,
                        width: 10.0,
                        height: 10.0,
                    }),
                },
                provenance: Default::default(),
            },
            SourceSegment {
                text_range: ByteRange { start: 10, end: 20 },
                origin: SourceOrigin::PdfPage {
                    page: 1,
                    bbox: Some(BoundingBox {
                        x: 5.0,
                        y: 5.0,
                        width: 10.0,
                        height: 10.0,
                    }),
                },
                provenance: Default::default(),
            },
        ],
    };

    let origin = map.resolve_range(ByteRange { start: 5, end: 15 }).unwrap();
    match origin {
        SourceOrigin::PdfPage { page, bbox } => {
            assert_eq!(page, 1);
            let b = bbox.unwrap();
            assert_eq!(b.x, 0.0);
            assert_eq!(b.y, 0.0);
            assert_eq!(b.width, 15.0);
            assert_eq!(b.height, 15.0);
        }
        _ => panic!("Expected PdfPage origin"),
    }
}

#[test]
fn test_embedding_engine_methods() {
    assert_eq!(EmbeddingEngine::SBERT.as_str(), "sbert");
    assert_eq!(EmbeddingEngine::Candle.as_str(), "candle");
    assert_eq!(EmbeddingEngine::Fastembed.as_str(), "fastembed");

    assert_eq!(EmbeddingEngine::SBERT.default_device(), "auto");
    assert_eq!(EmbeddingEngine::Candle.default_device(), "auto");
    assert_eq!(EmbeddingEngine::Fastembed.default_device(), "cpu");

    assert!(EmbeddingEngine::SBERT.supports_custom_models());
    assert!(EmbeddingEngine::Candle.supports_custom_models());
    assert!(!EmbeddingEngine::Fastembed.supports_custom_models());
}

#[test]
fn test_semantic_settings_defaults() {
    let settings = SemanticSettings::default();
    assert!(!settings.enabled);
    assert_eq!(settings.selected.engine, EmbeddingEngine::default());
    assert_eq!(settings.selected.model.model_id(), "AllMiniLML6V2");
    assert_eq!(settings.selected.dimension, 384);
    assert_eq!(settings.chunk_size, 600);
    assert_eq!(settings.chunk_overlap, 128);
    assert_eq!(settings.worker_timeout_secs, 300);

    assert_eq!(settings.device_for(EmbeddingEngine::SBERT), "auto");

    let mut settings = SemanticSettings::default();
    settings
        .engine_devices
        .insert(EmbeddingEngine::SBERT, "cuda".to_string());
    assert_eq!(settings.device_for(EmbeddingEngine::SBERT), "cuda");
}

#[test]
fn test_generation_settings_defaults() {
    let settings = GenerationSettings::default();
    assert!(!settings.enabled);
    assert_eq!(settings.engine, crate::generate::GenerationEngine::Candle);
    assert_eq!(settings.model, None);
    assert_eq!(settings.device, None);
    assert_eq!(settings.ollama_url, DEFAULT_OLLAMA_URL);
    assert_eq!(settings.context_tokens, None);
    assert!(settings.sampling_overrides.is_empty());
}

#[test]
fn legacy_generation_settings_default_to_candle_and_the_local_ollama_url() {
    let settings: GenerationSettings = serde_json::from_value(serde_json::json!({
        "enabled": true,
        "model": "org/model"
    }))
    .unwrap();
    assert_eq!(settings.engine, crate::generate::GenerationEngine::Candle);
    assert_eq!(settings.ollama_url, DEFAULT_OLLAMA_URL);
}

#[test]
fn legacy_quantized_gemma_selection_migrates_to_dense_model() {
    let settings: GenerationSettings = serde_json::from_value(serde_json::json!({
        "enabled": true,
        "model": LEGACY_QUANTIZED_GEMMA_MODEL
    }))
    .unwrap();
    assert_eq!(
        settings.model,
        Some(GeneratorModel(DENSE_GEMMA_MODEL.to_string()))
    );
    assert_eq!(
        serde_json::to_value(settings.model.unwrap()).unwrap(),
        serde_json::json!(DENSE_GEMMA_MODEL)
    );
}

#[test]
fn legacy_generation_timeout_is_not_persisted_as_a_second_worker_policy() {
    let settings: GenerationSettings = serde_json::from_value(serde_json::json!({
        "enabled": true,
        "model": "org/model",
        "device": null,
        "worker_timeout_secs": 60,
        "sampling_overrides": {}
    }))
    .unwrap();

    let serialized = serde_json::to_value(settings).unwrap();
    assert_eq!(
        serialized.get("worker_timeout_secs"),
        None,
        "generation must use the shared five-minute worker residency"
    );
    assert_eq!(crate::worker::DEFAULT_IDLE_TIMEOUT_SECS, 300);
}

#[test]
fn test_source_map_resolve_fallback() {
    let map = SourceMap {
        segments: vec![SourceSegment {
            text_range: ByteRange { start: 0, end: 10 },
            origin: SourceOrigin::TextFile { line: 1, col: 1 },
            provenance: Default::default(),
        }],
    };

    // Offset beyond all segments should fall back to last segment
    match map.resolve(100).unwrap() {
        SourceOrigin::TextFile { line, .. } => assert_eq!(line, 1),
        _ => panic!("Expected TextFile origin"),
    }
}

#[test]
fn test_source_map_resolve_range_multi_page() {
    let map = SourceMap {
        segments: vec![
            SourceSegment {
                text_range: ByteRange { start: 0, end: 10 },
                origin: SourceOrigin::PdfPage {
                    page: 1,
                    bbox: None,
                },
                provenance: Default::default(),
            },
            SourceSegment {
                text_range: ByteRange { start: 10, end: 20 },
                origin: SourceOrigin::PdfPage {
                    page: 2,
                    bbox: None,
                },
                provenance: Default::default(),
            },
        ],
    };

    // Range spanning page 1 and 2
    let origin = map.resolve_range(ByteRange { start: 5, end: 15 }).unwrap();
    match origin {
        SourceOrigin::PdfPage { page, .. } => assert_eq!(page, 1),
        _ => panic!("Expected PdfPage origin on page 1"),
    }
}

#[test]
fn test_source_map_resolve_range_no_overlap() {
    let map = SourceMap {
        segments: vec![SourceSegment {
            text_range: ByteRange { start: 10, end: 20 },
            origin: SourceOrigin::TextFile { line: 2, col: 1 },
            provenance: Default::default(),
        }],
    };

    // Range before any segment
    let origin = map.resolve_range(ByteRange { start: 0, end: 5 }).unwrap();
    match origin {
        SourceOrigin::TextFile { line, .. } => assert_eq!(line, 2),
        _ => panic!("Expected fallback to last segment"),
    }
}

#[test]
fn test_embedding_engine_supported() {
    let engines = EmbeddingEngine::supported_engines();
    assert!(!engines.is_empty());
    assert!(engines.contains(&EmbeddingEngine::SBERT));
}

#[test]
fn test_deserialize_custom_models() {
    #[derive(Deserialize)]
    struct Wrapper {
        #[allow(dead_code)]
        #[serde(deserialize_with = "deserialize_custom_models")]
        models: Vec<CustomModel>,
    }

    // Test old format (Vec<String>)
    let json = r#"{"models": ["model1", "model2"]}"#;
    let w: Wrapper = serde_json::from_str(json).unwrap();
    assert_eq!(w.models.len(), 2);
    assert_eq!(w.models[0].model_id, "model1");
    assert_eq!(w.models[0].engine, EmbeddingEngine::SBERT);

    // Test new format (Vec<CustomModel>)
    let json = r#"{"models": [{"engine": "Candle", "model_id": "model3"}]}"#;
    let w: Wrapper = serde_json::from_str(json).unwrap();
    assert_eq!(w.models.len(), 1);
    assert_eq!(w.models[0].model_id, "model3");
    assert_eq!(w.models[0].engine, EmbeddingEngine::Candle);
}

#[test]
fn test_deserialize_custom_models_invalid() {
    #[derive(Deserialize)]
    struct Wrapper {
        #[allow(dead_code)]
        #[serde(deserialize_with = "deserialize_custom_models")]
        models: Vec<CustomModel>,
    }

    let json = r#"{"models": [123]}"#;
    let res: Result<Wrapper, _> = serde_json::from_str(json);
    assert!(res.is_err());
}

#[test]
fn test_semantic_settings_deserialize_legacy_fields() {
    let json = r#"{
            "enabled": true,
            "engine": "Candle",
            "model": "sentence-transformers/all-MiniLM-L12-v2",
            "dimension": 384,
            "engine_devices": {},
            "index_path": null,
            "custom_models": [],
            "chunk_size": 600,
            "chunk_overlap": 128,
            "worker_timeout_secs": 300
        }"#;
    let settings: SemanticSettings = serde_json::from_str(json).unwrap();
    assert_eq!(settings.selected.engine, EmbeddingEngine::Candle);
    assert_eq!(
        settings.selected.model.model_id(),
        "sentence-transformers/all-MiniLM-L12-v2"
    );
    assert_eq!(settings.selected.dimension, 384);
}

/// `FileType` is serialized into saved settings, into search results and
/// into the interface, which compares it against the literal `"Pdf"`.
/// Renaming the variant must not rename the value.
#[test]
fn the_paged_file_type_is_still_pdf_on_the_wire() {
    assert_eq!(serde_json::to_string(&FileType::Paged).unwrap(), "\"Pdf\"");
    assert_eq!(
        serde_json::from_str::<FileType>("\"Pdf\"").unwrap(),
        FileType::Paged
    );
    assert_eq!(
        serde_json::to_string(&FileType::PlainText).unwrap(),
        "\"PlainText\""
    );
}

/// A book is detected as a paged document, not as plain text whose bytes
/// happen to be a zip. Before the MuPDF backend read these formats, an
/// `.epub` admitted to the extension list became `PlainText` and failed on
/// `read_to_string`.
#[test]
fn books_are_detected_as_paged_documents() {
    let supported = default_supported_extensions();
    for name in [
        "book.pdf",
        "book.epub",
        "book.mobi",
        "book.prc",
        "book.pdb",
        "book.fb2",
        "book.azw",
        "book.azw3",
        "book.cbz",
        "book.cbt",
    ] {
        assert_eq!(
            FileType::detect(std::path::Path::new(name), &supported),
            Some(FileType::Paged),
            "{name}"
        );
    }
    for name in ["notes.txt", "readme.md", "page.html", "data.xml", "run.log"] {
        assert_eq!(
            FileType::detect(std::path::Path::new(name), &supported),
            Some(FileType::PlainText),
            "{name}"
        );
    }
}

/// The whole point of admitting documents in `detect`: a settings file
/// that predates a format, or one a user pruned, still reads the books in
/// the corpus rather than silently omitting them.
#[test]
fn documents_are_admitted_with_no_extension_setting_at_all() {
    for extension in crate::extract::document::format::PagedFormat::EXTENSIONS {
        let name = format!("book.{extension}");
        assert_eq!(
            FileType::detect(std::path::Path::new(&name), &[]),
            Some(FileType::Paged),
            "{name}"
        );
    }
}

/// A stored list naming a document says nothing the reader does not
/// already do, so it does not survive the read: the setting holds text
/// extensions only, whatever an older install wrote into it.
#[test]
fn a_stored_document_extension_is_dropped_from_the_setting() {
    let mut json = serde_json::to_value(Settings::default()).unwrap();
    json["supported_extensions"] = serde_json::json!(["txt", "pdf", "EPUB", "rs", "cbz", "azw3"]);

    let settings: Settings = serde_json::from_value(json).unwrap();

    assert_eq!(
        settings.supported_extensions,
        vec!["txt".to_string(), "rs".to_string()]
    );
}

#[test]
fn test_file_type_detect_none() {
    assert_eq!(FileType::detect(Path::new("unknown"), &[]), None);
    assert_eq!(FileType::detect(Path::new("test.unknown"), &[]), None);
}

#[test]
fn test_file_type_detect_known_names() {
    let extensions = vec![];
    assert_eq!(
        FileType::detect(Path::new("Dockerfile"), &extensions),
        Some(FileType::PlainText)
    );
    assert_eq!(
        FileType::detect(Path::new("Makefile"), &extensions),
        Some(FileType::PlainText)
    );
    assert_eq!(
        FileType::detect(Path::new("dockerfile"), &extensions),
        Some(FileType::PlainText)
    );
}

#[test]
fn test_deserialize_custom_models_non_array() {
    #[derive(Deserialize)]
    struct Wrapper {
        #[allow(dead_code)]
        #[serde(deserialize_with = "deserialize_custom_models")]
        models: Vec<CustomModel>,
    }

    let json = r#"{"models": "not an array"}"#;
    let w: Wrapper = serde_json::from_str(json).unwrap();
    assert!(w.models.is_empty());
}

#[test]
fn test_search_query_defaults() {
    let json = r#"{"pattern": "p", "is_regex": false, "case_sensitive": false, "root": ".", "max_results": 10}"#;
    let q: SearchQuery = serde_json::from_str(json).unwrap();
    assert!(q.respect_gitignore);
    assert_eq!(q.max_file_size, 0);
    assert_eq!(q.context_lines, 2);
    assert_eq!(q.mode, SearchMode::Grep);
}

#[test]
fn test_embedder_model_serde() {
    let m = EmbedderModel("model-1".to_string());
    let json = serde_json::to_string(&m).unwrap();
    assert_eq!(json, "\"model-1\"");

    let m2: EmbedderModel = serde_json::from_str(&json).unwrap();
    assert_eq!(m2, m);
    assert_eq!(m2.model_id(), "model-1");
}

#[test]
fn test_settings_default() {
    let s = Settings::default();
    assert!(!s.supported_extensions.contains(&"pdf".to_string()));
    assert!(s.supported_extensions.contains(&"txt".to_string()));
    assert_eq!(s.context_lines, 2);
    assert_eq!(s.chat_backend, AgentBackend::ClaudeCode);
    assert_eq!(s.pdf_auto_zoom_target_px, 15.5);
    assert_eq!(s.max_file_size, 50 * 1024 * 1024);
    assert!(s.grep_use_index);
}

/// A settings file written before the toggle existed must read back as the
/// same value a fresh install starts with; otherwise the serde default and
/// `Settings::default` would be two answers to one question.
#[test]
fn grep_use_index_absent_from_an_older_settings_file_reads_as_the_default() {
    let json = r#"{
            "favorites": [],
            "recent_dirs": [],
            "respect_gitignore": true,
            "max_file_size": 10485760,
            "context_lines": 2,
            "theme": "System"
        }"#;

    let settings: Settings = serde_json::from_str(json).unwrap();
    assert_eq!(settings.grep_use_index, Settings::default().grep_use_index);
    assert!(settings.grep_use_index);
}

#[test]
fn editor_line_numbers_absent_from_an_older_settings_file_reads_as_shown() {
    let json = r#"{
            "favorites": [],
            "recent_dirs": [],
            "respect_gitignore": true,
            "max_file_size": 10485760,
            "context_lines": 2,
            "theme": "System"
        }"#;

    let settings: Settings = serde_json::from_str(json).unwrap();
    assert!(settings.editor_line_numbers);
    assert_eq!(
        settings.editor_line_numbers,
        Settings::default().editor_line_numbers
    );
}

#[test]
fn open_links_in_app_absent_from_an_older_settings_file_reads_as_the_default() {
    let json = r#"{
            "favorites": [],
            "recent_dirs": [],
            "respect_gitignore": true,
            "max_file_size": 10485760,
            "context_lines": 2,
            "theme": "System"
        }"#;

    let settings: Settings = serde_json::from_str(json).unwrap();
    assert_eq!(
        settings.open_links_in_app,
        Settings::default().open_links_in_app
    );
    assert!(settings.open_links_in_app);
}

#[test]
fn external_mcp_settings_default_old_configs_to_loopback() {
    let settings: ExternalMcpSettings =
        serde_json::from_str(r#"{"enabled":true,"port":39217}"#).unwrap();
    assert_eq!(
        settings.bind_address,
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
    );
    assert!(!settings.require_token);
    assert!(serde_json::from_str::<ExternalMcpSettings>(
        r#"{"enabled":true,"bind_address":"not-an-address","port":39217}"#
    )
    .is_err());
}

#[test]
fn test_removed_gemini_chat_backend_setting_migrates_to_default() {
    let json = r#"{
            "favorites": [],
            "recent_dirs": [],
            "respect_gitignore": true,
            "max_file_size": 10485760,
            "context_lines": 2,
            "theme": "System",
            "semantic": {
                "enabled": false,
                "index_path": null
            },
            "chat_backend": "Gemini"
        }"#;

    let settings: Settings = serde_json::from_str(json).unwrap();
    assert_eq!(settings.chat_backend, AgentBackend::ClaudeCode);
    assert_eq!(settings.pdf_auto_zoom_target_px, 15.5);
}

#[test]
fn test_nanocoder_chat_backend_setting_deserializes() {
    let json = r#"{
            "favorites": [],
            "recent_dirs": [],
            "respect_gitignore": true,
            "max_file_size": 10485760,
            "context_lines": 2,
            "theme": "System",
            "semantic": {
                "enabled": false,
                "index_path": null
            },
            "chat_backend": "Nanocoder"
        }"#;

    let settings: Settings = serde_json::from_str(json).unwrap();
    assert_eq!(settings.chat_backend, AgentBackend::Nanocoder);
}

#[test]
fn generation_stream_event_uses_one_tagged_transport_contract() {
    let event = GenerationStreamEvent::Completed {
        request_id: "summary-42".to_string(),
        task: GenerationTask::DocumentSummary,
        text: "Final summary.".to_string(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "phase": "completed",
            "request_id": "summary-42",
            "task": "document_summary",
            "text": "Final summary."
        })
    );
    assert_eq!(
        serde_json::from_value::<GenerationStreamEvent>(json).unwrap(),
        event
    );
}

// ── Integrations settings: the shapes older builds wrote ─────────────────

fn integrations_from(json: &str) -> IntegrationsSettings {
    serde_json::from_str(json).unwrap()
}

#[test]
fn settings_written_before_providers_were_manifests_are_read_into_todays_shape() {
    let json = r#"{
            "zotero": {"enabled": true, "base_url": "http://127.0.0.1:23119", "citation_style": "apa"},
            "semantic_scholar": {"enabled": true, "base_url": "https://api.semanticscholar.org", "api_key": " key-1 "},
            "openalex": {"enabled": true, "base_url": "https://mirror.example.test/", "email": "me@example.org"}
        }"#;
    let settings = integrations_from(json);

    let s2 = &settings.builtin["semantic_scholar"];
    assert!(s2.enabled);
    assert_eq!(s2.secrets["api_key"], "key-1");
    // The shipped origin is no override.
    assert_eq!(s2.base_url, None);

    let openalex = &settings.builtin["openalex"];
    assert!(openalex.enabled);
    // The retired `mailto` address has nowhere to go.
    assert!(openalex.secrets.is_empty());
    assert_eq!(
        openalex.base_url.as_deref(),
        Some("https://mirror.example.test")
    );
    assert!(settings.zotero.enabled);
}

#[test]
fn a_blank_legacy_credential_is_no_credential() {
    let json = r#"{"semantic_scholar": {"enabled": false, "api_key": "  "}}"#;
    let settings = integrations_from(json);
    assert!(settings.builtin["semantic_scholar"].secrets.is_empty());
}

#[test]
fn a_value_in_todays_shape_wins_over_the_older_key_and_only_today_is_written_back() {
    let json = r#"{
            "openalex": {"enabled": true, "base_url": "https://old.example.test"},
            "builtin": {"openalex": {"enabled": false, "secrets": {"api_key": "new-key"}}}
        }"#;
    let settings = integrations_from(json);
    let openalex = &settings.builtin["openalex"];
    assert!(!openalex.enabled);
    assert_eq!(openalex.base_url, None);
    assert_eq!(openalex.secrets["api_key"], "new-key");

    let written = serde_json::to_value(&settings).unwrap();
    assert!(written.get("openalex").is_none());
    assert!(written.get("semantic_scholar").is_none());
    assert!(written["builtin"]["openalex"].is_object());
    // And it reads back as itself.
    let again: IntegrationsSettings = serde_json::from_value(written).unwrap();
    assert_eq!(again.builtin, settings.builtin);
}

#[test]
fn an_openalex_email_stored_for_the_retired_mailto_pool_is_dropped() {
    let json = r#"{
            "builtin": {"openalex": {"enabled": true, "secrets": {"email": "me@example.org", "api_key": "key-1"}}}
        }"#;
    let settings = integrations_from(json);
    let secrets = &settings.builtin["openalex"].secrets;
    assert_eq!(secrets.get("email"), None);
    assert_eq!(secrets["api_key"], "key-1");
}

#[test]
fn a_stored_version_one_manifest_is_rewritten_as_version_two_when_settings_load() {
    let v1 = r#"
manifest_version = 1
id = "crossref"
name = "Crossref"
[http]
base_url = "https://api.crossref.org"
[[http.params]]
location = "query"
name = "mailto"
value = "team@example.com"
[capabilities.search]
path = "/works?query.bibliographic={query}&rows={limit}"
items = "message.items[*]"
[capabilities.search.fields]
id = "DOI"
title = "title[0]"
doi = { path = "DOI", coerce = "normalize_doi" }
"#;
    let json = serde_json::json!({
        "custom": [{"id": "crossref", "enabled": true, "manifest": v1}]
    });
    let settings: IntegrationsSettings = serde_json::from_value(json).unwrap();
    let config = &settings.custom[0];
    assert!(config.enabled);
    assert!(
        config.manifest.contains("manifest_version = 2"),
        "{}",
        config.manifest
    );
    // The rewrite is a manifest the engine loads.
    crate::integrations::custom::CustomSource::from_config(config, &settings).unwrap();
}

#[test]
fn a_version_one_manifest_that_cannot_be_converted_is_left_as_stored() {
    let broken = "manifest_version = 1\nid = \"x\"\nname = \"X\"\n[http]\nbase_url = \"https://x.test\"\n[capabilities.search]\npath = \"/s\"\n[capabilities.search.fields]\nid = \"a..b\"\n";
    let json = serde_json::json!({ "custom": [{"id": "x", "manifest": broken}] });
    let settings: IntegrationsSettings = serde_json::from_value(json).unwrap();
    // Unchanged: the registry will report it by name instead of this
    // guessing at what it meant.
    assert_eq!(settings.custom[0].manifest, broken);
}

#[test]
fn a_metadata_source_preference_is_the_bare_string_it_always_was() {
    for (stored, expected) in [
        ("file", MetadataSourcePreference::File),
        ("zotero", MetadataSourcePreference::Zotero),
        (
            "openalex",
            MetadataSourcePreference::Provider("openalex".into()),
        ),
        (
            "custom:crossref",
            MetadataSourcePreference::Provider("custom:crossref".into()),
        ),
    ] {
        let parsed: MetadataSourcePreference =
            serde_json::from_value(serde_json::json!(stored)).unwrap();
        assert_eq!(parsed, expected);
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            serde_json::json!(stored)
        );
    }
    assert!(serde_json::from_value::<MetadataSourcePreference>(serde_json::json!("")).is_err());
}
