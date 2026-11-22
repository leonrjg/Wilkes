use super::*;

#[tokio::test]
async fn test_citation_links_rejects_root_outside_library() {
    let (dir, ctx) = test_ctx();
    let library = dir.path().join("library");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&library).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let source = outside.join("source.txt");
    std::fs::write(&source, "source").unwrap();
    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({ "last_directory": library }),
    )
    .await
    .unwrap();

    let err = ctx
        .citation_links(CitationLinksQuery {
            root: outside,
            path: source,
        })
        .await
        .unwrap_err();

    assert!(err.contains("not in the library"));
}

#[tokio::test]
async fn test_citation_links_empty_when_anchor_has_no_doi() {
    let (dir, ctx) = test_ctx();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let source = library.join("source.txt");
    std::fs::write(&source, "source").unwrap();
    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({ "last_directory": library }),
    )
    .await
    .unwrap();

    let links = ctx
        .clone()
        .citation_links(CitationLinksQuery {
            root: library,
            path: source,
        })
        .await
        .unwrap();

    assert!(links.references.is_empty());
    assert!(links.cited_by.is_empty());
    assert!(links.all_references.is_empty());
}

#[tokio::test]
async fn test_reading_order_orders_a_folder_by_the_subjects_its_documents_name() {
    let (dir, ctx) = test_ctx();
    let library = std::fs::canonicalize(dir.path()).unwrap().join("library");
    let algebra_folder = library.join("Algebra");
    std::fs::create_dir_all(&algebra_folder).unwrap();
    let arithmetic = library.join("arithmetic.txt");
    // Named by its folder, not by its file.
    let algebra = algebra_folder.join("notes-2.txt");
    let calculus = library.join("calculus.txt");
    let scan = library.join("scan.pdf");
    std::fs::write(&arithmetic, reading_order_book(20, &[("arithmetic", 20)])).unwrap();
    std::fs::write(
        &algebra,
        reading_order_book(20, &[("algebra", 20), ("arithmetic", 3)]),
    )
    .unwrap();
    std::fs::write(
        &calculus,
        reading_order_book(
            20,
            &[
                ("calculus", 20),
                ("algebra", 4),
                ("arithmetic", 2),
                ("limits", 5),
            ],
        ),
    )
    .unwrap();
    // A paged document the text index has not read.
    std::fs::write(&scan, "%PDF-1.7").unwrap();
    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({ "last_directory": library }),
    )
    .await
    .unwrap();

    let order = ctx
        .clone()
        .reading_order(ReadingOrderQuery {
            root: library.clone(),
            theme: None,
        })
        .await
        .unwrap();

    let paths: Vec<&Path> = order.documents.iter().map(|d| d.path.as_path()).collect();
    assert_eq!(paths, [&arithmetic, &algebra, &calculus, &scan]);
    let placements: Vec<ReadingPlacement> = order.documents.iter().map(|d| d.placement).collect();
    assert_eq!(
        placements,
        [
            ReadingPlacement::Foundation,
            ReadingPlacement::Bridge,
            ReadingPlacement::Specialised,
            ReadingPlacement::NoText,
        ]
    );
    assert_eq!(order.documents[1].subjects, ["algebra"]);
    assert_eq!(order.documents[1].after.len(), 1);
    assert_eq!(order.documents[1].after[0].path, arithmetic);
    assert_eq!(
        order.documents[1].after[0].reasons,
        [ReadingOrderReason::Names {
            subjects: vec!["arithmetic".into()]
        }]
    );
    assert_eq!(order.documents[0].needed_by, 2);
    assert!(order.documents.iter().all(|d| d.theme_share.is_none()));
    let subjects: Vec<(&str, usize)> = order
        .subjects
        .iter()
        .map(|s| (s.name.as_str(), s.documents))
        .collect();
    assert_eq!(
        subjects,
        [("arithmetic", 3), ("algebra", 2), ("calculus", 1)]
    );

    // A theme says how much of each document discusses it, in the same
    // order.
    let themed = ctx
        .clone()
        .reading_order(ReadingOrderQuery {
            root: library.clone(),
            theme: Some(" Limits ".into()),
        })
        .await
        .unwrap();
    assert_eq!(themed.theme.as_deref(), Some("limit"));
    let shares: Vec<Option<f32>> = themed.documents.iter().map(|d| d.theme_share).collect();
    assert_eq!(shares, [Some(0.0), Some(0.0), Some(0.25), None]);

    // A document that changed is read again: algebra no longer names
    // arithmetic, so nothing ties the two.
    std::fs::write(&algebra, reading_order_book(21, &[("algebra", 21)])).unwrap();
    let reread = ctx
        .clone()
        .reading_order(ReadingOrderQuery {
            root: library.clone(),
            theme: None,
        })
        .await
        .unwrap();
    let algebra_now = reread.documents.iter().find(|d| d.path == algebra).unwrap();
    assert!(algebra_now.after.iter().all(|link| link.path != arithmetic));

    // A subfolder is ordered on its own, and a folder outside the library
    // is refused.
    let folder_only = ctx
        .clone()
        .reading_order(ReadingOrderQuery {
            root: algebra_folder.clone(),
            theme: None,
        })
        .await
        .unwrap();
    assert_eq!(folder_only.documents.len(), 1);
    assert_eq!(
        folder_only.documents[0].placement,
        ReadingPlacement::Standalone
    );
    let outside = ctx
        .clone()
        .reading_order(ReadingOrderQuery {
            root: std::fs::canonicalize(dir.path()).unwrap(),
            theme: None,
        })
        .await
        .unwrap_err();
    assert!(outside.contains("not in the library"), "{outside}");
    let wordy = ctx
        .clone()
        .reading_order(ReadingOrderQuery {
            root: library.clone(),
            theme: Some("one two three four five".into()),
        })
        .await
        .unwrap_err();
    assert!(wordy.contains("one to 4 words"), "{wordy}");
}

#[tokio::test]
async fn test_citation_graph_joins_a_scope_by_doi_and_says_what_it_covers() {
    let (dir, ctx) = test_ctx();
    let library = std::fs::canonicalize(dir.path()).unwrap().join("library");
    let sub = library.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let a = library.join("a.txt");
    let a_copy = library.join("a-copy.txt");
    let b = library.join("b.txt");
    let no_doi = library.join("no-doi.txt");
    let d = sub.join("d.txt");
    for (path, text) in [
        (&a, "a"),
        (&a_copy, "a copy"),
        (&b, "b"),
        (&no_doi, "none"),
        (&d, "d"),
    ] {
        std::fs::write(path, text).unwrap();
    }

    let cache = ctx.metadata_cache().expect("cache opens");
    {
        let guard = cache.lock().unwrap();
        for (path, doi) in [
            (&a, Some("10.1000/A")),
            (&a_copy, Some("10.1000/a")),
            (&b, Some("10.1000/b")),
            (&no_doi, None),
            (&d, Some("10.1000/d")),
        ] {
            guard
                .upsert(
                    path,
                    FileIdentity::for_path(path).unwrap(),
                    &DocumentMetadata {
                        title: Some(path.file_name().unwrap().to_string_lossy().into()),
                        doi: doi.map(str::to_string),
                        ..DocumentMetadata::default()
                    },
                    wilkes_core::metadata::cache::MetadataSource::File,
                )
                .unwrap();
        }
        let cited = |doi: &str, title: Option<&str>| CitationReference {
            doi: doi.into(),
            title: title.map(str::to_string),
            ..CitationReference::default()
        };
        guard
            .replace_citations(
                "10.1000/a",
                "openalex",
                &[
                    cited("10.1000/b", None),
                    cited("10.1000/x", Some("Outside work")),
                    cited("10.1000/y", None),
                    cited("10.1000/d", None),
                ],
            )
            .unwrap();
        guard
            .replace_citations(
                "10.1000/b",
                "openalex",
                &[cited("10.1000/x", Some("Outside work"))],
            )
            .unwrap();
    }
    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({ "last_directory": library }),
    )
    .await
    .unwrap();

    let graph = ctx
        .clone()
        .citation_graph(CitationGraphQuery {
            root: None,
            min_external_citations: 2,
        })
        .await
        .unwrap();
    let dois: Vec<&str> = graph.documents.iter().map(|d| d.doi.as_str()).collect();
    assert_eq!(dois, ["10.1000/a", "10.1000/b", "10.1000/d"]);
    // Two copies of one paper are one node.
    assert_eq!(graph.documents[0].entries.len(), 2);
    let edges: Vec<(usize, usize)> = graph.edges.iter().map(|e| (e.source, e.target)).collect();
    assert_eq!(edges, [(0, 1), (0, 2)]);
    assert_eq!(graph.documents[1].cited_by, [0]);
    // d's references were never fetched: unknown, not none.
    assert!(graph.documents[0].references_fetched);
    assert!(!graph.documents[2].references_fetched);
    // x is cited twice and listed; y once and only counted.
    assert_eq!(graph.external_total, 2);
    assert_eq!(graph.external.len(), 1);
    assert_eq!(graph.external[0].reference.doi, "10.1000/x");
    assert_eq!(
        graph.external[0].reference.title.as_deref(),
        Some("Outside work")
    );
    assert_eq!(graph.external[0].cited_by, [0, 1]);
    assert!(graph.external[0].library_entry.is_none());
    assert_eq!(
        graph
            .without_doi
            .iter()
            .map(|e| e.path.clone())
            .collect::<Vec<_>>(),
        [no_doi.clone()]
    );
    assert_eq!(
        graph.coverage,
        CitationCoverage {
            files: 5,
            pending_metadata: 0,
            metadata_unavailable: 0,
            with_doi: 3,
            references_fetched: 2,
            references_provider: graph.coverage.references_provider,
        }
    );

    // Scoped to a folder, a work in another folder of the library is
    // outside the scope but still found in the library.
    let graph = ctx
        .clone()
        .citation_graph(CitationGraphQuery {
            root: Some(library.clone()),
            min_external_citations: 1,
        })
        .await
        .unwrap();
    assert_eq!(graph.documents.len(), 3);
    let graph = ctx
        .clone()
        .citation_graph(CitationGraphQuery {
            root: Some(sub.clone()),
            min_external_citations: 1,
        })
        .await
        .unwrap();
    assert_eq!(graph.documents.len(), 1);
    assert_eq!(graph.documents[0].doi, "10.1000/d");
    assert!(graph.edges.is_empty());
    assert!(graph.external.is_empty());
    assert_eq!(graph.coverage.files, 1);

    // A root outside the library is refused.
    let outside = std::fs::canonicalize(dir.path()).unwrap();
    assert!(ctx
        .citation_graph(CitationGraphQuery {
            root: Some(outside),
            min_external_citations: 1,
        })
        .await
        .is_err());
}

#[tokio::test]
async fn test_citation_links_resolves_library_edges() {
    let (dir, ctx) = test_ctx();
    let library = std::fs::canonicalize(dir.path()).unwrap().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let source = library.join("source.txt");
    let cited = library.join("cited.txt");
    std::fs::write(&source, "source").unwrap();
    std::fs::write(&cited, "cited").unwrap();

    let cache = ctx.metadata_cache().expect("cache opens");
    // Seed each row under the file's real identity. A fabricated identity
    // would make every listing treat the file as uncached and schedule a
    // background re-extraction, which would then own (and overwrite) the
    // DOI these edges are keyed by.
    let source_id = FileIdentity::for_path(&source).unwrap();
    let cited_id = FileIdentity::for_path(&cited).unwrap();
    {
        let guard = cache.lock().unwrap();
        guard
            .upsert(
                &source,
                source_id,
                &DocumentMetadata {
                    doi: Some("10.1000/source".into()),
                    ..DocumentMetadata::default()
                },
                wilkes_core::metadata::cache::MetadataSource::File,
            )
            .unwrap();
        guard
            .upsert(
                &cited,
                cited_id,
                &DocumentMetadata {
                    doi: Some("10.1000/cited.long".into()),
                    ..DocumentMetadata::default()
                },
                wilkes_core::metadata::cache::MetadataSource::File,
            )
            .unwrap();
        guard
            .replace_citations(
                "10.1000/source",
                "openalex",
                &[
                    CitationReference {
                        doi: "10.1000/cited".into(),
                        ..CitationReference::default()
                    },
                    CitationReference {
                        doi: "10.1000/cited.long".into(),
                        title: Some("Exact cited work".into()),
                        authors: Some("Smith".into()),
                        year: Some(2024),
                        venue: None,
                    },
                    CitationReference {
                        doi: "10.1000/missing".into(),
                        ..CitationReference::default()
                    },
                ],
            )
            .unwrap();
    }

    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({ "last_directory": library }),
    )
    .await
    .unwrap();

    let links = ctx
        .clone()
        .citation_links(CitationLinksQuery {
            root: library.clone(),
            path: source.clone(),
        })
        .await
        .unwrap();

    assert_eq!(
        links
            .references
            .iter()
            .map(|e| e.path.clone())
            .collect::<Vec<_>>(),
        vec![cited.clone()]
    );
    assert!(links.cited_by.is_empty());
    assert_eq!(
        links
            .all_references
            .iter()
            .map(|reference| (
                reference.doi.as_str(),
                reference.title.as_deref(),
                reference.authors.as_deref(),
                reference.year,
            ))
            .collect::<Vec<_>>(),
        vec![
            ("10.1000/cited", None, None, None),
            (
                "10.1000/cited.long",
                Some("Exact cited work"),
                Some("Smith"),
                Some(2024),
            ),
            ("10.1000/missing", None, None, None),
        ]
    );

    // The reverse direction resolves from the same stored edge.
    let reverse = ctx
        .citation_links(CitationLinksQuery {
            root: library,
            path: cited,
        })
        .await
        .unwrap();
    assert_eq!(
        reverse
            .cited_by
            .iter()
            .map(|e| e.path.clone())
            .collect::<Vec<_>>(),
        vec![source]
    );
    assert!(reverse.references.is_empty());
    assert!(reverse.all_references.is_empty());
}
