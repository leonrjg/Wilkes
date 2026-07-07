use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::anyhow;

use crate::metadata::doi::normalize_doi;
use crate::network::{ProviderHttpClient, ProviderHttpErrorKind};
use crate::types::{IntegrationState, IntegrationStatus, OpenAlexSettings, OpenAlexWork};

use super::model::OpenAlexWorksResponse;

const LOOKUP_SELECT: &str =
    "id,doi,display_name,publication_year,publication_date,cited_by_count,ids,primary_location";
const STATUS_PROBE_DOI: &str = "10.1145/3801158";

#[derive(Clone)]
pub struct OpenAlexClient {
    base_url: String,
    email: Option<String>,
    http: ProviderHttpClient,
}

impl OpenAlexClient {
    pub fn from_settings(settings: &OpenAlexSettings) -> Self {
        Self::new(settings.base_url.clone(), settings.email.clone())
    }

    pub fn new(base_url: String, email: Option<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            email: email.and_then(|value| {
                let trimmed = value.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            }),
            http: ProviderHttpClient::new("OpenAlex"),
        }
    }

    pub async fn status(&self, enabled: bool) -> anyhow::Result<IntegrationStatus> {
        if !enabled {
            return Ok(IntegrationStatus {
                id: "openalex".to_string(),
                enabled,
                state: IntegrationState::Disabled,
                message: "OpenAlex integration is disabled.".to_string(),
                version: None,
            });
        }

        let status = self
            .http
            .get_status(self.lookup_url(STATUS_PROBE_DOI), &[])
            .await;
        match status {
            Ok(_) => Ok(IntegrationStatus {
                id: "openalex".to_string(),
                enabled,
                state: IntegrationState::Ready,
                message: "OpenAlex API is reachable.".to_string(),
                version: None,
            }),
            Err(error) if error.kind == ProviderHttpErrorKind::RateLimited => {
                Ok(IntegrationStatus {
                    id: "openalex".to_string(),
                    enabled,
                    state: IntegrationState::RateLimited,
                    message: "OpenAlex API is reachable, but the rate limit is currently reached."
                        .to_string(),
                    version: None,
                })
            }
            Err(error) => Ok(IntegrationStatus {
                id: "openalex".to_string(),
                enabled,
                state: IntegrationState::RemoteApiDown,
                message: error.to_string(),
                version: None,
            }),
        }
    }

    pub async fn lookup_by_doi(&self, doi: &str) -> anyhow::Result<OpenAlexWork> {
        let doi = normalize_doi(doi).ok_or_else(|| anyhow!("Invalid DOI: {doi}"))?;
        let cached_at_ms = now_ms();
        let mut body = self.lookup_works(self.lookup_url(&doi)).await?;
        if body.results.is_empty() {
            body = self
                .lookup_works(self.location_landing_page_url(&doi))
                .await?;
        }

        body.results
            .into_iter()
            .next()
            .map(|work| work.into_work(doi.clone(), cached_at_ms))
            .ok_or_else(|| anyhow!("OpenAlex work not found for DOI {doi}"))
    }

    async fn lookup_works(&self, url: String) -> anyhow::Result<OpenAlexWorksResponse> {
        match self.http.get_json::<OpenAlexWorksResponse>(url, &[]).await {
            Ok(body) => Ok(body),
            Err(error) if error.kind == ProviderHttpErrorKind::RateLimited => {
                anyhow::bail!("OpenAlex API rate limit reached");
            }
            Err(error) => Err(error.into()),
        }
    }

    fn lookup_url(&self, doi: &str) -> String {
        let mut url = format!(
            "{}/works?filter=doi:{}&select={LOOKUP_SELECT}",
            self.base_url,
            urlencoding::encode(&format!("https://doi.org/{doi}")),
        );
        if let Some(email) = &self.email {
            url.push_str("&mailto=");
            url.push_str(&urlencoding::encode(email));
        }
        url
    }

    fn location_landing_page_url(&self, doi: &str) -> String {
        self.query_url(
            "locations.landing_page_url",
            &format!("https://doi.org/{}", doi.to_ascii_lowercase()),
        )
    }

    fn query_url(&self, field: &str, value: &str) -> String {
        let mut url = format!(
            "{}/works?filter={}:{}&select={LOOKUP_SELECT}",
            self.base_url,
            field,
            urlencoding::encode(value),
        );
        if let Some(email) = &self.email {
            url.push_str("&mailto=");
            url.push_str(&urlencoding::encode(email));
        }
        url
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
    async fn lookup_normalizes_doi_and_sends_mailto() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/works")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded(
                    "filter".into(),
                    "doi:https://doi.org/10.1145/3801158".into(),
                ),
                Matcher::UrlEncoded("select".into(), LOOKUP_SELECT.into()),
                Matcher::UrlEncoded("mailto".into(), "team@example.com".into()),
            ]))
            .with_status(200)
            .with_body(
                r#"{"results":[{"id":"https://openalex.org/W1","doi":"https://doi.org/10.1145/3801158","display_name":"T","publication_year":2026,"cited_by_count":4,"ids":{"doi":"https://doi.org/10.1145/3801158"}}]}"#,
            )
            .create_async()
            .await;

        let client = OpenAlexClient::new(server.url(), Some(" team@example.com ".into()));
        let work = client
            .lookup_by_doi("https://doi.org/10.1145/3801158")
            .await
            .unwrap();

        assert_eq!(work.doi, "10.1145/3801158");
        assert_eq!(work.work_id, "https://openalex.org/W1");
        assert_eq!(work.citation_count, 4);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn lookup_reports_empty_results_as_not_found() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/works")
            .match_query(Matcher::Any)
            .expect(2)
            .with_status(200)
            .with_body(r#"{"results":[]}"#)
            .create_async()
            .await;

        let client = OpenAlexClient::new(server.url(), None);
        let err = client.lookup_by_doi("10.1145/3801158").await.unwrap_err();

        assert!(err.to_string().contains("not found"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn lookup_falls_back_to_location_landing_page_url() {
        let mut server = mockito::Server::new_async().await;
        let doi_mock = server
            .mock("GET", "/works")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded(
                    "filter".into(),
                    "doi:https://doi.org/10.48550/arXiv.2103.04682".into(),
                ),
                Matcher::UrlEncoded("select".into(), LOOKUP_SELECT.into()),
            ]))
            .with_status(200)
            .with_body(r#"{"results":[]}"#)
            .create_async()
            .await;
        let location_mock = server
            .mock("GET", "/works")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded(
                    "filter".into(),
                    "locations.landing_page_url:https://doi.org/10.48550/arxiv.2103.04682"
                        .into(),
                ),
                Matcher::UrlEncoded("select".into(), LOOKUP_SELECT.into()),
            ]))
            .with_status(200)
            .with_body(
                r#"{"results":[{"id":"https://openalex.org/W3145166639","doi":"https://doi.org/10.1109/msr52588.2021.00074","display_name":"Sampling Projects in GitHub for MSR Studies","publication_year":2021,"cited_by_count":2,"ids":{"doi":"https://doi.org/10.1109/msr52588.2021.00074"}}]}"#,
            )
            .create_async()
            .await;

        let client = OpenAlexClient::new(server.url(), None);
        let work = client
            .lookup_by_doi("10.48550/arXiv.2103.04682")
            .await
            .unwrap();

        assert_eq!(work.doi, "10.48550/arXiv.2103.04682");
        assert_eq!(work.work_id, "https://openalex.org/W3145166639");
        assert_eq!(
            work.title.as_deref(),
            Some("Sampling Projects in GitHub for MSR Studies")
        );
        doi_mock.assert_async().await;
        location_mock.assert_async().await;
    }
}
