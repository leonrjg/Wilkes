use std::path::{Path, PathBuf};

use anyhow::Context;
use wilkes_core::integrations::zotero::model::ZoteroItem;
use wilkes_core::integrations::zotero::{resolve_file, MatchConfidence, ZoteroClient};
use wilkes_core::types::{
    AddOutcome, CitationResult, DocumentMetadata, IntegrationState, IntegrationStatus, Settings,
};

use crate::commands::metadata::get_file_metadata;

pub async fn zotero_status(settings: Settings) -> anyhow::Result<IntegrationStatus> {
    let client = ZoteroClient::from_settings(&settings.integrations.zotero);
    client.status(settings.integrations.zotero.enabled).await
}

/// Authoritative single-file metadata: file-based extraction overridden by the
/// Zotero library record when the file resolves. Best-effort — any Zotero
/// failure (disabled, unreachable, no match) keeps the file-based value. This
/// is the single owner of the compose rule for one file; batch tabulation
/// reuses `resolve_override` directly so it can fetch attachments once.
pub async fn resolve_file_metadata(
    settings: Settings,
    path: PathBuf,
) -> anyhow::Result<DocumentMetadata> {
    let file_based =
        get_file_metadata(path.clone(), settings.supported_extensions.clone()).await?;

    if !settings.integrations.zotero.enabled {
        return Ok(file_based);
    }

    let client = ZoteroClient::from_settings(&settings.integrations.zotero);
    let attachments = match client.attachment_items().await {
        Ok(a) => a,
        Err(e) => {
            tracing::info!("resolve_file_metadata: zotero attachment fetch failed: {e:#}");
            return Ok(file_based);
        }
    };
    let zotero = match resolve_override(&client, &path, &file_based, &attachments).await {
        Ok(opt) => opt,
        Err(e) => {
            tracing::info!("resolve_file_metadata: zotero resolve failed: {e:#}");
            None
        }
    };
    Ok(zotero.unwrap_or(file_based))
}

/// Resolve a file against Zotero and, if matched, return the authoritative
/// library metadata that should override file-based extraction. `attachments`
/// is the library's attachment list, passed in so batch callers fetch it once.
/// Returns `Ok(None)` when the file does not resolve to any item.
pub async fn resolve_override(
    client: &ZoteroClient,
    path: &Path,
    local_metadata: &DocumentMetadata,
    attachments: &[ZoteroItem],
) -> anyhow::Result<Option<DocumentMetadata>> {
    let resolved = resolve_file(client, path, local_metadata, attachments).await?;
    Ok(resolved.map(|r| document_metadata_from_item(&r.item)))
}

pub async fn zotero_generate_citation(
    settings: Settings,
    path: PathBuf,
) -> anyhow::Result<CitationResult> {
    ensure_ready(&settings).await?;
    let local_metadata =
        get_file_metadata(path.clone(), settings.supported_extensions.clone()).await?;
    let client = ZoteroClient::from_settings(&settings.integrations.zotero);
    let attachments = client.attachment_items().await?;
    let resolved = resolve_file(&client, &path, &local_metadata, &attachments)
        .await?
        .ok_or_else(|| anyhow::anyhow!("No Zotero item found for this file"))?;

    let citation = client
        .citation(
            &resolved.item.key,
            &settings.integrations.zotero.citation_style,
        )
        .await?;

    Ok(CitationResult {
        citation: citation.citation,
        bibliography: citation.bib,
        low_confidence: resolved.confidence == MatchConfidence::Low,
    })
}

pub async fn zotero_add_item(settings: Settings, path: PathBuf) -> anyhow::Result<AddOutcome> {
    ensure_ready(&settings).await?;
    let local_metadata =
        get_file_metadata(path.clone(), settings.supported_extensions.clone()).await?;
    let client = ZoteroClient::from_settings(&settings.integrations.zotero);
    let attachments = client.attachment_items().await?;

    if let Some(resolved) = resolve_file(&client, &path, &local_metadata, &attachments).await? {
        return match resolved.confidence {
            MatchConfidence::High => Ok(AddOutcome::AlreadyPresent {
                item_key: resolved.item.key,
            }),
            MatchConfidence::Low => Ok(AddOutcome::PossibleDuplicate {
                item_key: resolved.item.key,
                message:
                    "A possible Zotero duplicate was found by filename or title; no item was added."
                        .to_string(),
            }),
        };
    }

    let title = local_metadata
        .title
        .clone()
        .or_else(|| file_stem(&path))
        .unwrap_or_else(|| "Untitled attachment".to_string());
    let bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("failed to read {}", path.display()))?;
    let content_type = content_type_for(&path);
    client
        .save_standalone_attachment(&title, &path.to_string_lossy(), content_type, bytes)
        .await?;

    // Re-fetch attachments: the save above added a new one to the library.
    let attachments = client.attachment_items().await?;
    let item_key = resolve_file(&client, &path, &local_metadata, &attachments)
        .await?
        .filter(|resolved| resolved.confidence == MatchConfidence::High)
        .map(|resolved| resolved.item.key);

    Ok(AddOutcome::Added { item_key })
}

async fn ensure_ready(settings: &Settings) -> anyhow::Result<()> {
    if !settings.integrations.zotero.enabled {
        anyhow::bail!("Zotero integration is disabled.");
    }

    let status = zotero_status(settings.clone()).await?;
    match status.state {
        IntegrationState::Ready => Ok(()),
        _ => anyhow::bail!(status.message),
    }
}

fn document_metadata_from_item(
    item: &wilkes_core::integrations::zotero::model::ZoteroItem,
) -> DocumentMetadata {
    DocumentMetadata {
        title: item.data.title.clone(),
        author: citation_authors(item),
        doi: item.data.doi.clone(),
        // Zotero's normalized ISO date; the raw `data.date` is often unpadded or
        // non-numeric and would fail the viewer's date parser.
        created_at: item.meta.parsed_date.clone(),
    }
}

/// Author string in in-text citation form: "Guo", "Guo & Yang", or
/// "Guo et al." for three or more. Prefers author-type creators, falling back
/// to all creators when none are explicitly typed "author".
fn citation_authors(item: &wilkes_core::integrations::zotero::model::ZoteroItem) -> Option<String> {
    let creators = &item.data.creators;
    let authored: Vec<_> = creators
        .iter()
        .filter(|creator| creator.creator_type.as_deref() == Some("author"))
        .collect();
    let selected = if authored.is_empty() {
        creators.iter().collect::<Vec<_>>()
    } else {
        authored
    };

    let names: Vec<String> = selected
        .iter()
        .filter_map(|creator| creator.citation_name())
        .collect();

    match names.as_slice() {
        [] => None,
        [single] => Some(single.clone()),
        [first, second] => Some(format!("{first} & {second}")),
        [first, ..] => Some(format!("{first} et al.")),
    }
}

fn file_stem(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
    {
        Some(ext) if ext == "pdf" => "application/pdf",
        Some(ext) if ext == "txt" => "text/plain",
        Some(ext) if ext == "md" => "text/markdown",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wilkes_core::integrations::zotero::model::{
        ZoteroCreator, ZoteroItem, ZoteroItemData, ZoteroItemMeta,
    };

    fn author(first: &str, last: &str) -> ZoteroCreator {
        ZoteroCreator {
            creator_type: Some("author".to_string()),
            first_name: Some(first.to_string()),
            last_name: Some(last.to_string()),
            name: None,
        }
    }

    fn item_with(creators: Vec<ZoteroCreator>) -> ZoteroItem {
        ZoteroItem {
            key: "K".to_string(),
            meta: ZoteroItemMeta::default(),
            data: ZoteroItemData {
                creators,
                ..Default::default()
            },
        }
    }

    #[test]
    fn metadata_uses_normalized_parsed_date_not_raw_date() {
        let item = ZoteroItem {
            key: "K".to_string(),
            meta: ZoteroItemMeta {
                parsed_date: Some("2025-04-26".to_string()),
            },
            data: ZoteroItemData {
                date: Some("2025-4-26".to_string()),
                ..Default::default()
            },
        };
        assert_eq!(
            document_metadata_from_item(&item).created_at.as_deref(),
            Some("2025-04-26"),
        );
    }

    #[test]
    fn citation_authors_uses_et_al_for_three_or_more() {
        let item = item_with(vec![
            author("Jia", "Guo"),
            author("Z", "Yang"),
            author("M", "Sun"),
        ]);
        assert_eq!(citation_authors(&item).as_deref(), Some("Guo et al."));
    }

    #[test]
    fn citation_authors_joins_one_and_two() {
        assert_eq!(
            citation_authors(&item_with(vec![author("Jia", "Guo")])).as_deref(),
            Some("Guo"),
        );
        assert_eq!(
            citation_authors(&item_with(vec![author("Jia", "Guo"), author("Z", "Yang")]))
                .as_deref(),
            Some("Guo & Yang"),
        );
    }

    #[test]
    fn citation_authors_ignores_non_author_creators_when_authors_exist() {
        let editor = ZoteroCreator {
            creator_type: Some("editor".to_string()),
            first_name: Some("E".to_string()),
            last_name: Some("Ditor".to_string()),
            name: None,
        };
        let item = item_with(vec![author("Jia", "Guo"), editor]);
        assert_eq!(citation_authors(&item).as_deref(), Some("Guo"));
    }

    #[test]
    fn citation_authors_is_none_without_creators() {
        assert_eq!(citation_authors(&item_with(vec![])), None);
    }
}
