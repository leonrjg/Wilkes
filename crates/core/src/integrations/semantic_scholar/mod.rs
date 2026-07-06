pub mod client;
pub mod model;

use async_trait::async_trait;

use crate::integrations::Integration;
use crate::types::{IntegrationStatus, Settings};

pub use client::SemanticScholarClient;

pub struct SemanticScholarIntegration;

#[async_trait]
impl Integration for SemanticScholarIntegration {
    fn id(&self) -> &'static str {
        "semantic_scholar"
    }

    fn is_enabled(&self, settings: &Settings) -> bool {
        settings.integrations.semantic_scholar.enabled
    }

    async fn health_check(&self, settings: &Settings) -> anyhow::Result<IntegrationStatus> {
        SemanticScholarClient::from_settings(&settings.integrations.semantic_scholar)
            .status(settings.integrations.semantic_scholar.enabled)
            .await
    }
}
