use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::types::{ByteRange, EmbeddingEngine, SourceOrigin};

pub const IDENTITY_SCHEMA_VERSION: u32 = 1;
/// Marks an artifact revision that names only the requested model, not the
/// files that produced the vectors. One owner for the rule, so the index
/// creation guard and the legacy migration cannot drift apart.
pub const UNRESOLVED_ARTIFACT_REVISION_PREFIX: &str = "unresolved-runtime-";
pub const PASSAGE_INPUT_RECIPE: &str = "wilkes-passage-input-v1";
pub const POOLING_NORMALIZATION_RECIPE: &str = "engine-native-pooling+l2-output-v1";
/// v2 is the sanitized reading: line-wrapped words joined, page furniture
/// removed, marginalia moved out of the reading order, and PDF outline entries
/// anchored at a byte offset rather than a page. It changes
/// `ExtractionRecipe::id()`, hence rendition identity, hence
/// `extracted_content_sha256` — so every managed document re-extracts and
/// re-embeds rather than a v1 reading being mixed with a v2 one.
/// Bumped whenever an extractor's output changes for the same source, so a
/// rendition produced by an older build stops claiming this runtime would
/// reproduce it.
///
/// v3: a typeset region no longer declares a structural boundary unless its
/// kind is line-structured (`serialize::is_structural_block`). The reading's
/// bytes are unchanged — a display formula still opens its own line and
/// carries its label — but the passages cut from them are not, so a v2
/// rendition and a v3 one are different renditions of the same reading.
pub const EXTRACTOR_RECIPE_VERSION: &str = "wilkes-extractors-v3";
pub const CHUNKER_RECIPE_VERSION: &str = "text-splitter-0.27-trim-v1";

macro_rules! opaque_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

opaque_id!(EmbeddingSpaceId);
opaque_id!(DocumentSnapshotId);
opaque_id!(RenditionId);
opaque_id!(ChunkRef);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingSpaceIdentity {
    pub identity_schema_version: u32,
    pub engine: EmbeddingEngine,
    pub model_id: String,
    /// Which artifacts produced these vectors, where the runtime could tell.
    ///
    /// Recorded provenance, not identity: [`Self::id`] does not hash it, so it
    /// never decides whether two vectors may be compared. Show it, log it,
    /// compare it when diagnosing a surprise — do not refuse over it. An
    /// engine that cannot fingerprint its weights records an unresolved
    /// marker rather than inventing a value.
    pub artifact_revision: String,
    pub dimension: usize,
    pub passage_input_recipe: String,
    pub pooling_normalization_recipe: String,
}

/// The embedding evidence recorded by an index.
///
/// Every index has enough display metadata for Wilkes to preserve its legacy
/// local-search behavior. Only indexes created by a runtime that recorded the
/// complete coordinate-system identity carry `exact_identity`; managed-corpus
/// operations must require that stronger evidence before copying vectors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEmbeddingMetadata {
    pub engine: EmbeddingEngine,
    pub model_id: String,
    pub dimension: usize,
    #[serde(default)]
    pub exact_identity: Option<EmbeddingSpaceIdentity>,
}

impl IndexEmbeddingMetadata {
    pub fn exact(identity: EmbeddingSpaceIdentity) -> Self {
        Self {
            engine: identity.engine,
            model_id: identity.model_id.clone(),
            dimension: identity.dimension,
            exact_identity: Some(identity),
        }
    }

    pub fn legacy(engine: EmbeddingEngine, model_id: impl Into<String>, dimension: usize) -> Self {
        Self {
            engine,
            model_id: model_id.into(),
            dimension,
            exact_identity: None,
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(identity) = &self.exact_identity {
            anyhow::ensure!(
                identity.engine == self.engine
                    && identity.model_id == self.model_id
                    && identity.dimension == self.dimension,
                "Index embedding metadata contradicts its exact identity"
            );
        }
        Ok(())
    }
}

impl EmbeddingSpaceIdentity {
    /// A placeholder identity: names the model but not the artifacts that
    /// produced the vectors. Only the legacy-index migration and the managed
    /// configuration check mint these; an embedder must never claim one, and
    /// an index must never record one as its exact identity.
    pub fn for_runtime(engine: EmbeddingEngine, model_id: &str, dimension: usize) -> Self {
        Self::with_artifact_revision(
            engine,
            model_id,
            dimension,
            format!(
                "{UNRESOLVED_ARTIFACT_REVISION_PREFIX}v1:{}:{}",
                engine.as_str(),
                model_id
            ),
        )
    }

    /// A resolved-shaped identity for tests, which have no model cache to
    /// fingerprint. Distinct from every real artifact revision.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn for_test(engine: EmbeddingEngine, model_id: &str, dimension: usize) -> Self {
        Self::with_artifact_revision(
            engine,
            model_id,
            dimension,
            format!("test-artifact-v1:{}:{}", engine.as_str(), model_id),
        )
    }

    /// Whether this identity names the artifacts that produced the vectors,
    /// rather than only the model that was requested.
    pub fn is_resolved(&self) -> bool {
        !self
            .artifact_revision
            .starts_with(UNRESOLVED_ARTIFACT_REVISION_PREFIX)
    }

    pub fn with_artifact_revision(
        engine: EmbeddingEngine,
        model_id: &str,
        dimension: usize,
        artifact_revision: String,
    ) -> Self {
        Self {
            identity_schema_version: IDENTITY_SCHEMA_VERSION,
            engine,
            model_id: model_id.to_string(),
            artifact_revision,
            dimension,
            passage_input_recipe: PASSAGE_INPUT_RECIPE.to_string(),
            pooling_normalization_recipe: POOLING_NORMALIZATION_RECIPE.to_string(),
        }
    }

    /// The fields that decide whether two vectors may be compared, and
    /// nothing else.
    ///
    /// A space id answers one question — can a vector from over there be read
    /// against a vector from over here — and only the engine, the model and
    /// the width bear on it. Everything else this struct records is
    /// provenance: worth knowing, worth showing, and not worth refusing over.
    ///
    /// It used to hash the whole struct. That made `artifact_revision` — a
    /// content fingerprint of a model cache, or an `installation-epoch` UUID
    /// when the cache had not materialized — decide whether two indexes of the
    /// same model at the same width could share a vector. They could not,
    /// across a reinstall or a second machine, and nothing said why: the id is
    /// a hash, so a mismatch names no field. `identity_schema_version` was in
    /// there too, which made every future schema bump a scheduled invalidation
    /// of every index on disk.
    ///
    /// The input and pooling recipes came out with them. They are constants
    /// that have never varied, so they only ever added ways to differ.
    ///
    /// What this gives up: weights swapped under an unchanged model name are
    /// no longer detected, and vectors from before the swap read as
    /// comparable. What it buys is that reuse works at all — across machines,
    /// across reinstalls, across every index already on disk. Vectors are a
    /// cache; the cost of a wrong one is a worse neighbour, recomputable at
    /// will, and never a change to anything a citation points at.
    pub fn id(&self) -> EmbeddingSpaceId {
        EmbeddingSpaceId(tagged_hash(
            "space",
            &canonical_json(&serde_json::json!({
                "engine": self.engine,
                "model_id": self.model_id,
                "dimension": self.dimension,
            })),
        ))
    }
}

/// Fingerprint the resolved model snapshot, including tokenizer, auxiliary
/// prefixes, and pooling configuration, when the weights are under this cache
/// root. When they are not, say so: the answer is provenance, and an unknown
/// provenance is reported as unknown rather than stood in for.
pub fn artifact_revision_for_cache(
    cache_root: &std::path::Path,
    repo_id: &str,
) -> anyhow::Result<String> {
    let repo_dir = cache_root.join(format!("models--{}", repo_id.replace('/', "--")));
    let mut files = Vec::new();
    if repo_dir.exists() {
        let mut pending = vec![repo_dir.clone()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory)? {
                let entry = entry?;
                let file_type = entry.file_type()?;
                if file_type.is_dir() {
                    pending.push(entry.path());
                } else if file_type.is_file() {
                    files.push(entry.path());
                }
            }
        }
    }
    files.sort();
    if !files.is_empty() {
        let mut digest = Sha256::new();
        for path in files {
            let relative = path.strip_prefix(&repo_dir)?;
            digest.update(relative.to_string_lossy().as_bytes());
            digest.update([0]);
            digest.update(std::fs::read(&path)?);
            digest.update([0]);
        }
        return Ok(format!("artifact-sha256:{}", hex_digest(digest.finalize())));
    }

    // An engine whose weights are not under this cache root — SBERT keeps its
    // own venv — has no fingerprint to give, and says so. It used to mint a
    // random UUID and persist it as an "installation epoch", which read as
    // evidence, differed on every machine and every reinstall, and was hashed
    // into the space id: a value invented to stand for "unknown" that then
    // decided vectors were incomparable. Provenance may be unknown. It may not
    // be fabricated.
    tracing::debug!(
        "no model artifacts under {} for {repo_id}; recording an unresolved artifact revision",
        cache_root.display()
    );
    Ok(format!(
        "{UNRESOLVED_ARTIFACT_REVISION_PREFIX}cache-not-materialized"
    ))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionRecipe {
    pub identity_schema_version: u32,
    pub extractor_recipe_version: String,
    pub selected_extractor: String,
    pub chunker_recipe_version: String,
    pub chunk_size: usize,
    pub chunk_overlap: usize,
    pub locator_schema_version: String,
    /// The image analyzer the reading was produced under: models, revisions,
    /// prompts, admission thresholds, technical limits and the serialization
    /// of the enrichment block, as the analyzer names itself.
    ///
    /// Absent from the hash when it is empty, which is what a runtime with no
    /// analyzer configured has to declare — so installing a recognizer changes
    /// the recipe and forces re-extraction, while a runtime that never had one
    /// keeps the identity it already had.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub image_analyzer_recipe: String,
}

impl ExtractionRecipe {
    pub fn new(chunk_size: usize, chunk_overlap: usize) -> Self {
        Self {
            identity_schema_version: IDENTITY_SCHEMA_VERSION,
            extractor_recipe_version: EXTRACTOR_RECIPE_VERSION.to_string(),
            selected_extractor: "registry-auto-detect-v1".to_string(),
            chunker_recipe_version: CHUNKER_RECIPE_VERSION.to_string(),
            chunk_size,
            chunk_overlap,
            locator_schema_version: "source-origin-v1".to_string(),
            image_analyzer_recipe: String::new(),
        }
    }

    /// Exact per-document extractor selection, from the registry that will do
    /// the extracting.
    ///
    /// The registry is a parameter rather than the path alone because the
    /// analyzer is part of what produces the bytes, and the registry is what
    /// holds it. A recipe derived without it would describe a reading nobody
    /// produced — which is precisely how two consumers come to disagree about
    /// what a document says.
    ///
    /// Source bytes alone do not encode the media type in every supported
    /// format, so the selected extractor is part of rendition compatibility
    /// rather than inferred from a matching digest.
    pub fn for_path(
        path: &std::path::Path,
        extractors: &crate::extract::ExtractorRegistry,
        chunk_size: usize,
        chunk_overlap: usize,
    ) -> Self {
        let mut recipe = Self::new(chunk_size, chunk_overlap);
        recipe.selected_extractor = match path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "pdf" => "pdf-mupdf-v1",
            _ => "plain-text-v1",
        }
        .to_string();
        recipe.image_analyzer_recipe = extractors.image_analyzer_identity().to_string();
        recipe
    }

    /// The recipe a corpus is described by, with the analyzer the runtime is
    /// configured with. Path-independent: it answers "what would this runtime
    /// produce", not "what did this file produce".
    pub fn for_runtime(
        extractors: &crate::extract::ExtractorRegistry,
        chunk_size: usize,
        chunk_overlap: usize,
    ) -> Self {
        let mut recipe = Self::new(chunk_size, chunk_overlap);
        recipe.image_analyzer_recipe = extractors.image_analyzer_identity().to_string();
        recipe
    }

    pub fn id(&self) -> String {
        tagged_hash("extract", &canonical_json(self))
    }
}

#[derive(Serialize)]
struct RenditionDescriptor<'a> {
    identity_schema_version: u32,
    snapshot_id: &'a str,
    extraction_recipe_id: &'a str,
    chunks: &'a [ChunkDescriptor],
}

#[derive(Clone, Debug, Serialize)]
pub struct ChunkDescriptor {
    pub ordinal: usize,
    pub text_sha256: String,
    pub byte_range: ByteRange,
    pub origin: SourceOrigin,
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes))
}

pub fn sha256_file(path: &std::path::Path) -> anyhow::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex_digest(digest.finalize()))
}

pub fn snapshot_id(source_sha256: &str) -> DocumentSnapshotId {
    DocumentSnapshotId(tagged_hash(
        "snapshot",
        &format!("{IDENTITY_SCHEMA_VERSION}\0{source_sha256}"),
    ))
}

pub fn rendition_id(
    snapshot_id: &DocumentSnapshotId,
    extraction_recipe_id: &str,
    chunks: &[ChunkDescriptor],
) -> RenditionId {
    RenditionId(tagged_hash(
        "rendition",
        &canonical_json(&RenditionDescriptor {
            identity_schema_version: IDENTITY_SCHEMA_VERSION,
            snapshot_id: snapshot_id.as_str(),
            extraction_recipe_id,
            chunks,
        }),
    ))
}

pub fn chunk_ref(rendition_id: &RenditionId, ordinal: usize) -> ChunkRef {
    ChunkRef(tagged_hash(
        "chunk",
        &format!(
            "{IDENTITY_SCHEMA_VERSION}\0{}\0{ordinal}",
            rendition_id.as_str()
        ),
    ))
}

fn canonical_json(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("identity values are serializable")
}

fn tagged_hash(tag: &str, value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(tag.as_bytes());
    digest.update([0]);
    digest.update(value.as_bytes());
    format!("{tag}-{}", hex_digest(digest.finalize()))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_fingerprinted_artifact_revision_counts_as_resolved() {
        assert!(
            !EmbeddingSpaceIdentity::for_runtime(EmbeddingEngine::Candle, "model", 384)
                .is_resolved()
        );
        assert!(EmbeddingSpaceIdentity::with_artifact_revision(
            EmbeddingEngine::Candle,
            "model",
            384,
            "artifact-sha256:abc".to_string(),
        )
        .is_resolved());
        assert!(EmbeddingSpaceIdentity::with_artifact_revision(
            EmbeddingEngine::Candle,
            "model",
            384,
            "installation-epoch:abc".to_string(),
        )
        .is_resolved());
        assert!(
            EmbeddingSpaceIdentity::for_test(EmbeddingEngine::Candle, "model", 384).is_resolved()
        );
    }

    #[test]
    fn identity_changes_with_every_coordinate_defining_display_input() {
        let base = EmbeddingSpaceIdentity::for_runtime(EmbeddingEngine::Candle, "model", 384);
        assert_ne!(
            base.id(),
            EmbeddingSpaceIdentity::for_runtime(EmbeddingEngine::Fastembed, "model", 384).id()
        );
        assert_ne!(
            base.id(),
            EmbeddingSpaceIdentity::for_runtime(EmbeddingEngine::Candle, "other", 384).id()
        );
        assert_ne!(
            base.id(),
            EmbeddingSpaceIdentity::for_runtime(EmbeddingEngine::Candle, "model", 768).id()
        );
    }

    /// Legacy metadata still round-trips with no exact identity.
    ///
    /// What it no longer carries is a *second* compatibility rule.
    /// `is_locally_compatible_with` existed so a legacy index could be read
    /// locally on an engine/model/dimension match while being refused for
    /// managed reuse on a full-struct match — two verdicts about one index,
    /// which is how an index came to be usable and unusable at the same time.
    /// `validate_embedding_space` now compares space ids, which *is* the
    /// engine/model/dimension rule, so there is one verdict and this is gone.
    #[test]
    fn legacy_metadata_round_trips_without_claiming_exact_identity() {
        let legacy = IndexEmbeddingMetadata::legacy(EmbeddingEngine::Candle, "model", 384);
        assert!(legacy.exact_identity.is_none());
        let json = serde_json::to_value(&legacy).unwrap();
        assert!(json["exact_identity"].is_null());
        assert_eq!(
            serde_json::from_value::<IndexEmbeddingMetadata>(json).unwrap(),
            legacy
        );
    }

    #[test]
    fn duplicate_text_occurrences_have_distinct_chunk_refs() {
        let snapshot = snapshot_id(&sha256_bytes(b"same same"));
        let descriptor = |ordinal| ChunkDescriptor {
            ordinal,
            text_sha256: sha256_bytes(b"same"),
            byte_range: ByteRange {
                start: ordinal * 5,
                end: ordinal * 5 + 4,
            },
            origin: SourceOrigin::TextFile { line: 1, col: 1 },
        };
        let rendition = rendition_id(&snapshot, "recipe", &[descriptor(0), descriptor(1)]);
        assert_ne!(chunk_ref(&rendition, 0), chunk_ref(&rendition, 1));
    }

    /// A space id answers whether two vectors may be compared, so it moves
    /// only when that answer moves.
    #[test]
    fn provenance_does_not_decide_comparability() {
        let base = EmbeddingSpaceIdentity::with_artifact_revision(
            EmbeddingEngine::Candle,
            "org/model",
            384,
            "artifact-sha256:aaa".to_string(),
        );

        // The same model at the same width, fingerprinted differently: a
        // reinstall, a second machine, or weights that live outside the cache
        // root. Every one of these used to mint a different space and lose all
        // reuse against it.
        for revision in [
            "artifact-sha256:bbb",
            "installation-epoch:8f14e45f-ea8f-4b1a-9c7c-6d7f2c3b4a5e",
            "unresolved-runtime-cache-not-materialized",
        ] {
            let other = EmbeddingSpaceIdentity {
                artifact_revision: revision.to_string(),
                ..base.clone()
            };
            assert_eq!(base.id(), other.id(), "artifact revision {revision}");
        }

        // Constants that have never varied, and a schema version whose next
        // bump would otherwise invalidate every index on disk.
        let drifted = EmbeddingSpaceIdentity {
            identity_schema_version: base.identity_schema_version + 7,
            passage_input_recipe: "some-other-input-recipe".to_string(),
            pooling_normalization_recipe: "some-other-pooling".to_string(),
            ..base.clone()
        };
        assert_eq!(base.id(), drifted.id());
    }

    /// And it moves for all three things that do change what a vector means.
    #[test]
    fn engine_model_and_width_still_separate_spaces() {
        let base = EmbeddingSpaceIdentity::for_test(EmbeddingEngine::Candle, "org/model", 384);
        assert_ne!(
            base.id(),
            EmbeddingSpaceIdentity::for_test(EmbeddingEngine::Candle, "org/other", 384).id()
        );
        assert_ne!(
            base.id(),
            EmbeddingSpaceIdentity::for_test(EmbeddingEngine::Candle, "org/model", 768).id()
        );
        assert_ne!(
            base.id(),
            EmbeddingSpaceIdentity::for_test(EmbeddingEngine::Fastembed, "org/model", 384).id()
        );
    }

    #[test]
    fn artifact_revision_tracks_cache_content_and_says_so_when_there_is_none() {
        let cache = tempfile::tempdir().unwrap();
        // No weights under this root, so there is nothing to fingerprint. The
        // answer says that, and says the same thing on the next machine and
        // after the next reinstall — where a minted epoch said something
        // different every time, and was hashed into the space id.
        let missing = artifact_revision_for_cache(cache.path(), "org/missing").unwrap();
        assert_eq!(missing, "unresolved-runtime-cache-not-materialized");
        assert_eq!(
            missing,
            artifact_revision_for_cache(cache.path(), "org/missing").unwrap()
        );

        let snapshot = cache
            .path()
            .join("models--org--model")
            .join("snapshots")
            .join("revision");
        std::fs::create_dir_all(&snapshot).unwrap();
        let weights = snapshot.join("model.bin");
        std::fs::write(&weights, b"one").unwrap();
        let first = artifact_revision_for_cache(cache.path(), "org/model").unwrap();
        std::fs::write(&weights, b"two").unwrap();
        let second = artifact_revision_for_cache(cache.path(), "org/model").unwrap();
        assert_ne!(first, second);
    }
}
