use async_trait::async_trait;

use crate::types::{IntegrationStatus, Settings};

pub mod citations;
pub mod openalex;
pub mod semantic_scholar;
pub mod zotero;

pub use citations::CitationSource;

#[async_trait]
pub trait Integration: Send + Sync {
    fn id(&self) -> &'static str;
    fn is_enabled(&self, settings: &Settings) -> bool;
    async fn health_check(&self, settings: &Settings) -> anyhow::Result<IntegrationStatus>;
}

pub struct IntegrationRegistry {
    integrations: Vec<Box<dyn Integration>>,
}

impl IntegrationRegistry {
    pub fn new() -> Self {
        Self {
            integrations: Vec::new(),
        }
    }

    pub fn register(&mut self, integration: Box<dyn Integration>) {
        self.integrations.push(integration);
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn Integration> {
        self.integrations.iter().map(|i| i.as_ref())
    }
}

impl Default for IntegrationRegistry {
    fn default() -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(zotero::ZoteroIntegration));
        registry.register(Box::new(semantic_scholar::SemanticScholarIntegration));
        registry.register(Box::new(openalex::OpenAlexIntegration));
        registry
    }
}
