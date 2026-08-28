//! Durable storage of what analysis established about one image.
//!
//! Image analysis is versioned extraction, not live generation: the same image
//! under the same recipe has the same answer forever, and re-reading a
//! document must not re-run a recognizer over its artwork. What is cached is
//! the annotation — text, geometry, admission and description — never the
//! pixels, which are already in the PDF.
//!
//! Every input that can change the answer is in the key, so there is no such
//! thing as a stale hit: a new model, prompt, threshold, coordinate mapping or
//! serialization produces a different key, the old entry is simply never asked
//! for again, and the document re-extracts and re-embeds.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::types::{ImageAnalysisStatus, ImageDescription, ImageOcrRegion};

/// Bumped when the stored shape changes. An entry written under another
/// version is not read, because a field that moved would be read as a field
/// that means something else.
const CACHE_FORMAT_VERSION: &str = "image-annotation-v1";

/// What analysis established about one image.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Annotation {
    pub ocr_regions: Vec<ImageOcrRegion>,
    pub description: Option<ImageDescription>,
    pub status: ImageAnalysisStatus,
}

impl Annotation {
    /// Whether this annotation is worth keeping.
    ///
    /// A partial result is not: the recognizer that failed may succeed on the
    /// next run, and caching the failure would make one bad run permanent.
    /// A complete analysis that found nothing *is* cached — "no text in this
    /// logo" is an answer, and re-deriving it for every logo in a library is
    /// the cost this cache exists to remove.
    pub fn is_durable(&self) -> bool {
        matches!(self.status, ImageAnalysisStatus::Complete)
    }
}

/// The identity of one analyzed image: everything that decides its annotation.
///
/// The digest is of the *decoded pixels*, which is what was analyzed. The
/// source document's own digest is deliberately absent: two documents that
/// draw the same pixels at the same place on the page have the same answer,
/// and keying on the file as well would only stop them sharing it while
/// costing a full read of every PDF to compute. A SHA-256 of the pixels plus
/// the placement plus the recipe names the analyzed thing exactly.
pub struct AnnotationKey {
    pub analyzer_identity: String,
    pub page: u32,
    pub image_sha256: String,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub bbox: crate::types::BoundingBox,
    pub transform: crate::types::ImageTransform,
}

impl AnnotationKey {
    fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        // Field-separated so no two different keys can concatenate alike.
        for field in [
            CACHE_FORMAT_VERSION.to_string(),
            self.analyzer_identity.clone(),
            self.page.to_string(),
            self.image_sha256.clone(),
            format!("{}x{}", self.pixel_width, self.pixel_height),
            // Fixed precision: a placement is in points, and four decimals is
            // far finer than any coordinate a renderer distinguishes. A raw
            // float would make the key depend on formatting.
            format!(
                "{:.4},{:.4},{:.4},{:.4}",
                self.bbox.x, self.bbox.y, self.bbox.width, self.bbox.height
            ),
            format!(
                "{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}",
                self.transform.a,
                self.transform.b,
                self.transform.c,
                self.transform.d,
                self.transform.e,
                self.transform.f
            ),
        ] {
            hasher.update(field.as_bytes());
            hasher.update([0]);
        }
        format!("{:x}", hasher.finalize())
    }
}

/// A directory of annotations, one file each.
///
/// A miss, an unreadable entry and a malformed entry are all the same thing —
/// analysis has to run — so none of them is an error the caller has to handle.
/// They are logged, because a cache that never hits is a fact worth seeing.
pub struct AnnotationCache {
    root: PathBuf,
}

impl AnnotationCache {
    /// The cache under `data_dir`, created if it is not there yet.
    pub fn open(data_dir: &Path) -> anyhow::Result<Self> {
        let root = data_dir.join("image-annotations");
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn path(&self, key: &AnnotationKey) -> PathBuf {
        self.root.join(format!("{}.json", key.digest()))
    }

    pub fn get(&self, key: &AnnotationKey) -> Option<Annotation> {
        let path = self.path(key);
        let bytes = std::fs::read(&path).ok()?;
        match serde_json::from_slice::<Annotation>(&bytes) {
            Ok(annotation) => Some(annotation),
            Err(error) => {
                warn!("discarding unreadable image annotation {path:?}: {error}");
                let _ = std::fs::remove_file(&path);
                None
            }
        }
    }

    pub fn put(&self, key: &AnnotationKey, annotation: &Annotation) {
        if !annotation.is_durable() {
            debug!("not caching a partial image analysis");
            return;
        }
        let path = self.path(key);
        // Written beside and renamed, so a reader never sees half an entry.
        let temporary = path.with_extension("json.tmp");
        let write = serde_json::to_vec(annotation)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| Ok(std::fs::write(&temporary, bytes)?))
            .and_then(|()| Ok(std::fs::rename(&temporary, &path)?));
        if let Err(error) = write {
            warn!("could not cache image annotation {path:?}: {error}");
            let _ = std::fs::remove_file(&temporary);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BoundingBox, ImageTransform, OcrAdmission, Point};
    use tempfile::tempdir;

    fn key(analyzer: &str, digest: &str) -> AnnotationKey {
        AnnotationKey {
            analyzer_identity: analyzer.to_string(),
            page: 18,
            image_sha256: digest.to_string(),
            pixel_width: 1559,
            pixel_height: 499,
            bbox: BoundingBox {
                x: 138.0,
                y: 248.0,
                width: 375.0,
                height: 120.0,
            },
            transform: ImageTransform {
                a: 375.0,
                b: 0.0,
                c: 0.0,
                d: 120.0,
                e: 138.0,
                f: 248.0,
            },
        }
    }

    fn annotation(text: &str, status: ImageAnalysisStatus) -> Annotation {
        Annotation {
            ocr_regions: vec![ImageOcrRegion {
                text: text.to_string(),
                confidence: 0.9,
                polygon_within_image: vec![Point { x: 1.0, y: 2.0 }],
                page_polygon: vec![Point { x: 3.0, y: 4.0 }],
                admission: OcrAdmission::Accepted,
            }],
            description: None,
            status,
        }
    }

    #[test]
    fn an_annotation_survives_a_round_trip() {
        let dir = tempdir().unwrap();
        let cache = AnnotationCache::open(dir.path()).expect("opens");
        let key = key("analyzer-v1", "pixels");

        assert!(cache.get(&key).is_none());
        cache.put(&key, &annotation("Knowledge base", ImageAnalysisStatus::Complete));
        let found = cache.get(&key).expect("hit");
        assert_eq!(found.ocr_regions[0].text, "Knowledge base");
        assert_eq!(found.ocr_regions[0].page_polygon[0], Point { x: 3.0, y: 4.0 });
    }

    /// The point of the key. A different recipe is a different answer, so it
    /// must not be able to read the old one — that is what makes a model,
    /// prompt or threshold change force re-extraction rather than silently
    /// reuse what the previous recipe decided.
    #[test]
    fn a_different_recipe_never_reads_the_previous_recipes_answer() {
        let dir = tempdir().unwrap();
        let cache = AnnotationCache::open(dir.path()).expect("opens");
        cache.put(
            &key("analyzer-v1", "pixels"),
            &annotation("old", ImageAnalysisStatus::Complete),
        );

        assert!(cache.get(&key("analyzer-v2", "pixels")).is_none());
        assert!(cache.get(&key("analyzer-v1", "other pixels")).is_none());

        let mut moved = key("analyzer-v1", "pixels");
        moved.page = 19;
        assert!(cache.get(&moved).is_none());

        let mut resized = key("analyzer-v1", "pixels");
        resized.transform.a = 400.0;
        assert!(cache.get(&resized).is_none());
    }

    /// A run where the recognizer failed must not become the permanent answer.
    #[test]
    fn a_partial_analysis_is_not_made_permanent() {
        let dir = tempdir().unwrap();
        let cache = AnnotationCache::open(dir.path()).expect("opens");
        let key = key("analyzer-v1", "pixels");
        cache.put(
            &key,
            &annotation(
                "half read",
                ImageAnalysisStatus::Partial {
                    failures: vec!["recognition: decoder error".to_string()],
                },
            ),
        );
        assert!(cache.get(&key).is_none());
    }

    /// An image that genuinely has no text in it is an answer, and one worth
    /// keeping: a library's repeated logos are re-met on every extraction.
    #[test]
    fn a_complete_analysis_that_found_nothing_is_still_cached() {
        let dir = tempdir().unwrap();
        let cache = AnnotationCache::open(dir.path()).expect("opens");
        let key = key("analyzer-v1", "logo");
        cache.put(
            &key,
            &Annotation {
                ocr_regions: Vec::new(),
                description: None,
                status: ImageAnalysisStatus::Complete,
            },
        );
        let found = cache.get(&key).expect("hit");
        assert!(found.ocr_regions.is_empty());
    }

    #[test]
    fn a_corrupt_entry_is_discarded_rather_than_returned() {
        let dir = tempdir().unwrap();
        let cache = AnnotationCache::open(dir.path()).expect("opens");
        let key = key("analyzer-v1", "pixels");
        std::fs::write(cache.path(&key), b"{not json").unwrap();
        assert!(cache.get(&key).is_none());
        assert!(!cache.path(&key).exists(), "the bad entry is removed");
    }
}
