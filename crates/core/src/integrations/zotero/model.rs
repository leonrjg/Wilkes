use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ZoteroItem {
    pub key: String,
    #[serde(default)]
    pub meta: ZoteroItemMeta,
    pub data: ZoteroItemData,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoteroItemMeta {
    /// Zotero's normalized ISO date ("YYYY-MM-DD"), derived from the free-text
    /// `data.date`. Preferred for display since `data.date` may be unpadded or
    /// non-numeric (e.g. "2025-4-26", "March 2025").
    #[serde(default)]
    pub parsed_date: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoteroItemData {
    #[serde(default)]
    pub item_type: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, alias = "DOI")]
    pub doi: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub creators: Vec<ZoteroCreator>,
    #[serde(default)]
    pub parent_item: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoteroCreator {
    #[serde(default)]
    pub creator_type: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

impl ZoteroCreator {
    pub fn display_name(&self) -> Option<String> {
        if let Some(name) = self.name.as_ref().filter(|s| !s.trim().is_empty()) {
            return Some(name.trim().to_string());
        }

        let full = [self.first_name.as_deref(), self.last_name.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");

        (!full.is_empty()).then_some(full)
    }

    /// Surname for an in-text citation ("Guo"). Falls back to a single-field
    /// name (e.g. an organisation) when there is no separate last name.
    pub fn citation_name(&self) -> Option<String> {
        if let Some(last) = self.last_name.as_ref().filter(|s| !s.trim().is_empty()) {
            return Some(last.trim().to_string());
        }
        self.name
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StandaloneAttachmentMetadata {
    // Zotero's connector expects `sessionID` (capital ID); `camelCase` would
    // otherwise emit `sessionId` and the connector rejects it with
    // SESSION_ID_NOT_PROVIDED.
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub title: String,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveStandaloneAttachmentResponse {
    #[serde(default)]
    pub can_recognize: bool,
}

/// CSL-formatted strings returned by the local API when `include=citation,bib`
/// is requested. Both are HTML fragments produced entirely by Zotero.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ZoteroCitation {
    #[serde(default)]
    pub citation: Option<String>,
    #[serde(default)]
    pub bib: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_attachment_metadata_uses_capital_session_id() {
        // Regression: the connector rejects `sessionId` with
        // SESSION_ID_NOT_PROVIDED; it must be serialized as `sessionID`.
        let json = serde_json::to_value(StandaloneAttachmentMetadata {
            session_id: "wilkes-zotero".to_string(),
            title: "Doc".to_string(),
            url: "file:///tmp/doc.pdf".to_string(),
        })
        .unwrap();

        assert_eq!(json["sessionID"], "wilkes-zotero");
        assert!(json.get("sessionId").is_none());
    }
}
