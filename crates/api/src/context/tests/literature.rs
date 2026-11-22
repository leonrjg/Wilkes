use super::*;

#[tokio::test]
async fn a_manifest_an_agent_saves_reaches_the_settings_file_and_the_interface_is_told() {
    use wilkes_agent::search::SearchService;
    let dir = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let ctx = generation_ctx(dir.path(), Arc::clone(&events));
    let manifest = r#"
manifest_version = 2
id = "crossref"
name = "Crossref"
[http]
base_url = "https://api.crossref.org"
[capabilities.search]
path = "/works?query.bibliographic={query}&rows={limit}"
items = "message.items"
[capabilities.search.fields]
id = "DOI"
title = "title[0]"
"#;
    let saved = Arc::clone(&ctx)
        .save_custom_integration(manifest.to_string(), Some(true))
        .await
        .unwrap();
    assert_eq!(saved["provider"], "custom:crossref");
    assert_eq!(saved["added"], true);

    let stored = get_settings(&dir.path().join("settings.json"))
        .await
        .unwrap()
        .integrations
        .custom;
    assert_eq!(stored.len(), 1);
    assert!(stored[0].enabled);
    assert_eq!(stored[0].manifest, manifest);
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|(name, _)| name == "integrations-updated"));

    let listed = Arc::clone(&ctx).custom_integrations().await.unwrap();
    assert_eq!(listed[0]["provider"], "custom:crossref");
}
