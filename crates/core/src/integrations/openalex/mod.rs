mod client;
mod model;

use async_trait::async_trait;

use crate::integrations::Integration;
use crate::types::{IntegrationStatus, Settings};

pub use client::OpenAlexClient;

pub struct OpenAlexIntegration;

#[async_trait]
impl Integration for OpenAlexIntegration {
    fn id(&self) -> &'static str {
        "openalex"
    }

    fn is_enabled(&self, settings: &Settings) -> bool {
        settings.integrations.openalex.enabled
    }

    async fn health_check(&self, settings: &Settings) -> anyhow::Result<IntegrationStatus> {
        OpenAlexClient::from_settings(&settings.integrations.openalex)
            .status(settings.integrations.openalex.enabled)
            .await
    }
}
