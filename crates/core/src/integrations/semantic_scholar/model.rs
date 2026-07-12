use std::collections::HashMap;

use serde::Deserialize;

use crate::types::{LiteratureSearchResult, SemanticScholarPaper};

#[derive(Clone, Debug, Deserialize)]
pub struct SemanticScholarSearchResponse {
    #[serde(default)]
    pub data: Vec<SemanticScholarPaperResponse>,
}

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
    #[serde(default)]
    pub is_open_access: bool,
    #[serde(default)]
    pub open_access_pdf: Option<SemanticScholarOpenAccessPdf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticScholarOpenAccessPdf {
    pub url: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
}

impl SemanticScholarPaperResponse {
    pub fn into_search_result(self) -> LiteratureSearchResult {
        let doi = self
            .external_ids
            .get("DOI")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let pdf = self.open_access_pdf;
        LiteratureSearchResult {
            id: self.paper_id,
            doi,
            title: self.title,
            year: self.year,
            publication_date: self.publication_date,
            venue: self.venue,
            citation_count: self.citation_count.unwrap_or(0),
            is_open_access: self.is_open_access,
            pdf_url: pdf.as_ref().map(|pdf| pdf.url.clone()),
            landing_page_url: None,
            open_access_status: pdf.as_ref().and_then(|pdf| pdf.status.clone()),
            license: pdf.and_then(|pdf| pdf.license),
        }
    }

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
