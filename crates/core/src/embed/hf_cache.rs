use std::path::{Path, PathBuf};
use std::fs;
use crate::types::ModelDescriptor;

/// Scan the HuggingFace cache directory to discover cached models.
/// This implementation follows the standard HF hub cache layout:
/// ~/.cache/huggingface/hub/
///   models--<org>--<name>/
///     snapshots/
///       <hash>/
///         config.json
///         tokenizer_config.json
///         pytorch_model.bin / model.safetensors
///         ...
pub fn list_cached_models() -> Vec<ModelDescriptor> {
    let cache_root = get_hf_cache_root();
    if !cache_root.exists() {
        return vec![];
    }

    let mut models = Vec::new();
    if let Ok(entries) = fs::read_dir(cache_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let folder_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) if name.starts_with("models--") => name,
                _ => continue,
            };

            // models--org--name -> org/name
            // models--name -> name
            let model_id_encoded = &folder_name[8..];
            let model_id = if let Some(pos) = model_id_encoded.find("--") {
                let (org, name) = model_id_encoded.split_at(pos);
                format!("{}/{}", org, &name[2..])
            } else {
                model_id_encoded.to_string()
            };

            if let Some(desc) = get_model_descriptor(&path, &model_id) {
                models.push(desc);
            }
        }
    }

    // Sort by model_id for consistent output
    models.sort_by(|a, b| a.model_id.cmp(&b.model_id));
    models
}

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

fn get_model_descriptor(model_dir: &Path, model_id: &str) -> Option<ModelDescriptor> {
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
    let dimension = config
        .get("hidden_size")
        .and_then(|v| v.as_u64())
        .or_else(|| config.get("dim").and_then(|v| v.as_u64()))
        .or_else(|| config.get("d_model").and_then(|v| v.as_u64()))
        .unwrap_or(768) as usize; // Default to 768 if not found

    let size_bytes = calculate_dir_size(&snapshot_path);

    Some(ModelDescriptor {
        model_id: model_id.to_string(),
        display_name: model_id.split('/').last().unwrap_or(model_id).to_string(),
        description: format!("Locally cached Python model ({} dimensions)", dimension),
        dimension,
        is_cached: true,
        is_default: model_id == "BAAI/bge-base-en-v1.5",
        is_recommended: model_id == "BAAI/bge-base-en-v1.5" || model_id == "sentence-transformers/all-MiniLM-L6-v2",
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
