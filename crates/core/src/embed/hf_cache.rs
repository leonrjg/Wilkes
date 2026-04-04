use std::path::{Path, PathBuf};
use std::fs;
use crate::types::ModelDescriptor;

pub fn get_hf_cache_root() -> PathBuf {
    if let Ok(hf_home) = std::env::var("HF_HOME") {
        PathBuf::from(hf_home).join("hub")
    } else {
        match dirs::home_dir() {
            Some(home) => home.join(".cache").join("huggingface").join("hub"),
            None => PathBuf::new(),
        }
    }
}

pub fn list_cached_models() -> Vec<ModelDescriptor> {
    let mut models = Vec::new();
    let root = get_hf_cache_root();
    if !root.exists() {
        return models;
    }

    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Models are stored as "models--org--repo"
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("models--") {
                    let parts: Vec<&str> = name.split("--").collect();
                    if parts.len() >= 3 {
                        let org = parts[1];
                        let repo = parts[2..].join("/");
                        let model_id = format!("{}/{}", org, repo);
                        if let Some(desc) = get_model_descriptor_from_path(&path, &model_id) {
                            models.push(desc);
                        }
                    }
                }
            }
        }
    }
    models
}

pub fn get_model_descriptor_from_path(model_dir: &Path, model_id: &str) -> Option<ModelDescriptor> {
    let snapshots_dir = model_dir.join("snapshots");
    if !snapshots_dir.exists() {
        return None;
    }

    // Use the first snapshot we find.
    let snapshot_path = fs::read_dir(snapshots_dir)
        .ok()?
        .flatten()
        .find(|e| e.path().is_dir())?
        .path();

    let config_path = snapshot_path.join("config.json");
    if !config_path.exists() {
        return None;
    }

    let config_content = fs::read_to_string(&config_path).ok()?;
    let config: serde_json::Value = serde_json::from_str(&config_content).ok()?;

    // Extract dimension from config.json.
    // Try common keys used by embedding models.
    let mut dimension = config
        .get("hidden_size")
        .and_then(|v| v.as_u64())
        .or_else(|| config.get("dim").and_then(|v| v.as_u64()))
        .or_else(|| config.get("d_model").and_then(|v| v.as_u64()));

    // Fallback: check sentence_bert_config.json if config.json didn't have it.
    if dimension.is_none() {
        let sbert_config_path = snapshot_path.join("sentence_bert_config.json");
        if let Ok(sbert_content) = fs::read_to_string(sbert_config_path) {
            if let Ok(sbert_config) = serde_json::from_str::<serde_json::Value>(&sbert_content) {
                dimension = sbert_config.get("dimension").and_then(|v| v.as_u64());
            }
        }
    }

    let dimension = dimension? as usize;

    let size_bytes = calculate_dir_size(&snapshot_path);

    Some(ModelDescriptor {
        model_id: model_id.to_string(),
        display_name: model_id.split('/').last().unwrap_or(model_id).to_string(),
        description: format!("Locally cached Python model ({} dimensions)", dimension),
        dimension,
        is_cached: true,
        // Flags are handled by the caller (built-in vs custom)
        is_default: false,
        is_recommended: false,
        size_bytes: Some(size_bytes),
        preferred_batch_size: Some(32),
    })
}

fn calculate_dir_size(path: &Path) -> u64 {
    let mut total_size = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                total_size += p.metadata().map(|m| m.len()).unwrap_or(0);
            } else if p.is_dir() {
                total_size += calculate_dir_size(&p);
            }
        }
    }
    total_size
}
