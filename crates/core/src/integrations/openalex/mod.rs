mod client;
mod model;

use async_trait::async_trait;

use crate::integrations::LiteratureSource;
use crate::types::{IntegrationStatus, LiteratureSearchResult};

pub use client::OpenAlexClient;

#[async_trait]
impl LiteratureSource for OpenAlexClient {
    fn id(&self) -> &str {
        "openalex"
    }

    fn name(&self) -> &str {
        "OpenAlex"
    }

    async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<LiteratureSearchResult>> {
        OpenAlexClient::search(self, query, limit).await
    }

    async fn status(&self, enabled: bool) -> anyhow::Result<IntegrationStatus> {
        OpenAlexClient::status(self, enabled).await
    }
}
