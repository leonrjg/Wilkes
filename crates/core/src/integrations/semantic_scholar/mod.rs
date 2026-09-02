pub mod client;
pub mod model;

use async_trait::async_trait;

use crate::integrations::LiteratureSource;
use crate::types::{IntegrationStatus, LiteratureSearchResult};

pub use client::SemanticScholarClient;

#[async_trait]
impl LiteratureSource for SemanticScholarClient {
    fn id(&self) -> &str {
        "semantic_scholar"
    }

    fn name(&self) -> &str {
        "Semantic Scholar"
    }

    async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<LiteratureSearchResult>> {
        SemanticScholarClient::search(self, query, limit).await
    }

    async fn status(&self, enabled: bool) -> anyhow::Result<IntegrationStatus> {
        SemanticScholarClient::status(self, enabled).await
    }
}
