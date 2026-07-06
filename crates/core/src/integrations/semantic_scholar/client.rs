use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::anyhow;

use crate::metadata::doi::normalize_doi;
use crate::network::{ProviderHttpClient, ProviderHttpErrorKind};
use crate::types::{
    IntegrationState, IntegrationStatus, SemanticScholarPaper, SemanticScholarSettings,
};

use super::model::SemanticScholarPaperResponse;

const LOOKUP_FIELDS: &str = "title,citationCount,externalIds,year,venue,publicationDate";
const STATUS_PROBE_DOI: &str = "10.1145/3801158";

#[derive(Clone)]
pub struct SemanticScholarClient {
    base_url: String,
    api_key: Option<String>,
    http: ProviderHttpClient,
}

impl SemanticScholarClient {
    pub fn from_settings(settings: &SemanticScholarSettings) -> Self {
        Self::new(settings.base_url.clone(), settings.api_key.clone())
    }

    pub fn new(base_url: String, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.and_then(|key| {
                let trimmed = key.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            }),
            http: ProviderHttpClient::new("Semantic Scholar"),
        }
    }

    pub async fn status(&self, enabled: bool) -> anyhow::Result<IntegrationStatus> {
        if !enabled {
            return Ok(IntegrationStatus {
                id: "semantic_scholar".to_string(),
                enabled,
                state: IntegrationState::Disabled,
                message: "Semantic Scholar integration is disabled.".to_string(),
                version: None,
            });
        }

        let status = self
            .http
            .get_status(
                self.url(&format!(
                    "/graph/v1/paper/DOI:{}?fields=paperId",
                    urlencoding::encode(STATUS_PROBE_DOI)
                )),
                &self.headers(),
            )
            .await;

        match status {
            Ok(_) => Ok(IntegrationStatus {
                id: "semantic_scholar".to_string(),
                enabled,
                state: IntegrationState::Ready,
                message: "Semantic Scholar API is reachable.".to_string(),
                version: None,
            }),
            Err(error) if error.kind == ProviderHttpErrorKind::RateLimited => {
                Ok(IntegrationStatus {
                    id: "semantic_scholar".to_string(),
                    enabled,
                    state: IntegrationState::RateLimited,
                    message: "Semantic Scholar API is reachable, but the public rate limit is currently reached.".to_string(),
                    version: None,
                })
            }
            Err(error) => Ok(IntegrationStatus {
                id: "semantic_scholar".to_string(),
                enabled,
                state: IntegrationState::RemoteApiDown,
                message: error.to_string(),
                version: None,
            }),
        }
    }

    pub async fn lookup_by_doi(&self, doi: &str) -> anyhow::Result<SemanticScholarPaper> {
        let doi = normalize_doi(doi).ok_or_else(|| anyhow!("Invalid DOI: {doi}"))?;
        let url = self.paper_url(&doi);
        match self
            .http
            .get_json::<SemanticScholarPaperResponse>(url, &self.headers())
            .await
        {
            Ok(body) => Ok(body.into_paper(doi, now_ms())),
            Err(error) if error.kind == ProviderHttpErrorKind::NotFound => {
                anyhow::bail!("Semantic Scholar paper not found for DOI {doi}");
            }
            Err(error) if error.kind == ProviderHttpErrorKind::RateLimited => {
                anyhow::bail!("Semantic Scholar API rate limit reached");
            }
            Err(error) => Err(error.into()),
        }
    }

    fn paper_url(&self, doi: &str) -> String {
        self.url(&format!(
            "/graph/v1/paper/DOI:{}?fields={LOOKUP_FIELDS}",
            urlencoding::encode(doi)
        ))
    }

    fn headers(&self) -> Vec<(&str, String)> {
        match &self.api_key {
            Some(api_key) => vec![("x-api-key", api_key.clone())],
            None => Vec::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;

    #[tokio::test]
    async fn lookup_normalizes_doi_and_sends_api_key() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/graph/v1/paper/DOI:10.1145%2F3801158")
            .match_query(Matcher::UrlEncoded("fields".into(), LOOKUP_FIELDS.into()))
            .match_header("x-api-key", "secret")
            .with_status(200)
            .with_body(
                r#"{"paperId":"p1","externalIds":{"DOI":"10.1145/3801158"},"title":"T","year":2026,"citationCount":3}"#,
            )
            .create_async()
            .await;

        let client = SemanticScholarClient::new(server.url(), Some(" secret ".into()));
        let paper = client
            .lookup_by_doi("https://doi.org/10.1145/3801158")
            .await
            .unwrap();

        assert_eq!(paper.doi, "10.1145/3801158");
        assert_eq!(paper.paper_id, "p1");
        assert_eq!(paper.citation_count, 3);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn status_uses_paper_probe_not_search() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/graph/v1/paper/DOI:10.1145%2F3801158")
            .match_query(Matcher::UrlEncoded("fields".into(), "paperId".into()))
            .with_status(200)
            .with_body(r#"{"paperId":"p1"}"#)
            .create_async()
            .await;

        let client = SemanticScholarClient::new(server.url(), None);
        let status = client.status(true).await.unwrap();

        assert_eq!(status.state, IntegrationState::Ready);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn lookup_retries_rate_limit_once() {
        let mut server = mockito::Server::new_async().await;
        let first = server
            .mock("GET", "/graph/v1/paper/DOI:10.1145%2F3801158")
            .match_query(Matcher::UrlEncoded("fields".into(), LOOKUP_FIELDS.into()))
            .with_status(429)
            .with_header("Retry-After", "0")
            .expect(1)
            .create_async()
            .await;
        let second = server
            .mock("GET", "/graph/v1/paper/DOI:10.1145%2F3801158")
            .match_query(Matcher::UrlEncoded("fields".into(), LOOKUP_FIELDS.into()))
            .with_status(200)
            .with_body(
                r#"{"paperId":"p1","externalIds":{"DOI":"10.1145/3801158"},"citationCount":9}"#,
            )
            .expect(1)
            .create_async()
            .await;

        let client = SemanticScholarClient::new(server.url(), None);
        let paper = client.lookup_by_doi("10.1145/3801158").await.unwrap();

        assert_eq!(paper.citation_count, 9);
        first.assert_async().await;
        second.assert_async().await;
    }
}
