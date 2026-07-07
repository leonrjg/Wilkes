use std::collections::HashMap;

use serde::Deserialize;

use crate::types::OpenAlexWork;

#[derive(Clone, Debug, Deserialize)]
pub struct OpenAlexWorksResponse {
    #[serde(default)]
    pub results: Vec<OpenAlexWorkResponse>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct OpenAlexWorkResponse {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub publication_year: Option<i64>,
    #[serde(default)]
    pub publication_date: Option<String>,
    #[serde(default)]
    pub cited_by_count: Option<i64>,
    #[serde(default)]
    pub ids: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub primary_location: Option<OpenAlexLocation>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct OpenAlexLocation {
    #[serde(default)]
    pub source: Option<OpenAlexSource>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct OpenAlexSource {
    #[serde(default)]
    pub display_name: Option<String>,
}

impl OpenAlexWorkResponse {
    pub fn into_work(self, doi: String, cached_at_ms: i64) -> OpenAlexWork {
        OpenAlexWork {
            doi,
            work_id: self.id,
            title: self.display_name,
            year: self.publication_year,
            publication_date: self.publication_date,
            venue: self
                .primary_location
                .and_then(|location| location.source)
                .and_then(|source| source.display_name),
            citation_count: self.cited_by_count.unwrap_or(0),
            external_ids: self.ids,
            cached_at_ms,
        }
    }
}
