use std::path::Path;
use wilkes_core::types::Settings;

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

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, serde_json::to_string_pretty(&current)?).await?;

    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use wilkes_core::types::{FileDisplayField, FileSortDirection, FileSortKey};

    #[tokio::test]
    async fn test_get_settings_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let settings = get_settings(&path).await.unwrap();
        assert_eq!(settings.respect_gitignore, true);
        assert_eq!(settings.context_lines, 2);
        assert_eq!(settings.file_sort_key, FileSortKey::Filename);
        assert_eq!(settings.file_sort_direction, FileSortDirection::Asc);
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
            "file_display_fields": ["publication", "size"]
        });

        let updated = update_settings(&path, patch).await.unwrap();
        assert_eq!(updated.file_sort_key, FileSortKey::Publication);
        assert_eq!(
            updated.file_display_fields,
            vec![FileDisplayField::Publication, FileDisplayField::Size]
        );

        let loaded = get_settings(&path).await.unwrap();
        assert_eq!(loaded.file_sort_key, FileSortKey::Publication);
        assert_eq!(
            loaded.file_display_fields,
            vec![FileDisplayField::Publication, FileDisplayField::Size]
        );
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
}
