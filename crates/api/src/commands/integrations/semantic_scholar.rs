use std::sync::{Arc, Mutex};

use wilkes_core::integrations::semantic_scholar::SemanticScholarClient;
use wilkes_core::metadata::cache::MetadataCache;
use wilkes_core::metadata::doi::normalize_doi;
use wilkes_core::types::{IntegrationStatus, SemanticScholarPaper, Settings};

pub async fn semantic_scholar_status(settings: Settings) -> anyhow::Result<IntegrationStatus> {
    let client = SemanticScholarClient::from_settings(&settings.integrations.semantic_scholar);
    client
        .status(settings.integrations.semantic_scholar.enabled)
        .await
}

pub async fn semantic_scholar_lookup(
    settings: Settings,
    cache: Option<Arc<Mutex<MetadataCache>>>,
    doi: String,
) -> anyhow::Result<SemanticScholarPaper> {
    ensure_enabled(&settings)?;
    let doi = normalize_doi(&doi).ok_or_else(|| anyhow::anyhow!("Invalid DOI: {doi}"))?;

    let cache = cache.ok_or_else(|| anyhow::anyhow!("Semantic Scholar cache is unavailable"))?;
    {
        let guard = cache
            .lock()
            .map_err(|_| anyhow::anyhow!("Semantic Scholar cache lock failed"))?;
        if let Some(cached) = guard.get_semantic_scholar_by_doi(&doi)? {
            return Ok(cached);
        }
    }

    let client = SemanticScholarClient::from_settings(&settings.integrations.semantic_scholar);
    let paper = client.lookup_by_doi(&doi).await?;

    {
        let guard = cache
            .lock()
            .map_err(|_| anyhow::anyhow!("Semantic Scholar cache lock failed"))?;
        guard.upsert_semantic_scholar_by_doi(&paper)?;
    }

    Ok(paper)
}

fn ensure_enabled(settings: &Settings) -> anyhow::Result<()> {
    if settings.integrations.semantic_scholar.enabled {
        Ok(())
    } else {
        anyhow::bail!("Semantic Scholar integration is disabled.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lookup_rejects_when_disabled() {
        let settings = Settings::default();
        let err = semantic_scholar_lookup(settings, None, "10.1145/3801158".into())
            .await
            .unwrap_err();

        assert!(err.to_string().contains("disabled"));
    }
}
