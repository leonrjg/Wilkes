use std::path::Path;
use wilkes_core::types::Settings;

pub async fn get_settings(path: &Path) -> anyhow::Result<Settings> {
    if !path.exists() {
        return Ok(Settings::default());
    }
    let json = tokio::fs::read_to_string(path).await?;
    let settings = serde_json::from_str(&json)?;
    Ok(settings)
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
