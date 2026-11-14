//! Feeds: sources that are watched for new items, and what an item becomes.
//!
//! A feed watches one source — a blog's syndication feed, or a provider's
//! listing (which, through a custom manifest with an HTML response, also
//! watches a page of links) — and says only which items exist. What is
//! imported is always fetched from the item itself: a provider's file through
//! acquire, or a post's own page, extracted and written as one self-contained
//! HTML file ([`article`], [`document`]).
//!
//! Everything here is a pure function of what a source or a page returned,
//! so every rule is tested against recorded markup. Fetching, pacing, storage
//! and cancellation belong to the application (`wilkes_api::feeds`).

pub mod article;
pub mod document;
pub mod identity;
pub mod syndication;

use serde::{Deserialize, Serialize};

use crate::types::BrowseRequest;

/// What a feed watches.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FeedSource {
    /// An RSS, Atom or JSON Feed document; only links, titles and dates are
    /// read from it.
    Syndication { url: String },
    /// A provider's browse request, saved whole; also a watched page, through
    /// a custom manifest with an HTML response.
    Listing {
        provider: String,
        request: BrowseRequest,
    },
}

/// One item a source lists, before anything is fetched from it.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceEntry {
    /// As the source gave it; what is fetched.
    pub link: String,
    pub title: Option<String>,
    pub author: Option<String>,
    /// The publication date the source gave, when it is a real one: see
    /// [`identity::publication_date`].
    pub published: Option<chrono::DateTime<chrono::Utc>>,
    /// A picture for the inbox row, from an `image/*` enclosure. Never
    /// imported.
    pub image_url: Option<String>,
}
