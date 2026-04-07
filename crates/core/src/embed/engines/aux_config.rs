use std::collections::HashMap;
use std::path::Path;

/// Accumulated per-model configuration derived from auxiliary HF config files.
#[derive(Default)]
pub struct EmbedderConfig {
    pub query_prefix: String,
    pub passage_prefix: String,
}

pub type AuxParser = (&'static str, fn(&str, &mut EmbedderConfig));

pub const AUX_PARSERS: &[AuxParser] = &[
    ("config_sentence_transformers.json", parse_st_config),
];

fn parse_st_config(content: &str, config: &mut EmbedderConfig) {
    #[derive(serde::Deserialize)]
    struct StConfig {
        prompts: Option<HashMap<String, String>>,
    }

    let Ok(st) = serde_json::from_str::<StConfig>(content) else { return };
    let Some(prompts) = st.prompts else { return };

    for (key, value) in &prompts {
        let k = key.to_lowercase();
        if k.contains("query") {
            config.query_prefix = value.clone();
        } else if k.contains("passage") || k.contains("document") || k.contains("doc") {
            config.passage_prefix = value.clone();
        } else {
            tracing::debug!("Unrecognized prompt key '{key}' in config_sentence_transformers.json — skipping");
        }
    }
}

/// Read auxiliary config files for `model_id` from `cache_root` and return the resulting config.
/// Does not perform any network I/O — call this from `build()` after files are present.
pub fn load_prefixes(cache_root: &Path, model_id: &str) -> EmbedderConfig {
    let mut config = EmbedderConfig::default();
    let cache = hf_hub::Cache::new(cache_root.to_path_buf());
    let repo = cache.repo(hf_hub::Repo::model(model_id.to_string()));

    for (filename, parser) in AUX_PARSERS {
        if let Some(path) = repo.get(filename) {
            match std::fs::read_to_string(&path) {
                Ok(content) => parser(&content, &mut config),
                Err(e) => tracing::debug!("Failed to read {filename} for {model_id}: {e}"),
            }
        }
    }

    if config.query_prefix.is_empty() {
        tracing::debug!("No prefix config found for {model_id} — prefixes will not be applied");
    }

    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_parse_st_config() {
        let content = r#"{
            "prompts": {
                "query": "query: ",
                "passage": "passage: ",
                "other": "ignored"
            }
        }"#;
        let mut config = EmbedderConfig::default();
        parse_st_config(content, &mut config);
        assert_eq!(config.query_prefix, "query: ");
        assert_eq!(config.passage_prefix, "passage: ");
    }

    #[test]
    fn test_parse_st_config_alt_keys() {
        let content = r#"{
            "prompts": {
                "search_query": "q:",
                "doc": "d:"
            }
        }"#;
        let mut config = EmbedderConfig::default();
        parse_st_config(content, &mut config);
        assert_eq!(config.query_prefix, "q:");
        assert_eq!(config.passage_prefix, "d:");
    }

    #[test]
    fn test_embedder_config_default() {
        let config = EmbedderConfig::default();
        assert!(config.query_prefix.is_empty());
        assert!(config.passage_prefix.is_empty());
    }

    #[test]
    fn test_fetch_aux_configs_invalid_path() {
        // Should not panic, just log debug
        fetch_aux_configs(Path::new("/non/existent/path/12345"), "test/model");
    }
}

/// Download all auxiliary config files for `model_id` into `cache_dir`.
/// Best-effort: individual failures are logged at debug level and never propagate.
pub fn fetch_aux_configs(cache_dir: &Path, model_id: &str) {
    let api = match hf_hub::api::sync::ApiBuilder::new()
        .with_cache_dir(cache_dir.to_path_buf())
        .build()
    {
        Ok(a) => a,
        Err(e) => {
            tracing::debug!("Could not initialise HF API for aux config fetch of {model_id}: {e}");
            return;
        }
    };
    let repo = api.model(model_id.to_string());
    for (filename, _) in AUX_PARSERS {
        if let Err(e) = repo.get(filename) {
            tracing::debug!("Could not fetch {filename} for {model_id}: {e}");
        }
    }
}
