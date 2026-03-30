use wilkes_core::types::Settings;

fn settings_path() -> anyhow::Result<std::path::PathBuf> {
    let config = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?;
    Ok(config.join("wilkes").join("settings.json"))
}

pub async fn get_settings() -> anyhow::Result<Settings> {
    let path = settings_path()?;
    if !path.exists() {
        return Ok(Settings::default());
    }
    let json = tokio::fs::read_to_string(&path).await?;
    let settings = serde_json::from_str(&json)?;
    Ok(settings)
}

pub async fn update_settings(patch: serde_json::Value) -> anyhow::Result<Settings> {
    let mut current = get_settings().await?;

    // Merge patch fields into current settings via round-trip through JSON.
    let mut current_json = serde_json::to_value(&current)?;
    if let (Some(obj), Some(patch_obj)) = (current_json.as_object_mut(), patch.as_object()) {
        for (k, v) in patch_obj {
            obj.insert(k.clone(), v.clone());
        }
    }
    current = serde_json::from_value(current_json)?;

    let path = settings_path()?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, serde_json::to_string_pretty(&current)?).await?;

    Ok(current)
}
