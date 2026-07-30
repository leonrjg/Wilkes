use std::collections::HashMap;

use serde::Deserialize;

use crate::types::{LiteratureSearchResult, OpenAlexWork};

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
    /// Top-level DOI, populated when the query selects `doi` directly (used by
    /// batch id→DOI resolution for the citation graph). Metadata lookups read
    /// the DOI from `ids` instead; both may be present.
    #[serde(default)]
    pub doi: Option<String>,
    /// OpenAlex ids of the works this work references. Populated only when the
    /// query selects `referenced_works`.
    #[serde(default)]
    pub referenced_works: Vec<String>,
    #[serde(default)]
    pub ids: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub primary_location: Option<OpenAlexLocation>,
    #[serde(default)]
    pub best_oa_location: Option<OpenAlexLocation>,
    #[serde(default)]
    pub open_access: Option<OpenAlexOpenAccess>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct OpenAlexLocation {
    #[serde(default)]
    pub source: Option<OpenAlexSource>,
    #[serde(default)]
    pub is_oa: bool,
    #[serde(default)]
    pub landing_page_url: Option<String>,
    #[serde(default)]
    pub pdf_url: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct OpenAlexOpenAccess {
    #[serde(default)]
    pub is_oa: bool,
    #[serde(default)]
    pub oa_status: Option<String>,
    #[serde(default)]
    pub oa_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct OpenAlexSource {
    #[serde(default)]
    pub display_name: Option<String>,
}

impl OpenAlexWorkResponse {
    pub fn into_search_result(self) -> LiteratureSearchResult {
        let doi = self
            .ids
            .get("doi")
            .and_then(|value| value.as_str())
            .and_then(crate::metadata::doi::normalize_doi);
        let oa = self.open_access;
        let best_oa = self.best_oa_location;
        LiteratureSearchResult {
            id: self.id,
            doi,
            title: self.display_name,
            year: self.publication_year,
            publication_date: self.publication_date,
            venue: self
                .primary_location
                .and_then(|location| location.source)
                .and_then(|source| source.display_name),
            citation_count: self.cited_by_count.unwrap_or(0),
            is_open_access: oa.as_ref().is_some_and(|oa| oa.is_oa)
                || best_oa.as_ref().is_some_and(|location| location.is_oa),
            pdf_url: best_oa
                .as_ref()
                .and_then(|location| location.pdf_url.clone()),
            landing_page_url: best_oa
                .as_ref()
                .and_then(|location| location.landing_page_url.clone())
                .or_else(|| oa.as_ref().and_then(|oa| oa.oa_url.clone())),
            open_access_status: oa.and_then(|oa| oa.oa_status),
            license: best_oa.and_then(|location| location.license),
        }
    }

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
