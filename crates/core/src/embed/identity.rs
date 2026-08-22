use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::types::{ByteRange, EmbeddingEngine, SourceOrigin};

pub const IDENTITY_SCHEMA_VERSION: u32 = 1;
pub const PASSAGE_INPUT_RECIPE: &str = "wilkes-passage-input-v1";
pub const POOLING_NORMALIZATION_RECIPE: &str = "engine-native-pooling+l2-output-v1";
/// v2 is the sanitized reading: line-wrapped words joined, page furniture
/// removed, marginalia moved out of the reading order, and PDF outline entries
/// anchored at a byte offset rather than a page. It changes
/// `ExtractionRecipe::id()`, hence rendition identity, hence
/// `extracted_content_sha256` — so every managed document re-extracts and
/// re-embeds rather than a v1 reading being mixed with a v2 one.
pub const EXTRACTOR_RECIPE_VERSION: &str = "wilkes-extractors-v2";
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
    /// Versioned artifact identity. Engines whose public model selector is an
    /// immutable catalog entry use that entry plus Wilkes' runtime epoch. If an
    /// engine later permits mutable revisions, its Embedder implementation must
    /// override `embedding_space_identity` with the resolved revision/digest.
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

    pub fn is_locally_compatible_with(&self, runtime: &EmbeddingSpaceIdentity) -> bool {
        match &self.exact_identity {
            Some(exact) => exact == runtime,
            None => {
                self.engine == runtime.engine
                    && self.model_id == runtime.model_id
                    && self.dimension == runtime.dimension
            }
        }
    }
}

impl EmbeddingSpaceIdentity {
    pub fn for_runtime(engine: EmbeddingEngine, model_id: &str, dimension: usize) -> Self {
        Self::with_artifact_revision(
            engine,
            model_id,
            dimension,
            format!("unresolved-runtime-v1:{}:{}", engine.as_str(), model_id),
        )
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

    pub fn id(&self) -> EmbeddingSpaceId {
        EmbeddingSpaceId(tagged_hash("space", &canonical_json(self)))
    }
}

/// Fingerprint the resolved model snapshot, including tokenizer, auxiliary
/// prefixes, and pooling configuration. If an engine has not materialized its
/// cache yet, use a persisted installation epoch; once files appear, the next
/// runtime gets their content fingerprint and refuses the old index.
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

    let epochs = cache_root.join("embedding-identity-epochs");
    std::fs::create_dir_all(&epochs)?;
    let epoch_path = epochs.join(format!("{}.txt", sha256_bytes(repo_id.as_bytes())));
    let epoch = if epoch_path.exists() {
        std::fs::read_to_string(&epoch_path)?
    } else {
        let epoch = uuid::Uuid::new_v4().to_string();
        let temporary = epoch_path.with_extension("tmp");
        std::fs::write(&temporary, &epoch)?;
        match std::fs::rename(&temporary, &epoch_path) {
            Ok(()) => epoch,
            Err(_error) if epoch_path.exists() => {
                let _ = std::fs::remove_file(temporary);
                std::fs::read_to_string(&epoch_path)?
            }
            Err(error) => return Err(error.into()),
        }
    };
    Ok(format!("installation-epoch:{}", epoch.trim()))
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
        }
    }

    /// Exact per-document extractor selection. Source bytes alone do not
    /// encode the media type in every supported format, so the selected
    /// extractor is part of rendition compatibility rather than inferred from
    /// a matching digest.
    pub fn for_path(path: &std::path::Path, chunk_size: usize, chunk_overlap: usize) -> Self {
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

    #[test]
    fn legacy_metadata_allows_local_tuple_match_without_claiming_exact_identity() {
        let runtime = EmbeddingSpaceIdentity::with_artifact_revision(
            EmbeddingEngine::Candle,
            "model",
            384,
            "artifact-sha256:current".to_string(),
        );
        let legacy = IndexEmbeddingMetadata::legacy(EmbeddingEngine::Candle, "model", 384);

        assert!(legacy.is_locally_compatible_with(&runtime));
        assert!(legacy.exact_identity.is_none());
        let json = serde_json::to_value(&legacy).unwrap();
        assert!(json["exact_identity"].is_null());
        assert_eq!(
            serde_json::from_value::<IndexEmbeddingMetadata>(json).unwrap(),
            legacy
        );
        assert!(
            !IndexEmbeddingMetadata::legacy(EmbeddingEngine::Fastembed, "model", 384)
                .is_locally_compatible_with(&runtime)
        );
    }

    #[test]
    fn exact_metadata_requires_the_full_runtime_identity_even_for_local_use() {
        let recorded = EmbeddingSpaceIdentity::with_artifact_revision(
            EmbeddingEngine::Candle,
            "model",
            384,
            "artifact-sha256:old".to_string(),
        );
        let current = EmbeddingSpaceIdentity::with_artifact_revision(
            EmbeddingEngine::Candle,
            "model",
            384,
            "artifact-sha256:new".to_string(),
        );

        assert!(!IndexEmbeddingMetadata::exact(recorded).is_locally_compatible_with(&current));
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

    #[test]
    fn artifact_revision_tracks_cache_content_and_missing_cache_epoch_is_stable() {
        let cache = tempfile::tempdir().unwrap();
        let first_epoch = artifact_revision_for_cache(cache.path(), "org/missing").unwrap();
        let second_epoch = artifact_revision_for_cache(cache.path(), "org/missing").unwrap();
        assert_eq!(first_epoch, second_epoch);

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
