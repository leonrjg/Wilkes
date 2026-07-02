use anyhow::{anyhow, Context};
use reqwest::StatusCode;

use crate::types::{IntegrationState, IntegrationStatus, ZoteroSettings};

use super::model::{
    SaveStandaloneAttachmentResponse, StandaloneAttachmentMetadata, ZoteroCitation, ZoteroItem,
};

#[derive(Clone)]
pub struct ZoteroClient {
    base_url: String,
    http: reqwest::Client,
}

impl ZoteroClient {
    pub fn from_settings(settings: &ZoteroSettings) -> Self {
        Self::new(settings.base_url.clone())
    }

    pub fn new(base_url: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    pub async fn status(&self, enabled: bool) -> anyhow::Result<IntegrationStatus> {
        let connector = self.http.get(self.url("/connector/ping")).send().await;

        let version = match connector {
            Ok(response) if response.status().is_success() => response.text().await.ok(),
            _ => {
                return Ok(IntegrationStatus {
                    id: "zotero".to_string(),
                    enabled,
                    state: IntegrationState::ZoteroDown,
                    message: "Zotero is not reachable at the configured local URL.".to_string(),
                    version: None,
                })
            }
        };

        let api = self
            .http
            .get(self.api_url("/api/users/0/items?limit=1"))
            .send()
            .await;

        match api {
            Ok(response) if response.status().is_success() => Ok(IntegrationStatus {
                id: "zotero".to_string(),
                enabled,
                state: IntegrationState::Ready,
                message: "Zotero local API is reachable.".to_string(),
                version,
            }),
            Ok(response) if response.status() == StatusCode::FORBIDDEN => Ok(IntegrationStatus {
                id: "zotero".to_string(),
                enabled,
                state: IntegrationState::LocalApiDisabled,
                message: "Zotero is running, but the local API is disabled.".to_string(),
                version,
            }),
            Ok(response) => Ok(IntegrationStatus {
                id: "zotero".to_string(),
                enabled,
                state: IntegrationState::ZoteroDown,
                message: format!("Zotero local API returned HTTP {}.", response.status()),
                version,
            }),
            Err(_) => Ok(IntegrationStatus {
                id: "zotero".to_string(),
                enabled,
                state: IntegrationState::ZoteroDown,
                message: "Zotero local API is not reachable.".to_string(),
                version,
            }),
        }
    }

    /// Full-text search restricted to bibliographic (non-attachment) items.
    ///
    /// `qmode=everything` matches attachment full text, so a query for a DOI or
    /// title otherwise returns dozens of attachment PDFs that merely *mention*
    /// the string, burying the actual item past `limit`. Excluding attachments
    /// (`itemType=-attachment`) leaves only the items whose own fields we match
    /// against in `find_by_doi` / `find_by_title`.
    pub async fn search_everything(
        &self,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<ZoteroItem>> {
        let url = self.api_url(&everything_query_path(query, limit));
        self.get_items(&url).await
    }

    pub async fn attachment_items(&self) -> anyhow::Result<Vec<ZoteroItem>> {
        let mut start = 0usize;
        let limit = 100usize;
        let mut out = Vec::new();

        loop {
            let url = self.api_url(&format!(
                "/api/users/0/items?itemType=attachment&start={start}&limit={limit}"
            ));
            let batch = self.get_items(&url).await?;
            let count = batch.len();
            out.extend(batch);
            if count < limit {
                break;
            }
            start += limit;
        }

        Ok(out)
    }

    pub async fn item(&self, key: &str) -> anyhow::Result<ZoteroItem> {
        let url = self.api_url(&format!("/api/users/0/items/{}", urlencoding::encode(key)));
        let response = self.http.get(url).send().await?;
        if !response.status().is_success() {
            anyhow::bail!("Zotero item lookup failed with HTTP {}", response.status());
        }
        response.json::<ZoteroItem>().await.map_err(Into::into)
    }

    /// Fetch the CSL citation and bibliography entry for an item, formatted by
    /// Zotero in the given style. Zotero owns all formatting; we only pick the
    /// style id and render the returned HTML.
    pub async fn citation(&self, key: &str, style: &str) -> anyhow::Result<ZoteroCitation> {
        let url = self.api_url(&format!(
            "/api/users/0/items/{}?include=citation,bib&style={}",
            urlencoding::encode(key),
            urlencoding::encode(style),
        ));
        let response = self.http.get(url).send().await?;
        if !response.status().is_success() {
            anyhow::bail!(
                "Zotero citation request failed with HTTP {}",
                response.status()
            );
        }
        response.json::<ZoteroCitation>().await.map_err(Into::into)
    }

    pub async fn save_standalone_attachment(
        &self,
        title: &str,
        source_path: &str,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> anyhow::Result<SaveStandaloneAttachmentResponse> {
        let metadata = StandaloneAttachmentMetadata {
            // The connector registers each save under its sessionID and rejects a
            // reused one with SESSION_EXISTS, so mint a fresh id per request.
            session_id: uuid::Uuid::new_v4().to_string(),
            title: title.to_string(),
            url: format!("file://{source_path}"),
        };
        let metadata = serde_json::to_string(&metadata)?;
        let response = self
            .http
            .post(self.url("/connector/saveStandaloneAttachment"))
            .header("X-Metadata", metadata)
            .header("Content-Type", content_type)
            .body(bytes)
            .send()
            .await
            .context("failed to send file to Zotero connector")?;

        if response.status() != StatusCode::CREATED {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Zotero connector saveStandaloneAttachment failed with HTTP {status}: {body}"
            ));
        }

        response
            .json::<SaveStandaloneAttachmentResponse>()
            .await
            .or_else(|_| {
                Ok(SaveStandaloneAttachmentResponse {
                    can_recognize: false,
                })
            })
    }

    async fn get_items(&self, url: &str) -> anyhow::Result<Vec<ZoteroItem>> {
        let response = self.http.get(url).send().await?;
        if !response.status().is_success() {
            anyhow::bail!(
                "Zotero local API request failed with HTTP {}",
                response.status()
            );
        }
        response.json::<Vec<ZoteroItem>>().await.map_err(Into::into)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn api_url(&self, path: &str) -> String {
        self.url(path)
    }
}

fn everything_query_path(query: &str, limit: usize) -> String {
    format!(
        "/api/users/0/items?q={}&qmode=everything&itemType=-attachment&limit={}",
        urlencoding::encode(query),
        limit
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_query_excludes_attachments() {
        // Regression: qmode=everything matches attachment full text, so without
        // this exclusion a DOI/title query is flooded with attachment PDFs and
        // the real item is pushed past `limit`, making it unresolvable.
        let path = everything_query_path("10.1007/s10664-025-10614-4", 10);
        assert!(path.contains("itemType=-attachment"), "path was: {path}");
        assert!(path.contains("qmode=everything"));
        assert!(path.contains("q=10.1007%2Fs10664-025-10614-4"));
        assert!(path.contains("limit=10"));
    }
}
