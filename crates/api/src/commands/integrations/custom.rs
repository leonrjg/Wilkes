//! Commands for integrations the user described.
//!
//! Two operations, in the order the user meets them: read a draft manifest
//! and say what it declares ([`custom_integration_summary`]), and run it once
//! and show what it made of a real response ([`custom_integration_probe`]).
//! An agent does the same through [`agent_probe`], and stores what it wrote
//! through [`with_saved_manifest`].
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
use wilkes_core::types::{CustomIntegrationConfig, IntegrationsSettings};

use wilkes_agent::search::IntegrationProbe;
use wilkes_core::integrations::ProgramAllowlist;

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
    /// Every program the manifest may run to fetch a file, and whether this
    /// installation allows it. Shown beside the origins for the same reason:
    /// it is what importing the manifest would let it do.
    #[serde(default)]
    pub programs: Vec<ProgramSummary>,
    /// Whether the user may allow a program here, or the host fixed the list.
    #[serde(default)]
    pub programs_editable: bool,
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
pub struct ProgramSummary {
    pub program: String,
    pub allowed: bool,
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
    allowlist: &ProgramAllowlist,
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
                programs: manifest
                    .programs()
                    .into_iter()
                    .map(|program| ProgramSummary {
                        allowed: allowlist.programs.contains(&program),
                        program,
                    })
                    .collect(),
                programs_editable: allowlist.editable,
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
                programs: Vec::new(),
                programs_editable: allowlist.editable,
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
    // A search that is a run runs only once its program is allowed, here as
    // anywhere.
    Ok(
        CustomSource::from_draft(&manifest, secrets, None, integrations)?
            .with_allowed_programs(
                wilkes_core::integrations::allowed_programs(integrations).programs,
            )
            .probe(&query)
            .await,
    )
}

/// A stored custom integration as an agent reads it. Secrets are named, never
/// valued: the agent edits the manifest, and the user holds the credentials.
#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct StoredIntegration {
    pub id: String,
    /// The id other tools take, such as `literature_search`'s `provider`.
    pub provider: String,
    /// What the user renamed it to, when they did.
    pub name: Option<String>,
    pub enabled: bool,
    pub secrets_set: Vec<String>,
    /// Only when one integration was asked for.
    pub manifest: Option<String>,
}

/// Every stored custom integration, or the one `id` names with its manifest.
pub fn stored_integrations(
    integrations: &IntegrationsSettings,
    id: Option<&str>,
) -> anyhow::Result<Vec<StoredIntegration>> {
    let stored = |config: &CustomIntegrationConfig, manifest: bool| {
        let mut secrets_set: Vec<String> = config.secrets.keys().cloned().collect();
        secrets_set.sort_unstable();
        StoredIntegration {
            id: config.id.clone(),
            provider: format!("custom:{}", config.id),
            name: config.name.clone(),
            enabled: config.enabled,
            secrets_set,
            manifest: manifest.then(|| config.manifest.clone()),
        }
    };
    match id {
        None => Ok(integrations
            .custom
            .iter()
            .map(|config| stored(config, false))
            .collect()),
        Some(id) => {
            let id = id.strip_prefix("custom:").unwrap_or(id);
            let config = integrations
                .custom
                .iter()
                .find(|config| config.id == id)
                .ok_or_else(|| {
                    let known: Vec<&str> = integrations
                        .custom
                        .iter()
                        .map(|config| config.id.as_str())
                        .collect();
                    anyhow::anyhow!(
                        "no custom integration '{id}'; the stored ones are: {}",
                        if known.is_empty() {
                            "none".to_string()
                        } else {
                            known.join(", ")
                        }
                    )
                })?;
            Ok(vec![stored(config, true)])
        }
    }
}

/// The secrets stored for the integration `manifest` would replace, when it
/// reaches nowhere that integration's own manifest did not. Anything else
/// gets none: a draft is written by whoever wrote it, and the user stored the
/// secrets for where the old one sent them.
fn carried_secrets(
    manifest: &Manifest,
    integrations: &IntegrationsSettings,
) -> Option<HashMap<String, String>> {
    let config = integrations
        .custom
        .iter()
        .find(|config| config.id == manifest.id)?;
    if config.secrets.is_empty() {
        return None;
    }
    let previous = Manifest::resolve(&config.manifest, integrations)
        .inspect_err(|error| {
            tracing::warn!(
                integration = %config.id,
                "stored manifest could not be read to compare with its replacement: {error:#}"
            );
        })
        .ok()?;
    manifest
        .reaches_no_further_than(&previous.manifest)
        .then(|| config.secrets.clone())
}

/// Run a draft once as [`custom_integration_probe`] does, with the secrets
/// stored for the integration it would replace when [`carried_secrets`]
/// allows them. An agent never sees nor supplies a secret value.
pub async fn agent_probe(
    manifest: String,
    query: String,
    integrations: &IntegrationsSettings,
) -> anyhow::Result<IntegrationProbe> {
    let resolved = Manifest::resolve(&manifest, integrations)?;
    let secrets = carried_secrets(&resolved.manifest, integrations);
    let stored_secrets = secrets.is_some();
    let report =
        custom_integration_probe(manifest, secrets.unwrap_or_default(), query, integrations)
            .await?;
    Ok(IntegrationProbe {
        report,
        stored_secrets,
    })
}

/// What saving a manifest did.
#[derive(Clone, Debug, serde::Serialize)]
pub struct SavedManifest {
    pub provider: String,
    /// Whether no integration had this id before.
    pub added: bool,
    pub enabled: bool,
    /// Required secrets the user has not stored, which they set in Settings
    /// before the provider can send a request.
    pub missing_secrets: Vec<String>,
    /// The integration had secrets, and they were not kept because the new
    /// manifest reaches an origin or program the old one did not.
    pub secrets_cleared: bool,
    pub summary: ManifestSummary,
}

/// `integrations` with `manifest` stored under its own id, replacing the
/// integration of that id or added beside the others.
///
/// A replaced integration keeps the name the user gave it, and keeps its
/// secrets when [`carried_secrets`] allows. `enabled` is kept unless given; a
/// new integration is off unless asked for. Nothing here allows a program: that
/// stays the user's act.
pub fn with_saved_manifest(
    integrations: &IntegrationsSettings,
    manifest: String,
    enabled: Option<bool>,
    allowlist: &ProgramAllowlist,
) -> anyhow::Result<(IntegrationsSettings, SavedManifest)> {
    let summary = custom_integration_summary(manifest.clone(), integrations, allowlist);
    if !summary.problems.is_empty() {
        let mut message = format!(
            "the manifest cannot be saved: {}",
            summary.problems.join("; ")
        );
        if let Some(upgraded) = &summary.upgraded {
            message.push_str(&format!(
                "\nThe same manifest as version 2, which can be saved:\n{upgraded}"
            ));
        }
        anyhow::bail!(message);
    }
    let resolved = Manifest::resolve(&manifest, integrations)?;
    let carried = carried_secrets(&resolved.manifest, integrations);
    let mut next = integrations.clone();
    let existing = next
        .custom
        .iter()
        .position(|config| config.id == summary.id);
    let added = existing.is_none();
    let secrets = carried.unwrap_or_default();
    let missing_secrets = summary
        .required_secrets
        .iter()
        .filter(|name| !secrets.contains_key(*name))
        .cloned()
        .collect();
    let (enabled, secrets_cleared) = match existing {
        Some(index) => {
            let config = &mut next.custom[index];
            let had_secrets = !config.secrets.is_empty();
            config.manifest = manifest;
            config.enabled = enabled.unwrap_or(config.enabled);
            config.secrets = secrets;
            (config.enabled, had_secrets && config.secrets.is_empty())
        }
        None => {
            let enabled = enabled.unwrap_or(false);
            next.custom.push(CustomIntegrationConfig {
                id: summary.id.clone(),
                name: None,
                enabled,
                manifest,
                secrets,
            });
            (enabled, false)
        }
    };
    let saved = SavedManifest {
        provider: format!("custom:{}", summary.id),
        added,
        enabled,
        missing_secrets,
        secrets_cleared,
        summary,
    };
    Ok((next, saved))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wilkes_core::integrations::allowed_programs;

    /// The summary as the application asks for it, with the installation's
    /// own answer to which programs may run.
    fn custom_integration_summary(
        manifest: String,
        integrations: &IntegrationsSettings,
    ) -> ManifestSummary {
        super::custom_integration_summary(manifest, integrations, &allowed_programs(integrations))
    }

    #[test]
    fn summary_names_each_program_and_whether_the_user_allowed_it() {
        let manifest = format!(
            "{MANIFEST}\n[capabilities.resolve_download]\nrun = {{ program = \"mangadex-dl\", args = [\"{{id}}\"] }}\n"
        );
        let mut settings = IntegrationsSettings::default();
        let summary = custom_integration_summary(manifest.clone(), &settings);
        assert!(summary.problems.is_empty(), "{:?}", summary.problems);
        assert!(summary.programs_editable);
        assert_eq!(
            summary.programs,
            vec![ProgramSummary {
                program: "mangadex-dl".to_string(),
                allowed: false,
            }]
        );

        settings.allowed_programs.push("mangadex-dl".to_string());
        let summary = custom_integration_summary(manifest, &settings);
        assert!(summary.programs[0].allowed);
    }

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

    fn stored(manifest: &str, secrets: &[(&str, &str)]) -> IntegrationsSettings {
        let mut settings = IntegrationsSettings::default();
        settings.custom.push(CustomIntegrationConfig {
            id: "crossref".to_string(),
            name: Some("My Crossref".to_string()),
            enabled: true,
            manifest: manifest.to_string(),
            secrets: secrets
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
        });
        settings
    }

    fn save(
        integrations: &IntegrationsSettings,
        manifest: &str,
        enabled: Option<bool>,
    ) -> anyhow::Result<(IntegrationsSettings, SavedManifest)> {
        with_saved_manifest(
            integrations,
            manifest.to_string(),
            enabled,
            &allowed_programs(integrations),
        )
    }

    #[test]
    fn a_saved_manifest_with_a_new_id_is_added_switched_off_and_without_secrets() {
        let (next, saved) = save(&IntegrationsSettings::default(), MANIFEST, None).unwrap();
        assert!(saved.added);
        assert!(!saved.enabled);
        assert_eq!(saved.provider, "custom:crossref");
        assert_eq!(saved.missing_secrets, vec!["crossref_token"]);
        assert_eq!(next.custom.len(), 1);
        assert!(next.custom[0].secrets.is_empty());
        assert_eq!(next.custom[0].manifest, MANIFEST);

        let (_, enabled) = save(&IntegrationsSettings::default(), MANIFEST, Some(true)).unwrap();
        assert!(enabled.enabled);
    }

    #[test]
    fn a_replaced_manifest_keeps_its_name_state_and_secrets_where_it_reaches_no_further() {
        let before = stored(MANIFEST, &[("crossref_token", "t0ken")]);
        let edited = MANIFEST.replace("rows={limit}", "rows={limit}&sort=score");
        let (next, saved) = save(&before, &edited, None).unwrap();
        assert!(!saved.added);
        assert!(saved.enabled);
        assert!(!saved.secrets_cleared);
        assert!(saved.missing_secrets.is_empty());
        assert_eq!(next.custom.len(), 1);
        assert_eq!(next.custom[0].manifest, edited);
        assert_eq!(next.custom[0].name.as_deref(), Some("My Crossref"));
        assert_eq!(next.custom[0].secrets["crossref_token"], "t0ken");

        let (next, saved) = save(&before, MANIFEST, Some(false)).unwrap();
        assert!(!saved.enabled);
        assert!(!next.custom[0].enabled);
    }

    #[test]
    fn a_replaced_manifest_that_reaches_somewhere_new_loses_the_stored_secrets() {
        let before = stored(MANIFEST, &[("crossref_token", "t0ken")]);
        let moved = MANIFEST.replace("api.crossref.org", "collector.test");
        let (next, saved) = save(&before, &moved, None).unwrap();
        assert!(saved.secrets_cleared);
        assert_eq!(saved.missing_secrets, vec!["crossref_token"]);
        assert!(next.custom[0].secrets.is_empty());
    }

    #[test]
    fn an_invalid_manifest_is_refused_with_its_problems_and_nothing_is_stored() {
        let error = save(&IntegrationsSettings::default(), "id = 'nope'", None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("cannot be saved"), "{error}");
    }

    #[test]
    fn stored_integrations_name_secrets_without_their_values() {
        let settings = stored(MANIFEST, &[("crossref_token", "t0ken")]);
        let listed = stored_integrations(&settings, None).unwrap();
        assert_eq!(listed[0].provider, "custom:crossref");
        assert_eq!(listed[0].secrets_set, vec!["crossref_token"]);
        assert!(listed[0].manifest.is_none());
        let one = stored_integrations(&settings, Some("custom:crossref")).unwrap();
        assert_eq!(one[0].manifest.as_deref(), Some(MANIFEST));
        assert!(!serde_json::to_string(&one).unwrap().contains("t0ken"));
        let error = stored_integrations(&settings, Some("other"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("crossref"), "{error}");
    }

    #[tokio::test]
    async fn an_agent_probe_uses_stored_secrets_only_where_the_stored_manifest_sent_them() {
        let settings = stored(MANIFEST, &[("crossref_token", "t0ken")]);
        let moved = MANIFEST.replace("api.crossref.org", "collector.invalid");
        let probe = agent_probe(moved, "q".to_string(), &settings)
            .await
            .unwrap();
        assert!(!probe.stored_secrets);
        // Refused for the missing secret before any request is made.
        assert!(probe
            .report
            .error
            .unwrap()
            .contains("secret 'crossref_token'"));
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
