//! Commands for integrations the user described.
//!
//! Two operations, in the order the user meets them: read a draft manifest
//! and say what it declares ([`custom_integration_summary`]), and run it once
//! and show what it made of a real response ([`custom_integration_probe`]).
//! Whether a *saved* provider is reachable is not asked here: it is asked of
//! the registry, in [`super::literature::status`], the same way for every
//! provider.
//!
//! The first two take a draft rather than an id on purpose. A manifest must be
//! checkable *before* it is stored — storing it first would mean either
//! persisting something known-broken or inventing a second, "unsaved" storage
//! state for drafts. They also take the saved integrations, which is where a
//! draft that extends another provider finds it.

use std::collections::HashMap;

use wilkes_core::integrations::custom::legacy;
use wilkes_core::integrations::custom::manifest::{authoring_prompt, Manifest, SecretSlot};
use wilkes_core::integrations::custom::{CustomSource, ProbeReport};
use wilkes_core::types::IntegrationsSettings;

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
    /// Secrets the manifest names and the user must supply before it can send
    /// a request.
    pub required_secrets: Vec<String>,
    /// Secrets that improve a request but are not needed to send it.
    pub optional_secrets: Vec<String>,
    /// Every secret this manifest names itself, once, as a form needs to
    /// render it. One that extends another provider does not list that
    /// provider's: they are set on it.
    pub secrets: Vec<SecretSlot>,
    /// The provider this manifest extends and what it takes from it.
    #[serde(default)]
    pub extends: Option<ExtendsSummary>,
    /// The result fields the manifest's search fills, which are the values a
    /// download step can count on finding in a result.
    #[serde(default)]
    pub result_fields: Vec<String>,
    /// Empty when the manifest is valid. Every problem at once, never the
    /// first one.
    pub problems: Vec<String>,
    /// When the manifest is one written before version 2, the same manifest
    /// as version 2. Nothing is saved or changed: it is what the editor offers
    /// to replace the text with.
    #[serde(default)]
    pub upgraded: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ExtendsSummary {
    pub provider: String,
    /// What the base calls itself.
    pub name: String,
    pub capabilities: Vec<String>,
}

/// The model-facing authoring contract. Core owns its contents beside the
/// parser; every application surface returns this same string unchanged.
pub fn custom_integration_authoring_prompt() -> String {
    authoring_prompt()
}

pub fn custom_integration_summary(
    manifest: String,
    integrations: &IntegrationsSettings,
) -> ManifestSummary {
    match Manifest::resolve(&manifest, integrations) {
        Ok(resolved) => {
            let secrets = resolved.own_secret_slots();
            let named = |optional: bool| {
                let mut names: Vec<String> = secrets
                    .iter()
                    .filter(|slot| slot.optional == optional)
                    .map(|slot| slot.name.clone())
                    .collect();
                names.sort_unstable();
                names
            };
            let manifest = &resolved.manifest;
            ManifestSummary {
                id: manifest.id.clone(),
                name: manifest.name.clone(),
                origins: manifest.origins(),
                capabilities: manifest
                    .capabilities
                    .declared()
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                required_secrets: named(false),
                optional_secrets: named(true),
                extends: resolved.inherited.map(|inherited| ExtendsSummary {
                    provider: inherited.provider,
                    name: inherited.name,
                    capabilities: inherited.capabilities,
                }),
                result_fields: manifest
                    .capabilities
                    .search
                    .iter()
                    .flat_map(|search| search.fields.keys().cloned())
                    .collect(),
                secrets,
                problems: Vec::new(),
                upgraded: None,
            }
        }
        // A manifest that does not parse has no id, name or origins to report;
        // saying so with the parse errors is the whole answer. One written
        // before version 2 also says what it would be as version 2, which is
        // an answer the user can act on rather than a version number — or, when
        // it cannot be converted, why not.
        Err(error) => {
            let mut problems = vec![error.to_string()];
            let mut upgraded = None;
            if legacy::declares_legacy_version(&manifest) {
                match legacy::upgrade(&manifest) {
                    Ok(converted) => upgraded = converted,
                    Err(conversion) => {
                        tracing::info!("version 1 manifest could not be converted: {conversion:#}");
                        problems.push(format!(
                            "It could not be converted to version 2 automatically: {conversion:#}"
                        ));
                    }
                }
            }
            ManifestSummary {
                id: String::new(),
                name: String::new(),
                origins: Vec::new(),
                capabilities: Vec::new(),
                required_secrets: Vec::new(),
                optional_secrets: Vec::new(),
                secrets: Vec::new(),
                extends: None,
                result_fields: Vec::new(),
                problems,
                upgraded,
            }
        }
    }
}

/// Run a draft manifest's search capability once and report what came back.
pub async fn custom_integration_probe(
    manifest: String,
    secrets: HashMap<String, String>,
    query: String,
    integrations: &IntegrationsSettings,
) -> anyhow::Result<ProbeReport> {
    Ok(
        CustomSource::from_draft(&manifest, secrets, None, integrations)?
            .probe(&query)
            .await,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
manifest_version = 2
id = "crossref"
name = "Crossref"

[http]
base_url = "https://api.crossref.org"

[http.headers]
Crossref-Plus-API-Token = { secret = "crossref_token" }

[http.query]
mailto = { secret = "email", optional = true, label = "Email", kind = "email" }

[capabilities.search]
path = "/works?query.bibliographic={query}&rows={limit}"
items = "message.items"

[capabilities.search.fields]
id = "DOI"
title = "title[0]"
"#;

    #[test]
    fn summary_names_the_origins_and_the_secrets_before_anything_is_saved() {
        let summary =
            custom_integration_summary(MANIFEST.to_string(), &IntegrationsSettings::default());
        assert!(summary.problems.is_empty(), "{:?}", summary.problems);
        assert_eq!(summary.origins, vec!["https://api.crossref.org"]);
        assert_eq!(summary.capabilities, vec!["search"]);
        assert_eq!(summary.required_secrets, vec!["crossref_token"]);
        assert_eq!(summary.optional_secrets, vec!["email"]);
        assert_eq!(summary.secrets.len(), 2);
        assert_eq!(
            summary
                .secrets
                .iter()
                .find(|s| s.name == "email")
                .unwrap()
                .label,
            "Email"
        );
    }

    #[test]
    fn summary_names_every_capability_the_manifest_declares() {
        let shipped = wilkes_core::integrations::builtin::BUILTIN_MANIFESTS
            .iter()
            .find(|builtin| builtin.id == "openalex")
            .unwrap();
        let summary = custom_integration_summary(
            shipped.source.to_string(),
            &IntegrationsSettings::default(),
        );
        assert!(summary.problems.is_empty(), "{:?}", summary.problems);
        assert_eq!(
            summary.capabilities,
            vec!["search", "browse", "health", "lookup", "references"]
        );
    }

    #[test]
    fn summary_of_a_manifest_that_extends_says_what_is_the_bases_and_what_is_its_own() {
        let derived = r#"
manifest_version = 2
id = "openalex-files"
name = "OpenAlex with downloads"
extends = { provider = "openalex", capabilities = ["search", "browse"] }

[capabilities.resolve_download]
url = "{download_url}"
steps = [
  { base_url = "https://files.example", path = "/by-doi?doi={doi}", query = { key = { secret = "files_key" } }, fields = { download_url = "url" } },
]
"#;
        let summary =
            custom_integration_summary(derived.to_string(), &IntegrationsSettings::default());
        assert!(summary.problems.is_empty(), "{:?}", summary.problems);
        assert_eq!(
            summary.extends,
            Some(ExtendsSummary {
                provider: "openalex".into(),
                name: "OpenAlex".into(),
                capabilities: vec!["search".into(), "browse".into()],
            })
        );
        // Both origins are named before anything is saved, the base's included.
        assert_eq!(
            summary.origins,
            vec!["https://api.openalex.org", "https://files.example"]
        );
        assert_eq!(
            summary.capabilities,
            vec!["search", "browse", "resolve_download"]
        );
        // OpenAlex's email is set on OpenAlex.
        assert_eq!(summary.required_secrets, vec!["files_key"]);
        assert!(summary.optional_secrets.is_empty());
        assert_eq!(summary.secrets.len(), 1);
        // What a download step can count on a result holding.
        assert!(summary.result_fields.contains(&"doi".to_string()));
        assert!(!summary.result_fields.contains(&"file_format".to_string()));
    }

    #[test]
    fn authoring_prompt_comes_from_the_core_manifest_contract() {
        assert_eq!(custom_integration_authoring_prompt(), authoring_prompt());
    }

    #[test]
    fn a_manifest_that_does_not_parse_reports_problems_and_no_origins() {
        let summary =
            custom_integration_summary("id = 'nope'".to_string(), &IntegrationsSettings::default());
        assert!(summary.origins.is_empty());
        assert!(!summary.problems.is_empty());
    }

    #[tokio::test]
    async fn probing_without_a_secret_fails_before_any_request() {
        let report = custom_integration_probe(
            MANIFEST.to_string(),
            HashMap::new(),
            "test query".to_string(),
            &IntegrationsSettings::default(),
        )
        .await
        .unwrap();
        assert!(!report.ok);
        assert!(report.error.unwrap().contains("secret 'crossref_token'"));
    }

    #[test]
    fn a_version_one_manifest_is_refused_and_offered_as_version_two() {
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
"#;
        let summary = custom_integration_summary(v1.to_string(), &IntegrationsSettings::default());
        assert!(summary.problems[0].contains("manifest_version 1 is not supported"));
        let upgraded = summary
            .upgraded
            .expect("a convertible version 1 manifest is offered");
        // The offer is a manifest the same function accepts.
        let again = custom_integration_summary(upgraded, &IntegrationsSettings::default());
        assert!(again.problems.is_empty(), "{:?}", again.problems);
        assert_eq!(again.origins, vec!["https://api.crossref.org"]);
        assert!(again.upgraded.is_none());
    }

    #[test]
    fn a_version_one_manifest_that_cannot_be_converted_says_why() {
        let v1 = "manifest_version = 1\nid = \"x\"\nname = \"X\"\n[http]\nbase_url = \"https://x.test\"\n[capabilities.search]\npath = \"/s\"\n[capabilities.search.fields]\nid = \"a..b\"\n";
        let summary = custom_integration_summary(v1.to_string(), &IntegrationsSettings::default());
        assert!(summary.upgraded.is_none());
        assert_eq!(summary.problems.len(), 2, "{:?}", summary.problems);
        assert!(
            summary.problems[1].contains("could not be converted"),
            "{:?}",
            summary.problems
        );
    }

    #[test]
    fn a_manifest_that_is_wrong_in_another_way_offers_no_conversion() {
        let summary =
            custom_integration_summary("id = 'nope'".to_string(), &IntegrationsSettings::default());
        assert!(summary.upgraded.is_none());
    }
}
