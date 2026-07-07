use std::sync::{Arc, Mutex};

use wilkes_core::integrations::openalex::OpenAlexClient;
use wilkes_core::metadata::cache::MetadataCache;
use wilkes_core::metadata::doi::normalize_doi;
use wilkes_core::types::{IntegrationStatus, OpenAlexWork, Settings};

pub async fn openalex_status(settings: Settings) -> anyhow::Result<IntegrationStatus> {
    let client = OpenAlexClient::from_settings(&settings.integrations.openalex);
    client.status(settings.integrations.openalex.enabled).await
}

pub async fn openalex_lookup(
    settings: Settings,
    cache: Option<Arc<Mutex<MetadataCache>>>,
    doi: String,
) -> anyhow::Result<OpenAlexWork> {
    ensure_enabled(&settings)?;
    let doi = normalize_doi(&doi).ok_or_else(|| anyhow::anyhow!("Invalid DOI: {doi}"))?;

    let cache = cache.ok_or_else(|| anyhow::anyhow!("OpenAlex cache is unavailable"))?;
    {
        let guard = cache
            .lock()
            .map_err(|_| anyhow::anyhow!("OpenAlex cache lock failed"))?;
        if let Some(cached) = guard.get_openalex_by_doi(&doi)? {
            return Ok(cached);
        }
    }

    let client = OpenAlexClient::from_settings(&settings.integrations.openalex);
    let work = client.lookup_by_doi(&doi).await?;

    {
        let guard = cache
            .lock()
            .map_err(|_| anyhow::anyhow!("OpenAlex cache lock failed"))?;
        guard.upsert_openalex_by_doi(&work)?;
    }

    Ok(work)
}

fn ensure_enabled(settings: &Settings) -> anyhow::Result<()> {
    if settings.integrations.openalex.enabled {
        Ok(())
    } else {
        anyhow::bail!("OpenAlex integration is disabled.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lookup_rejects_when_disabled() {
        let settings = Settings::default();
        let err = openalex_lookup(settings, None, "10.1145/3801158".into())
            .await
            .unwrap_err();

        assert!(err.to_string().contains("disabled"));
    }
}
