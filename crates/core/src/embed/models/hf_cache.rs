use std::path::{Path, PathBuf};
use std::collections::HashMap;
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

/// Overlay models found in the global HF cache onto `models`.
/// For models already in the map, marks them `is_cached = true` and fills in `size_bytes`.
/// Models found in the cache but not yet in the map are added as-is.
pub fn overlay_hf_cache(models: &mut HashMap<String, ModelDescriptor>) {
    let root = get_hf_cache_root();
    if !root.exists() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let folder_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) if name.starts_with("models--") => name.to_string(),
            _ => continue,
        };
        let encoded = &folder_name[8..];
        let model_id = if let Some(pos) = encoded.find("--") {
            let (org, name) = encoded.split_at(pos);
            format!("{}/{}", org, &name[2..])
        } else {
            encoded.to_string()
        };
        if let Some(desc) = get_model_descriptor_from_path(&path, &model_id) {
            models.entry(model_id)
                .and_modify(|e| {
                    e.is_cached = true;
                    if e.size_bytes.is_none() {
                        e.size_bytes = desc.size_bytes;
                    }
                })
                .or_insert(desc);
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs;

    #[test]
    fn test_calculate_dir_size() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        
        fs::write(root.join("f1.txt"), "abc").unwrap(); // 3 bytes
        fs::write(root.join("f2.txt"), "de").unwrap();  // 2 bytes
        
        let sub = root.join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("f3.txt"), "f").unwrap();    // 1 byte
        
        assert_eq!(calculate_dir_size(root), 6);
    }

    #[test]
    fn test_get_model_descriptor_from_path() {
        let dir = tempdir().unwrap();
        let model_dir = dir.path();
        let snapshots = model_dir.join("snapshots");
        let snapshot = snapshots.join("12345");
        fs::create_dir_all(&snapshot).unwrap();
        
        fs::write(snapshot.join("config.json"), r#"{"hidden_size": 384}"#).unwrap();
        fs::write(snapshot.join("model.bin"), "fake weights").unwrap();

        let desc = get_model_descriptor_from_path(model_dir, "org/repo").unwrap();
        assert_eq!(desc.model_id, "org/repo");
        assert_eq!(desc.dimension, 384);
        assert!(desc.size_bytes.unwrap() > 0);
    }

    #[test]
    fn test_overlay_and_list_cached_models() {
        let dir = tempdir().unwrap();
        let hf_home = dir.path().to_path_buf();
        std::env::set_var("HF_HOME", hf_home.to_str().unwrap());

        let hub_dir = hf_home.join("hub");
        let model_dir = hub_dir.join("models--org--repo");
        fs::create_dir_all(&model_dir).unwrap();

        let snapshots = model_dir.join("snapshots");
        let snapshot = snapshots.join("abc");
        fs::create_dir_all(&snapshot).unwrap();
        fs::write(snapshot.join("config.json"), r#"{"hidden_size": 768}"#).unwrap();

        let cached = list_cached_models();
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].model_id, "org/repo");
        assert_eq!(cached[0].dimension, 768);

        let mut models = HashMap::new();
        models.insert("org/repo".to_string(), ModelDescriptor {
            model_id: "org/repo".to_string(),
            display_name: "repo".to_string(),
            description: "test".to_string(),
            dimension: 768,
            is_cached: false,
            is_default: false,
            is_recommended: false,
            size_bytes: None,
            preferred_batch_size: None,
        });

        overlay_hf_cache(&mut models);
        let m = models.get("org/repo").unwrap();
        assert!(m.is_cached);
        assert!(m.size_bytes.is_some());

        std::env::remove_var("HF_HOME");
    }
}
