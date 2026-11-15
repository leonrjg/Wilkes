//! A web page kept as its article: one self-contained HTML file.
//!
//! Shared by the two ways a page comes into the library — a feed's post,
//! imported from its inbox, and a page saved from the browser — so both are
//! written the same: the article alone, every picture inside it as a `data:`
//! URI, and where it came from in its head and at its top
//! ([`wilkes_core::feeds::document`]). The viewer fetches nothing from the
//! web, so a picture left as a link would never be shown.
//!
//! Fetching the page itself is the caller's: a feed fetches it paced, the
//! browser through its own session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;
use tracing::warn;
use wilkes_core::feeds::article::{self, Article, SiteTemplate};
use wilkes_core::feeds::document::{self, Picture, Provenance};
use wilkes_core::feeds::identity;
use wilkes_core::network::{ProviderHttpClient, ProviderHttpError};

use super::feeds::{Cancelled, RequestGate};
use super::AppContext;

pub(crate) fn http_client() -> ProviderHttpClient {
    ProviderHttpClient::new("feeds")
}

pub(crate) fn request_headers() -> [(&'static str, String); 1] {
    [(
        "User-Agent",
        format!("Wilkes/{} (feed reader)", env!("CARGO_PKG_VERSION")),
    )]
}

pub(crate) fn http_message(error: &ProviderHttpError) -> String {
    match error.status {
        Some(status) => format!("HTTP {status}"),
        None => error
            .message
            .lines()
            .next()
            .unwrap_or("request failed")
            .to_string(),
    }
}

/// One GET: through `gate` when the caller paces its requests, at once when
/// it does not; raced against `cancel` either way.
pub(crate) async fn get(
    gate: Option<&RequestGate>,
    url: String,
    cancel: &CancellationToken,
) -> Result<Result<Vec<u8>, ProviderHttpError>, Cancelled> {
    let client = http_client();
    let headers = request_headers();
    let request = client.get_bytes(url, &headers);
    match gate {
        Some(gate) => gate.run(cancel, request).await,
        None => tokio::select! {
            _ = cancel.cancelled() => Err(Cancelled),
            fetched = request => Ok(fetched),
        },
    }
}

/// The page's article, read off the runtime's threads.
pub(crate) async fn read_article(
    page: String,
    link: &str,
    template: Option<Arc<SiteTemplate>>,
    cancel: &CancellationToken,
) -> Result<anyhow::Result<Article>, Cancelled> {
    let link = link.to_string();
    let extracted =
        tokio::task::spawn_blocking(move || article::extract(&page, &link, template.as_deref()));
    let joined = tokio::select! {
        _ = cancel.cancelled() => return Err(Cancelled),
        joined = extracted => joined,
    };
    Ok(match joined {
        Ok(Ok(article)) => Ok(article),
        Ok(Err(error)) => Err(anyhow::anyhow!("{error}")),
        Err(error) => Err(anyhow::anyhow!("extraction failed: {error}")),
    })
}

/// Every picture of the article, fetched. One that cannot be fetched, or is
/// no picture the viewer draws, fails the whole — unless
/// `allow_missing_images`, when it is left out and logged.
pub(crate) async fn fetch_pictures(
    article: &Article,
    link: &str,
    gate: Option<&RequestGate>,
    allow_missing_images: bool,
    cancel: &CancellationToken,
) -> Result<anyhow::Result<HashMap<String, Picture>>, Cancelled> {
    let mut pictures: HashMap<String, Picture> = HashMap::new();
    for address in &article.pictures {
        let failure = match get(gate, address.clone(), cancel).await? {
            Ok(bytes) => match document::picture_mime(&bytes) {
                Some(mime) => {
                    pictures.insert(address.clone(), Picture { mime, bytes });
                    continue;
                }
                None => format!("the picture {address} is not PNG, JPEG, GIF, WebP or SVG"),
            },
            Err(error) => format!(
                "the picture {address} could not be fetched: {}",
                http_message(&error)
            ),
        };
        if allow_missing_images {
            warn!("web article: {failure}; left out of {link}");
        } else {
            return Ok(Err(anyhow::anyhow!(failure)));
        }
    }
    Ok(Ok(pictures))
}

/// The date a page gives itself, when it is a real one.
pub(crate) fn page_date(article: &Article) -> Option<DateTime<Utc>> {
    article
        .published
        .as_deref()
        .and_then(|date| DateTime::parse_from_rfc3339(date).ok())
        .map(|date| date.with_timezone(&Utc))
        .and_then(|date| identity::publication_date(Some(date), None))
}

/// Where the file goes and what it is called.
pub(crate) struct Destination<'a> {
    pub root: &'a Path,
    /// Under `root`, `/`-separated; the root itself when absent.
    pub folder: Option<&'a str>,
    /// The date its name starts with.
    pub date: DateTime<Utc>,
    /// Why a name already taken is told apart: by this, or by a count.
    pub tell_apart_by: Option<&'a str>,
    /// A directory of its own beside the library, emptied afterwards.
    pub staging: PathBuf,
}

impl AppContext {
    /// Write the article as one file and import it into the library.
    pub(crate) async fn write_web_article(
        &self,
        provenance: &Provenance,
        article: &Article,
        pictures: &HashMap<String, Picture>,
        destination: Destination<'_>,
    ) -> anyhow::Result<PathBuf> {
        let settings = self.get_settings().await;
        let html = document::render(
            provenance,
            &document::embed_pictures(&article.html, pictures),
        );
        if settings.max_file_size > 0 && html.len() as u64 > settings.max_file_size {
            anyhow::bail!(
                "the article with its pictures is {:.1} MB, over the {:.1} MB limit above which files are not indexed",
                html.len() as f64 / 1_048_576.0,
                settings.max_file_size as f64 / 1_048_576.0
            );
        }

        let fallback = if provenance.feed.is_some() {
            "Post"
        } else {
            "Page"
        };
        let first = document::file_name(destination.date, provenance.title.as_deref(), fallback);
        let folder = match destination.folder {
            Some(folder) => destination.root.join(folder),
            None => destination.root.to_path_buf(),
        };
        let name = match destination.tell_apart_by {
            Some(id) if folder.join(&first).exists() => document::disambiguated(&first, id),
            Some(_) => first,
            None => {
                let mut name = first.clone();
                let mut count = 2;
                while folder.join(&name).exists() {
                    name = document::disambiguated(&first, &count.to_string());
                    count += 1;
                }
                name
            }
        };
        // Beside uploads, never in it: the server serves uploads as its
        // library, and a file staged there would be in the library before it
        // was imported into it.
        let staging = destination.staging;
        let written = async {
            tokio::fs::create_dir_all(&staging).await?;
            let path = staging.join(&name);
            tokio::fs::write(&path, html.as_bytes()).await?;
            let imported = crate::commands::files::import_files_into_root(
                vec![path],
                destination.root.to_path_buf(),
                destination.folder.map(str::to_string),
                settings.supported_extensions.clone(),
                crate::commands::files::FileImportMode::Move,
            )
            .await?;
            imported
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("the import step returned no file"))
        }
        .await;
        if let Err(error) = tokio::fs::remove_dir_all(&staging).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    "web article: could not clear {}: {error}",
                    staging.display()
                );
            }
        }
        written
    }

    /// A page saved from the browser, kept as a feed's post is: its article
    /// with its pictures inside, into the library root. `page` is the page as
    /// the browser fetched it, through its own session, so a sign-in it
    /// needed carries over; its pictures are fetched here. A picture that
    /// cannot be fetched fails the save and says which.
    pub async fn save_web_page(
        &self,
        link: &str,
        page: &Path,
        root: &Path,
    ) -> anyhow::Result<PathBuf> {
        self.ensure_writable().map_err(anyhow::Error::msg)?;
        let bytes = tokio::fs::read(page).await?;
        let source = String::from_utf8_lossy(&bytes).into_owned();
        // Nothing here is cancelled: a save is one page and its pictures, and
        // each request is bounded by the client's own timeout.
        let cancel = CancellationToken::new();
        let article = read_article(source, link, None, &cancel)
            .await
            .map_err(|Cancelled| anyhow::anyhow!("stopped"))??;
        let pictures = fetch_pictures(&article, link, None, false, &cancel)
            .await
            .map_err(|Cancelled| anyhow::anyhow!("stopped"))??;
        let fetched = Utc::now();
        let published = page_date(&article);
        let provenance = Provenance {
            title: article.title.clone(),
            author: article.byline.clone(),
            link: link.to_string(),
            published,
            feed: None,
            fetched,
            item_id: None,
        };
        self.write_web_article(
            &provenance,
            &article,
            &pictures,
            Destination {
                root,
                folder: None,
                date: published.unwrap_or(fetched),
                tell_apart_by: None,
                staging: self
                    .data_dir
                    .join("page-staging")
                    .join(uuid::Uuid::new_v4().to_string()),
            },
        )
        .await
    }
}
