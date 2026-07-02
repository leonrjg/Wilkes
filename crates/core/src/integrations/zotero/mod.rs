pub mod client;
pub mod lookup;
pub mod model;

use async_trait::async_trait;

use crate::integrations::Integration;
use crate::types::{IntegrationStatus, Settings};

pub use client::ZoteroClient;
pub use lookup::{resolve_file, MatchConfidence, ResolvedZoteroItem};

pub struct ZoteroIntegration;

#[async_trait]
impl Integration for ZoteroIntegration {
    fn id(&self) -> &'static str {
        "zotero"
    }

    fn is_enabled(&self, settings: &Settings) -> bool {
        settings.integrations.zotero.enabled
    }

    async fn health_check(&self, settings: &Settings) -> anyhow::Result<IntegrationStatus> {
        ZoteroClient::from_settings(&settings.integrations.zotero)
            .status(settings.integrations.zotero.enabled)
            .await
    }
}
