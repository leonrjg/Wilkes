use std::collections::HashMap;

use serde::Deserialize;

use crate::types::SemanticScholarPaper;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticScholarPaperResponse {
    pub paper_id: String,
    #[serde(default)]
    pub external_ids: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub venue: Option<String>,
    #[serde(default)]
    pub year: Option<i64>,
    #[serde(default)]
    pub citation_count: Option<i64>,
    #[serde(default)]
    pub publication_date: Option<String>,
}

impl SemanticScholarPaperResponse {
    pub fn into_paper(self, doi: String, cached_at_ms: i64) -> SemanticScholarPaper {
        SemanticScholarPaper {
            doi,
            paper_id: self.paper_id,
            title: self.title,
            year: self.year,
            publication_date: self.publication_date,
            venue: self.venue,
            citation_count: self.citation_count.unwrap_or(0),
            external_ids: self.external_ids,
            cached_at_ms,
        }
    }
}
