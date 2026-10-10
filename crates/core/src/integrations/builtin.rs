//! The providers Wilkes ships with, as the manifests they are.
//!
//! OpenAlex and Semantic Scholar are not code. Each is a document in
//! `builtin/` that is embedded in the binary and loaded through
//! [`super::custom::CustomSource`], the engine that loads a manifest a user
//! pasted in this morning. What differs is only that a built-in's manifest is
//! not stored in settings (it ships with the build, so it cannot go stale
//! beside it) and that its id is not namespaced.
//!
//! What *is* stored per installation is the user's own part of it
//! ([`crate::types::BuiltinProviderState`]): whether it is switched on, the
//! values for the secrets it names (an email address, an API key), and an
//! optional base URL for a mirror or proxy.

use std::sync::OnceLock;

use super::custom::manifest::Manifest;

/// A shipped manifest and the source it was parsed from.
pub struct BuiltinManifest {
    pub id: &'static str,
    pub source: &'static str,
}

/// In the order the interface lists them, which is also the order a search
/// asks them.
pub const BUILTIN_MANIFESTS: &[BuiltinManifest] = &[
    BuiltinManifest {
        id: "semantic_scholar",
        source: include_str!("builtin/semantic_scholar.toml"),
    },
    BuiltinManifest {
        id: "openalex",
        source: include_str!("builtin/openalex.toml"),
    },
];

/// The shipped manifests, parsed.
///
/// Parsed once and cloned, because a registry is derived from settings on
/// demand and every derivation asks for them.
///
/// A manifest that fails to parse is a defect in this build, not in the user's
/// settings, and `shipped_manifests_are_valid` is the test that keeps it from
/// reaching one; if one ever does, it is named in the log and left out rather
/// than taking the others with it.
pub fn manifests() -> Vec<Manifest> {
    static PARSED: OnceLock<Vec<Manifest>> = OnceLock::new();
    PARSED
        .get_or_init(|| {
            BUILTIN_MANIFESTS
                .iter()
                .filter_map(|builtin| match Manifest::parse(builtin.source) {
                    Ok(manifest) if manifest.id == builtin.id => Some(manifest),
                    Ok(manifest) => {
                        tracing::error!(
                            expected = builtin.id,
                            found = %manifest.id,
                            "built-in manifest declares a different id than it is registered under; skipped"
                        );
                        None
                    }
                    Err(error) => {
                        tracing::error!(
                            integration = builtin.id,
                            "built-in manifest is invalid and was skipped: {error:#}"
                        );
                        None
                    }
                })
                .collect()
        })
        .clone()
}

pub fn is_builtin(id: &str) -> bool {
    BUILTIN_MANIFESTS.iter().any(|builtin| builtin.id == id)
}

/// The order in which providers' records are preferred when several describe
/// one document: the shipped providers first, in the order they are listed,
/// then any other by id.
///
/// A rule that needs only the id, so a place that holds only a provider's
/// records (the metadata cache, a [`crate::types::DocumentMetadata`]) can apply
/// it without the settings that name the user's own providers.
pub fn precedence(id: &str) -> (usize, &str) {
    (
        BUILTIN_MANIFESTS
            .iter()
            .position(|builtin| builtin.id == id)
            .unwrap_or(BUILTIN_MANIFESTS.len()),
        id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One header per map and one line per entry. A header per filter, then a
    /// header per part of a filter, restates the path for every entry; it was
    /// removed once and this is what stops it coming back.
    #[test]
    fn shipped_manifests_write_one_line_per_entry() {
        for builtin in BUILTIN_MANIFESTS {
            assert_eq!(
                crate::integrations::custom::manifest::layout_problems(builtin.source),
                Vec::<String>::new(),
                "{}",
                builtin.id
            );
        }
    }

    #[test]
    fn shipped_manifests_are_valid_and_registered_under_their_own_ids() {
        for builtin in BUILTIN_MANIFESTS {
            let manifest = Manifest::parse(builtin.source)
                .unwrap_or_else(|error| panic!("{} is invalid: {error:#}", builtin.id));
            assert_eq!(manifest.id, builtin.id);
        }
        assert_eq!(manifests().len(), BUILTIN_MANIFESTS.len());
    }

    #[test]
    fn shipped_manifests_name_their_secrets_as_optional() {
        // Both services work anonymously: an unset email or key must not stop
        // a request being sent.
        for manifest in manifests() {
            assert!(
                manifest.required_secrets().is_empty(),
                "{} requires a secret",
                manifest.id
            );
            assert!(!manifest.optional_secrets().is_empty(), "{}", manifest.id);
        }
    }

    #[test]
    fn shipped_providers_precede_the_users_own_and_keep_their_listed_order() {
        let mut ids = vec![
            "custom:crossref",
            "openalex",
            "custom:a",
            "semantic_scholar",
        ];
        ids.sort_by_key(|id| precedence(id));
        assert_eq!(
            ids,
            vec![
                "semantic_scholar",
                "openalex",
                "custom:a",
                "custom:crossref"
            ]
        );
    }

    #[test]
    fn a_built_in_id_is_not_a_custom_id() {
        assert!(is_builtin("openalex"));
        assert!(!is_builtin("custom:openalex"));
    }
}
