use async_trait::async_trait;

/// Provider-neutral source of outgoing citation edges.
///
/// The contract is **DOI in, DOIs out**: callers identify a work by its DOI and
/// receive the normalized DOIs it references. No provider-specific identifier
/// (an OpenAlex work id, a Semantic Scholar paper id) ever crosses this
/// boundary, so the citation-graph storage and queries stay decoupled from
/// whichever provider happens to supply the edges. Swapping OpenAlex for
/// another provider means writing a second implementation of this trait; the
/// persisted `document_citations` schema and every query over it are unchanged.
#[async_trait]
pub trait CitationSource: Send + Sync {
    /// Return the normalized DOIs referenced by the work identified by `doi`.
    ///
    /// References that the provider cannot resolve to a DOI (books, datasets,
    /// works without a registered DOI) are omitted: they can never participate
    /// in a DOI-keyed library join, so they are not edges we can store.
    async fn references(&self, doi: &str) -> anyhow::Result<Vec<String>>;
}
