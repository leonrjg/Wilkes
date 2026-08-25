use std::collections::HashMap;
use std::path::Path;

/// Accumulated per-model configuration derived from auxiliary HF config files.
#[derive(Default)]
pub struct EmbedderConfig {
    pub query_prefix: String,
    pub passage_prefix: String,
}

pub type AuxParser = (&'static str, fn(&str, &mut EmbedderConfig));

pub const AUX_PARSERS: &[AuxParser] = &[("config_sentence_transformers.json", parse_st_config)];

/// Prefixes for models that document their retrieval convention in the model
/// card rather than in `config_sentence_transformers.json`.
///
/// Most retrieval-trained models do not ship a `prompts` map — of the ten
/// checked on 2026-08-25, three did — so discovery finds nothing and an
/// asymmetric model silently embeds a query as though it were a passage.
/// Underdog measured what that costs on a real corpus: the same model over the
/// same 6,600 records puts the right answer at rank 52 with its query prefix
/// applied and rank 1792 without it, while every similarity *rises*, so the
/// space looks more confident exactly where it has gone blind
/// (ACQUISITION §12i).
///
/// **Discovery stays primary.** This table is consulted only for a prefix the
/// parsed config did not supply, so a model that ships its prompts keeps
/// using its own, and a model nobody here has labelled behaves exactly as it
/// did before this table existed. Keyed by HuggingFace repository id,
/// including the ONNX mirrors, because a mirror rarely carries the original's
/// auxiliary configs.
const CURATED_PREFIXES: &[(&str, &str, &str)] = &[
    // BGE English v1.5 — query-side only, per the model card.
    ("BAAI/bge-small-en-v1.5", BGE_QUERY, ""),
    ("BAAI/bge-base-en-v1.5", BGE_QUERY, ""),
    ("BAAI/bge-large-en-v1.5", BGE_QUERY, ""),
    ("Xenova/bge-small-en-v1.5", BGE_QUERY, ""),
    ("Xenova/bge-base-en-v1.5", BGE_QUERY, ""),
    ("Xenova/bge-large-en-v1.5", BGE_QUERY, ""),
    ("Qdrant/bge-small-en-v1.5-onnx-Q", BGE_QUERY, ""),
    ("Qdrant/bge-base-en-v1.5-onnx-Q", BGE_QUERY, ""),
    ("Qdrant/bge-large-en-v1.5-onnx-Q", BGE_QUERY, ""),
    // BGE Chinese v1.5 — the same convention, in Chinese.
    ("Xenova/bge-small-zh-v1.5", BGE_QUERY_ZH, ""),
    ("Xenova/bge-large-zh-v1.5", BGE_QUERY_ZH, ""),
    // E5 — both sides, and the passage prefix is not optional for this family.
    ("intfloat/e5-small-v2", "query: ", "passage: "),
    ("intfloat/e5-base-v2", "query: ", "passage: "),
    ("intfloat/e5-large-v2", "query: ", "passage: "),
    ("intfloat/multilingual-e5-small", "query: ", "passage: "),
    ("intfloat/multilingual-e5-base", "query: ", "passage: "),
    ("intfloat/multilingual-e5-large", "query: ", "passage: "),
    ("Qdrant/multilingual-e5-large-onnx", "query: ", "passage: "),
    // Nomic — task prefixes rather than role prefixes; these are the two the
    // retrieval task uses.
    (
        "nomic-ai/nomic-embed-text-v1",
        "search_query: ",
        "search_document: ",
    ),
    (
        "nomic-ai/nomic-embed-text-v1.5",
        "search_query: ",
        "search_document: ",
    ),
    // Arctic and mxbai ship their prompts, so these entries only cover the
    // mirrors that republish the weights without the config.
    ("Snowflake/snowflake-arctic-embed-xs", BGE_QUERY, ""),
    ("Snowflake/snowflake-arctic-embed-s", BGE_QUERY, ""),
    ("Snowflake/snowflake-arctic-embed-m", BGE_QUERY, ""),
    ("Snowflake/snowflake-arctic-embed-l", BGE_QUERY, ""),
    ("snowflake/snowflake-arctic-embed-xs", BGE_QUERY, ""),
    ("snowflake/snowflake-arctic-embed-s", BGE_QUERY, ""),
    ("snowflake/snowflake-arctic-embed-m-long", BGE_QUERY, ""),
    ("snowflake/snowflake-arctic-embed-l", BGE_QUERY, ""),
    ("mixedbread-ai/mxbai-embed-large-v1", BGE_QUERY, ""),
];

/// Shared by the BGE, arctic and mxbai families, which all inherit it from
/// BGE's training recipe.
const BGE_QUERY: &str = "Represent this sentence for searching relevant passages: ";
const BGE_QUERY_ZH: &str = "为这个句子生成表示以用于检索相关文章：";

fn parse_st_config(content: &str, config: &mut EmbedderConfig) {
    #[derive(serde::Deserialize)]
    struct StConfig {
        prompts: Option<HashMap<String, String>>,
    }

    let Ok(st) = serde_json::from_str::<StConfig>(content) else {
        return;
    };
    let Some(prompts) = st.prompts else { return };

    for (key, value) in &prompts {
        let k = key.to_lowercase();
        if k.contains("query") {
            config.query_prefix = value.clone();
        } else if k.contains("passage") || k.contains("document") || k.contains("doc") {
            config.passage_prefix = value.clone();
        } else {
            tracing::debug!(
                "Unrecognized prompt key '{key}' in config_sentence_transformers.json — skipping"
            );
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

    apply_curated_prefixes(model_id, &mut config);

    if config.query_prefix.is_empty() {
        tracing::debug!("No prefix config found for {model_id} — prefixes will not be applied");
    }

    config
}

/// Fill in prefixes the parsed config did not supply, for models this table
/// knows. Never overrides what discovery found.
fn apply_curated_prefixes(model_id: &str, config: &mut EmbedderConfig) {
    let Some((_, query, passage)) = CURATED_PREFIXES
        .iter()
        .find(|(candidate, _, _)| *candidate == model_id)
    else {
        return;
    };
    if config.query_prefix.is_empty() && !query.is_empty() {
        tracing::debug!("Using curated query prefix for {model_id}");
        config.query_prefix = (*query).to_string();
    }
    if config.passage_prefix.is_empty() && !passage.is_empty() {
        tracing::debug!("Using curated passage prefix for {model_id}");
        config.passage_prefix = (*passage).to_string();
    }
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
    fn test_curated_prefix_fills_what_the_config_omits() {
        let mut config = EmbedderConfig::default();
        apply_curated_prefixes("Xenova/bge-base-en-v1.5", &mut config);
        assert_eq!(config.query_prefix, BGE_QUERY);
        assert!(
            config.passage_prefix.is_empty(),
            "BGE prefixes the query only"
        );

        let mut e5 = EmbedderConfig::default();
        apply_curated_prefixes("intfloat/multilingual-e5-small", &mut e5);
        assert_eq!(e5.query_prefix, "query: ");
        assert_eq!(e5.passage_prefix, "passage: ");
    }

    #[test]
    fn test_curated_prefix_never_overrides_the_model_s_own_prompts() {
        let mut config = EmbedderConfig {
            query_prefix: "from the config: ".to_string(),
            passage_prefix: String::new(),
        };
        apply_curated_prefixes("Snowflake/snowflake-arctic-embed-m", &mut config);
        assert_eq!(
            config.query_prefix, "from the config: ",
            "discovery stays primary"
        );
    }

    #[test]
    fn test_unlabelled_model_is_untouched() {
        let mut config = EmbedderConfig::default();
        apply_curated_prefixes("some-org/a-model-nobody-here-has-labelled", &mut config);
        assert!(config.query_prefix.is_empty());
        assert!(config.passage_prefix.is_empty());

        let mut mini = EmbedderConfig::default();
        apply_curated_prefixes("Qdrant/all-MiniLM-L6-v2-onnx", &mut mini);
        assert!(
            mini.query_prefix.is_empty(),
            "the pinned model takes no prefixes and must keep taking none"
        );
    }

    #[test]
    fn test_embedder_config_default() {
        let config = EmbedderConfig::default();
        assert!(config.query_prefix.is_empty());
        assert!(config.passage_prefix.is_empty());
    }

    #[test]
    fn test_parse_st_config_unrecognized_key() {
        let content = r#"{
            "prompts": {
                "unknown": "value"
            }
        }"#;
        let mut config = EmbedderConfig::default();
        parse_st_config(content, &mut config);
        assert!(config.query_prefix.is_empty());
    }

    #[test]
    fn test_fetch_aux_configs_invalid_path() {
        // Should not panic, just log debug
        fetch_aux_configs(Path::new("/non/existent/path/12345"), "test/model");
    }

    #[test]
    fn test_parse_st_config_invalid_json() {
        let mut config = EmbedderConfig::default();
        parse_st_config("invalid json", &mut config);
        assert!(config.query_prefix.is_empty());
    }

    #[test]
    fn test_parse_st_config_no_prompts() {
        let mut config = EmbedderConfig::default();
        parse_st_config("{}", &mut config);
        assert!(config.query_prefix.is_empty());
    }

    #[test]
    fn test_load_prefixes_non_existent() {
        let dir = tempdir().unwrap();
        let config = load_prefixes(dir.path(), "non/existent");
        assert!(config.query_prefix.is_empty());
    }

    #[test]
    fn test_load_prefixes_with_file() {
        let dir = tempdir().unwrap();
        let model_id = "test/model";

        // Let's just test that it returns default config when files are missing.
        let config = load_prefixes(dir.path(), model_id);
        assert!(config.query_prefix.is_empty());
    }

    #[test]
    fn test_load_prefixes_read_error() {
        let dir = tempdir().unwrap();
        let model_id = "test/model";

        // Create a directory where a file should be to cause a read error
        let folder = format!("models--{}", model_id.replace('/', "--"));
        let snapshots = dir.path().join(folder).join("snapshots").join("main");
        fs::create_dir_all(&snapshots).unwrap();

        // Mock Repo::get by setting the refs
        let refs = dir
            .path()
            .join(format!("models--{}", model_id.replace('/', "--")))
            .join("refs");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join("main"), "main").unwrap();

        // Create a directory with the name of the file to trigger read_to_string error
        fs::create_dir(snapshots.join("config_sentence_transformers.json")).unwrap();

        let config = load_prefixes(dir.path(), model_id);
        assert!(config.query_prefix.is_empty());
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
