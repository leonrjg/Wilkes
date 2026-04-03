use hf_hub::api::sync::ApiBuilder;
use std::path::Path;

/// Fetch total download size for `model_id` from the HuggingFace API.
pub fn fetch_model_size(model_id: &str) -> anyhow::Result<u64> {
    #[derive(serde::Deserialize)]
    struct Sibling {
        rfilename: String,
        size: Option<u64>,
    }
    #[derive(serde::Deserialize)]
    struct HfModelInfo {
        siblings: Vec<Sibling>,
    }

    let url = format!("https://huggingface.co/api/models/{model_id}?blobs=true");
    let hf_info: HfModelInfo = ureq::get(&url)
        .call()
        .map_err(|e| anyhow::anyhow!("HF API request failed: {e}"))?
        .into_json()
        .map_err(|e| anyhow::anyhow!("HF API response parse failed: {e}"))?;

    // We sum up everything in the repo. For SBERT/Python, this is accurate as it 
    // downloads the whole repo. For Candle, it's an upper bound but close enough.
    let total: u64 = hf_info
        .siblings
        .iter()
        .filter_map(|s| s.size)
        .sum();

    anyhow::ensure!(total > 0, "No model files found in HF repo for '{model_id}'");
    Ok(total)
}

/// Check if a model is cached in the given data directory using hf-hub's structure.
pub fn is_model_cached(data_dir: &Path, model_id: &str) -> bool {
    // For Python/SBERT, they use the same standard HF cache structure if 
    // HF_HOME is set, or they use their own. In Wilkes, we try to share the data_dir.
    // If config.json exists, it's a good indicator.
    hf_hub::Cache::new(data_dir.to_path_buf())
        .repo(hf_hub::Repo::model(model_id.to_string()))
        .get("config.json")
        .is_some()
}
