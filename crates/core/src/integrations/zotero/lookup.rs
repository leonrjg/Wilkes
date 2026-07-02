use std::path::Path;

use crate::types::DocumentMetadata;

use super::client::ZoteroClient;
use super::model::ZoteroItem;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchConfidence {
    High,
    Low,
}

#[derive(Clone, Debug)]
pub struct ResolvedZoteroItem {
    pub item: ZoteroItem,
    pub confidence: MatchConfidence,
}

/// Resolve a file to a Zotero item. `attachments` is the library's attachment
/// list, passed in so batch callers (e.g. metadata tabulation over a whole
/// directory) fetch it once rather than per file. Single-file callers fetch it
/// once and hand it in the same way.
pub async fn resolve_file(
    client: &ZoteroClient,
    path: &Path,
    metadata: &DocumentMetadata,
    attachments: &[ZoteroItem],
) -> anyhow::Result<Option<ResolvedZoteroItem>> {
    if let Some(doi) = metadata.doi.as_ref().filter(|s| !s.trim().is_empty()) {
        if let Some(item) = find_by_doi(client, doi).await? {
            return Ok(Some(ResolvedZoteroItem {
                item,
                confidence: MatchConfidence::High,
            }));
        }
    }

    if let Some(item) = find_by_attachment_path(client, path, attachments).await? {
        return Ok(Some(item));
    }

    if let Some(title) = metadata.title.as_ref().filter(|s| !s.trim().is_empty()) {
        if let Some(item) = find_by_title(client, title).await? {
            return Ok(Some(ResolvedZoteroItem {
                item,
                confidence: MatchConfidence::Low,
            }));
        }
    }

    Ok(None)
}

async fn find_by_doi(client: &ZoteroClient, doi: &str) -> anyhow::Result<Option<ZoteroItem>> {
    let wanted = normalize_cmp(doi);
    Ok(client
        .search_everything(doi, 10)
        .await?
        .into_iter()
        .find(|item| {
            item.data.doi.as_deref().map(normalize_cmp).as_deref() == Some(wanted.as_str())
        }))
}

async fn find_by_attachment_path(
    client: &ZoteroClient,
    path: &Path,
    attachments: &[ZoteroItem],
) -> anyhow::Result<Option<ResolvedZoteroItem>> {
    let absolute = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string();
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string);

    for attachment in attachments {
        if attachment
            .data
            .path
            .as_deref()
            .is_some_and(|p| path_matches(p, &absolute))
        {
            let item = parent_or_attachment(client, attachment.clone()).await?;
            return Ok(Some(ResolvedZoteroItem {
                item,
                confidence: MatchConfidence::High,
            }));
        }
    }

    let Some(file_name) = file_name else {
        return Ok(None);
    };
    let wanted = normalize_cmp(&file_name);

    for attachment in attachments {
        if attachment
            .data
            .filename
            .as_deref()
            .map(normalize_cmp)
            .as_deref()
            == Some(wanted.as_str())
        {
            let item = parent_or_attachment(client, attachment.clone()).await?;
            return Ok(Some(ResolvedZoteroItem {
                item,
                confidence: MatchConfidence::Low,
            }));
        }
    }

    Ok(None)
}

async fn find_by_title(client: &ZoteroClient, title: &str) -> anyhow::Result<Option<ZoteroItem>> {
    let wanted = normalize_cmp(title);
    Ok(client
        .search_everything(title, 10)
        .await?
        .into_iter()
        .find(|item| {
            item.data.title.as_deref().map(normalize_cmp).as_deref() == Some(wanted.as_str())
        }))
}

async fn parent_or_attachment(
    client: &ZoteroClient,
    attachment: ZoteroItem,
) -> anyhow::Result<ZoteroItem> {
    match attachment.data.parent_item.as_deref() {
        Some(parent) => client.item(parent).await,
        None => Ok(attachment),
    }
}

fn path_matches(zotero_path: &str, absolute: &str) -> bool {
    let without_prefix = zotero_path
        .strip_prefix("file://")
        .unwrap_or(zotero_path)
        .trim();
    without_prefix == absolute
}

fn normalize_cmp(value: &str) -> String {
    value.trim().to_lowercase()
}
