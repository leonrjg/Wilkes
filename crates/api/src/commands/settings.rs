use std::path::Path;
use wilkes_core::types::Settings;

use crate::workspace::{read_manifest, update_manifest};

pub async fn get_settings(path: &Path) -> anyhow::Result<Settings> {
    if !path.exists() {
        return Ok(Settings::default());
    }
    let json = tokio::fs::read_to_string(path).await?;
    let mut settings: Settings = serde_json::from_str(&json)?;
    canonicalize_configured_roots(&mut settings);
    Ok(settings)
}

/// Resolve aliases at the settings boundary so every consumer compares the
/// same spelling of a configured root. Nested roots remain separate: only
/// duplicate aliases within each persisted list are removed.
fn canonicalize_configured_roots(settings: &mut Settings) {
    if let Some(root) = settings.last_directory.as_mut() {
        if let Ok(canonical) = std::fs::canonicalize(&*root) {
            *root = canonical;
        }
    }
    canonicalize_root_list(&mut settings.favorites);
    canonicalize_root_list(&mut settings.recent_dirs);
}

fn canonicalize_root_list(roots: &mut Vec<std::path::PathBuf>) {
    let mut canonical = Vec::with_capacity(roots.len());
    for root in roots.drain(..) {
        let resolved = std::fs::canonicalize(&root).unwrap_or(root);
        if !canonical.contains(&resolved) {
            canonical.push(resolved);
        }
    }
    *roots = canonical;
}

pub async fn update_settings(path: &Path, patch: serde_json::Value) -> anyhow::Result<Settings> {
    let mut current = get_settings(path).await?;

    // Merge patch fields into current settings via round-trip through JSON.
    let mut current_json = serde_json::to_value(&current)?;
    if let (Some(obj), Some(patch_obj)) = (current_json.as_object_mut(), patch.as_object()) {
        for (k, v) in patch_obj {
            obj.insert(k.clone(), v.clone());
        }
    }
    current = serde_json::from_value(current_json)?;
    validate_custom_integrations(&current)?;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, serde_json::to_string_pretty(&current)?).await?;

    Ok(current)
}

/// Refuse to persist a custom integration that cannot be loaded back.
///
/// Here rather than in the command that edits one, because `update_settings`
/// is the only way anything reaches the settings file: a check anywhere else
/// would be a check one caller could go around, and the registry would then be
/// dropping providers at load time for a mistake nobody was told about.
fn validate_custom_integrations(settings: &Settings) -> anyhow::Result<()> {
    let mut seen: Vec<&str> = Vec::new();
    for config in &settings.integrations.custom {
        wilkes_core::integrations::custom::CustomSource::from_config(config).map_err(|error| {
            anyhow::anyhow!(
                "custom integration '{}' cannot be saved: {error}",
                config.id
            )
        })?;
        anyhow::ensure!(
            !seen.contains(&config.id.as_str()),
            "two custom integrations share the id '{}'",
            config.id
        );
        seen.push(&config.id);
    }
    Ok(())
}

/// Read global preferences and overlay the roots owned by one workspace.
/// When both paths are identical this preserves the original single-file
/// behavior used by focused AppContext tests.
pub async fn get_scoped_settings(
    global_path: &Path,
    workspace_path: &Path,
) -> anyhow::Result<Settings> {
    let mut settings = get_settings(global_path).await?;
    if global_path == workspace_path {
        return Ok(settings);
    }
    let manifest = read_manifest(workspace_path)?;
    settings.favorites = manifest.favorites;
    settings.recent_dirs = manifest.recent_roots;
    settings.last_directory = manifest.active_root;
    if let Some(semantic) = manifest.semantic {
        settings.semantic = semantic;
    }
    canonicalize_configured_roots(&mut settings);
    Ok(settings)
}

/// Persist root fields only in the active workspace manifest and every other
/// setting only in the global settings file. There is no shadow copy of roots.
pub async fn update_scoped_settings(
    global_path: &Path,
    workspace_path: &Path,
    patch: serde_json::Value,
) -> anyhow::Result<Settings> {
    if global_path == workspace_path {
        return update_settings(global_path, patch).await;
    }

    let manifest = read_manifest(workspace_path)?;
    if manifest.is_application_managed() {
        let changes_managed_configuration = patch.as_object().is_some_and(|object| {
            [
                "favorites",
                "bookmarked_dirs",
                "recent_dirs",
                "last_directory",
                "semantic",
            ]
            .iter()
            .any(|key| object.contains_key(*key))
        });
        anyhow::ensure!(
            !changes_managed_configuration,
            "MANAGED_WORKSPACE_PROTECTED: roots and semantic configuration are immutable"
        );
    }

    // Validate and normalize workspace-owned values before writing either
    // file, so a malformed mixed patch cannot partially update global prefs.
    let object = patch.as_object();
    let mut favorites = object
        .and_then(|object| {
            object
                .get("favorites")
                .or_else(|| object.get("bookmarked_dirs"))
        })
        .map(|value| serde_json::from_value::<Vec<std::path::PathBuf>>(value.clone()))
        .transpose()?;
    let mut recent_roots = object
        .and_then(|object| object.get("recent_dirs"))
        .map(|value| serde_json::from_value::<Vec<std::path::PathBuf>>(value.clone()))
        .transpose()?;
    let mut active_root = object
        .and_then(|object| object.get("last_directory"))
        .map(|value| serde_json::from_value::<Option<std::path::PathBuf>>(value.clone()))
        .transpose()?;
    let semantic = object
        .and_then(|object| object.get("semantic"))
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()?;
    if let Some(roots) = favorites.as_mut() {
        canonicalize_root_list(roots);
    }
    if let Some(roots) = recent_roots.as_mut() {
        canonicalize_root_list(roots);
    }
    if let Some(Some(root)) = active_root.as_mut() {
        if let Ok(canonical) = std::fs::canonicalize(&*root) {
            *root = canonical;
        }
    }

    let mut global_patch = patch.clone();
    if let Some(object) = global_patch.as_object_mut() {
        object.remove("favorites");
        object.remove("bookmarked_dirs");
        object.remove("recent_dirs");
        object.remove("last_directory");
        object.remove("semantic");
    }
    if global_patch
        .as_object()
        .is_some_and(|object| !object.is_empty())
    {
        update_settings(global_path, global_patch).await?;
    }
    if global_path.exists() {
        let mut global: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(global_path).await?)?;
        let mut changed = false;
        if let Some(object) = global.as_object_mut() {
            for key in [
                "favorites",
                "bookmarked_dirs",
                "recent_dirs",
                "last_directory",
                "semantic",
            ] {
                changed |= object.remove(key).is_some();
            }
        }
        if changed {
            tokio::fs::write(global_path, serde_json::to_string_pretty(&global)?).await?;
        }
    }

    if favorites.is_some() || recent_roots.is_some() || active_root.is_some() || semantic.is_some()
    {
        update_manifest(workspace_path, |manifest| {
            if let Some(value) = favorites {
                manifest.favorites = value;
            }
            if let Some(value) = recent_roots {
                manifest.recent_roots = value;
            }
            if let Some(value) = active_root {
                manifest.active_root = value;
            }
            if let Some(value) = semantic {
                manifest.semantic = Some(value);
            }
            Ok(())
        })?;
    }

    get_scoped_settings(global_path, workspace_path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use wilkes_core::types::{FileDisplayField, FileSortDirection, FileSortKey};

    const VALID_MANIFEST: &str = r#"
manifest_version = 1
id = "crossref"
name = "Crossref"
[http]
base_url = "https://api.crossref.org"
[capabilities.search]
path = "/works?query.bibliographic={query}&rows={limit}"
items = "message.items[*]"
[capabilities.search.fields]
id = "DOI"
title = "title[0]"
"#;

    fn custom_patch(id: &str, manifest: &str) -> serde_json::Value {
        serde_json::json!({
            "integrations": {
                "custom": [{"id": id, "enabled": true, "manifest": manifest, "secrets": {}}]
            }
        })
    }

    #[tokio::test]
    async fn a_valid_custom_integration_round_trips_through_the_settings_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let saved = update_settings(&path, custom_patch("crossref", VALID_MANIFEST))
            .await
            .unwrap();
        assert_eq!(saved.integrations.custom.len(), 1);

        let reread = get_settings(&path).await.unwrap();
        assert_eq!(reread.integrations.custom[0].id, "crossref");
        assert!(reread.integrations.custom[0].enabled);
    }

    /// Nothing invalid may reach the file. The registry drops a manifest it
    /// cannot load, so a save that let one through would produce a provider
    /// that is configured, enabled, and absent.
    #[tokio::test]
    async fn an_invalid_custom_integration_is_refused_and_nothing_is_written() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let broken = VALID_MANIFEST.replace(r#"title = "title[0]""#, r#"titel = "title[0]""#);
        let error = update_settings(&path, custom_patch("crossref", &broken))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("cannot be saved"), "{error}");
        assert!(!path.exists(), "a refused save must not write the file");
    }

    #[tokio::test]
    async fn a_stored_id_that_disagrees_with_the_manifest_is_refused() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let error = update_settings(&path, custom_patch("elsewhere", VALID_MANIFEST))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("does not match"), "{error}");
    }

    #[tokio::test]
    async fn two_custom_integrations_may_not_share_an_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let one = serde_json::json!({"id": "crossref", "enabled": true, "manifest": VALID_MANIFEST, "secrets": {}});

        let error = update_settings(
            &path,
            serde_json::json!({"integrations": {"custom": [one.clone(), one]}}),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("share the id"), "{error}");
    }

    #[tokio::test]
    async fn test_get_settings_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let settings = get_settings(&path).await.unwrap();
        assert_eq!(settings.respect_gitignore, true);
        assert_eq!(settings.context_lines, 2);
        assert_eq!(settings.file_sort_key, FileSortKey::Filename);
        assert_eq!(settings.file_sort_direction, FileSortDirection::Asc);
        assert!(!settings.file_tree_enabled);
        assert_eq!(settings.chat_custom_instructions, "");
        assert!(!settings.external_mcp.enabled);
        assert!(!settings.external_mcp.require_token);
        assert_eq!(
            settings.external_mcp.bind_address,
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        );
        assert!(settings.external_mcp.port > 0);
    }

    #[tokio::test]
    async fn test_update_settings_accepts_publication_sort_and_display_fields() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let patch = serde_json::json!({
            "file_sort_key": "publication",
            "file_display_fields": ["publication", "size"],
            "file_tree_enabled": true
        });

        let updated = update_settings(&path, patch).await.unwrap();
        assert_eq!(updated.file_sort_key, FileSortKey::Publication);
        assert_eq!(
            updated.file_display_fields,
            vec![FileDisplayField::Publication, FileDisplayField::Size]
        );
        assert!(updated.file_tree_enabled);

        let loaded = get_settings(&path).await.unwrap();
        assert_eq!(loaded.file_sort_key, FileSortKey::Publication);
        assert_eq!(
            loaded.file_display_fields,
            vec![FileDisplayField::Publication, FileDisplayField::Size]
        );
        assert!(loaded.file_tree_enabled);
    }

    #[tokio::test]
    async fn test_update_settings() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let patch = serde_json::json!({
            "context_lines": 5,
            "respect_gitignore": false,
            "file_sort_key": "modified",
            "file_sort_direction": "desc"
        });

        let updated = update_settings(&path, patch).await.unwrap();
        assert_eq!(updated.context_lines, 5);
        assert_eq!(updated.respect_gitignore, false);
        assert_eq!(updated.file_sort_key, FileSortKey::Modified);
        assert_eq!(updated.file_sort_direction, FileSortDirection::Desc);

        // Verify it was persisted
        let loaded = get_settings(&path).await.unwrap();
        assert_eq!(loaded.context_lines, 5);
        assert_eq!(loaded.respect_gitignore, false);
        assert_eq!(loaded.file_sort_key, FileSortKey::Modified);
        assert_eq!(loaded.file_sort_direction, FileSortDirection::Desc);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn get_settings_canonicalizes_root_aliases_without_collapsing_nested_roots() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let parent = dir.path().join("parent");
        let child = parent.join("child");
        let alias = dir.path().join("alias");
        std::fs::create_dir_all(&child).unwrap();
        symlink(&parent, &alias).unwrap();

        let path = dir.path().join("settings.json");
        let mut settings = Settings::default();
        settings.last_directory = Some(parent.clone());
        settings.favorites = vec![alias.join("child"), child.clone()];
        tokio::fs::write(&path, serde_json::to_string(&settings).unwrap())
            .await
            .unwrap();

        let loaded = get_settings(&path).await.unwrap();

        assert_eq!(loaded.last_directory, Some(parent.canonicalize().unwrap()));
        assert_eq!(loaded.favorites, vec![child.canonicalize().unwrap()]);
    }

    #[tokio::test]
    async fn test_update_semantic_settings() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let patch = serde_json::json!({
            "semantic": {
                "enabled": true,
                "chunk_size": 1000
            }
        });

        let updated = update_settings(&path, patch).await.unwrap();
        assert_eq!(updated.semantic.enabled, true);
        assert_eq!(updated.semantic.chunk_size, 1000);
    }

    #[tokio::test]
    async fn test_update_chat_custom_instructions_persists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let instructions = "Use concise answers and cite document pages.";

        let updated = update_settings(
            &path,
            serde_json::json!({ "chat_custom_instructions": instructions }),
        )
        .await
        .unwrap();

        assert_eq!(updated.chat_custom_instructions, instructions);
        assert_eq!(
            get_settings(&path).await.unwrap().chat_custom_instructions,
            instructions
        );
    }

    #[tokio::test]
    async fn scoped_settings_keep_roots_and_semantic_state_out_of_global_preferences() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("settings.json");
        let active = crate::workspace::initialize_workspace_registry(dir.path(), &global).unwrap();
        let manifest = crate::workspace::workspace_manifest_path(dir.path(), &active);
        let root = dir.path().join("library");
        std::fs::create_dir_all(&root).unwrap();
        let mut semantic = serde_json::to_value(Settings::default().semantic).unwrap();
        semantic["chunk_size"] = serde_json::json!(500);

        let updated = update_scoped_settings(
            &global,
            &manifest,
            serde_json::json!({
                "theme": "Dark",
                "favorites": [root.clone()],
                "last_directory": root.clone(),
                "semantic": semantic,
            }),
        )
        .await
        .unwrap();

        assert_eq!(updated.last_directory, Some(root.canonicalize().unwrap()));
        assert_eq!(updated.semantic.chunk_size, 500);
        let global_json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(global).unwrap()).unwrap();
        assert_eq!(global_json["theme"], "Dark");
        assert!(global_json.get("favorites").is_none());
        assert!(global_json.get("last_directory").is_none());
        assert!(global_json.get("semantic").is_none());
    }
}
