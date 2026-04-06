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

    #[tokio::test]
    async fn test_get_settings_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let settings = get_settings(&path).await.unwrap();
        assert_eq!(settings.respect_gitignore, true);
        assert_eq!(settings.context_lines, 2);
    }

    #[tokio::test]
    async fn test_update_settings() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let patch = serde_json::json!({
            "context_lines": 5,
            "respect_gitignore": false
        });

        let updated = update_settings(&path, patch).await.unwrap();
        assert_eq!(updated.context_lines, 5);
        assert_eq!(updated.respect_gitignore, false);

        // Verify it was persisted
        let loaded = get_settings(&path).await.unwrap();
        assert_eq!(loaded.context_lines, 5);
        assert_eq!(loaded.respect_gitignore, false);
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
}
