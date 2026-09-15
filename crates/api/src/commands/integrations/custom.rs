//! Commands for integrations the user described.
//!
//! Three operations, in the order the user meets them: read a draft manifest
//! and say what it declares ([`custom_integration_summary`]), run it once and
//! show what it made of a real response ([`custom_integration_probe`]), and
//! report whether a saved one is reachable ([`custom_integration_status`]).
//!
//! The first two take a draft rather than an id on purpose. A manifest must be
//! checkable *before* it is stored — storing it first would mean either
//! persisting something known-broken or inventing a second, "unsaved" storage
//! state for drafts.

use std::collections::HashMap;

use wilkes_core::integrations::custom::manifest::{authoring_prompt, Manifest};
use wilkes_core::integrations::custom::{CustomSource, ProbeReport};
use wilkes_core::integrations::LiteratureSource;
use wilkes_core::types::{IntegrationStatus, Settings};

/// What a manifest declares, for the import dialog to show before anything is
/// saved.
///
/// Importing a manifest is an egress decision — it is a description of who
/// Wilkes will talk to, written by whoever handed over the file — so every
/// origin it may contact is named here rather than discovered from network
/// traffic afterwards.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ManifestSummary {
    pub id: String,
    pub name: String,
    /// All declared request origins, deduplicated in declaration order.
    pub origins: Vec<String>,
    pub capabilities: Vec<String>,
    /// Secrets the manifest names and the user must supply.
    pub required_secrets: Vec<String>,
    /// Empty when the manifest is valid. Every problem at once, never the
    /// first one.
    pub problems: Vec<String>,
}

/// The model-facing authoring contract. Core owns its contents beside the
/// parser; every application surface returns this same string unchanged.
pub fn custom_integration_authoring_prompt() -> String {
    authoring_prompt()
}

pub fn custom_integration_summary(manifest: String) -> ManifestSummary {
    match Manifest::parse(&manifest) {
        Ok(manifest) => ManifestSummary {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            origins: manifest.origins(),
            capabilities: capability_names(&manifest),
            required_secrets: manifest
                .required_secrets()
                .into_iter()
                .map(str::to_string)
                .collect(),
            problems: Vec::new(),
        },
        // A manifest that does not parse has no id, name or origins to report;
        // saying so with the parse errors is the whole answer.
        Err(error) => ManifestSummary {
            id: String::new(),
            name: String::new(),
            origins: Vec::new(),
            capabilities: Vec::new(),
            required_secrets: Vec::new(),
            problems: vec![error.to_string()],
        },
    }
}

fn capability_names(manifest: &Manifest) -> Vec<String> {
    let mut names = Vec::new();
    if manifest.capabilities.search.is_some() {
        names.push("search".to_string());
    }
    if manifest.capabilities.health.is_some() {
        names.push("health".to_string());
    }
    if manifest.capabilities.resolve_download.is_some() {
        names.push("resolve_download".to_string());
    }
    names
}

/// Run a draft manifest's search capability once and report what came back.
pub async fn custom_integration_probe(
    manifest: String,
    secrets: HashMap<String, String>,
    query: String,
) -> anyhow::Result<ProbeReport> {
    let manifest = Manifest::parse(&manifest)?;
    Ok(CustomSource::new(manifest, secrets)?.probe(&query).await)
}

pub async fn custom_integration_status(
    settings: Settings,
    id: String,
) -> anyhow::Result<IntegrationStatus> {
    let config = settings
        .integrations
        .custom
        .iter()
        .find(|config| config.id == id)
        .ok_or_else(|| anyhow::anyhow!("No custom integration with id '{id}'"))?;
    CustomSource::from_config(config)?
        .status(config.enabled)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
manifest_version = 1
id = "crossref"
name = "Crossref"

[http]
base_url = "https://api.crossref.org"

[[http.params]]
location = "header"
name = "Crossref-Plus-API-Token"
secret = "crossref_token"

[capabilities.search]
path = "/works?query.bibliographic={query}&rows={limit}"
items = "message.items[*]"

[capabilities.search.fields]
id = "DOI"
title = "title[0]"
"#;

    #[test]
    fn summary_names_the_origins_and_the_secrets_before_anything_is_saved() {
        let summary = custom_integration_summary(MANIFEST.to_string());
        assert!(summary.problems.is_empty(), "{:?}", summary.problems);
        assert_eq!(summary.origins, vec!["https://api.crossref.org"]);
        assert_eq!(summary.capabilities, vec!["search"]);
        assert_eq!(summary.required_secrets, vec!["crossref_token"]);
    }

    #[test]
    fn authoring_prompt_comes_from_the_core_manifest_contract() {
        assert_eq!(custom_integration_authoring_prompt(), authoring_prompt());
    }

    #[test]
    fn a_manifest_that_does_not_parse_reports_problems_and_no_origins() {
        let summary = custom_integration_summary("id = 'nope'".to_string());
        assert!(summary.origins.is_empty());
        assert!(!summary.problems.is_empty());
    }

    #[tokio::test]
    async fn probing_without_a_secret_fails_before_any_request() {
        let report = custom_integration_probe(
            MANIFEST.to_string(),
            HashMap::new(),
            "test query".to_string(),
        )
        .await
        .unwrap();
        assert!(!report.ok);
        assert!(report.error.unwrap().contains("secret 'crossref_token'"));
    }

    #[tokio::test]
    async fn status_refuses_an_id_that_is_not_configured() {
        let error = custom_integration_status(Settings::default(), "crossref".into())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("No custom integration"));
    }
}
