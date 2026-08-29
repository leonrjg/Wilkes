use std::path::PathBuf;

use crate::generate::{
    GenerationEngine, GenerationRequest, GenerationRuntime, GenerationTimings, StopReason,
};
use crate::models::progress::EmbedProgress;
use crate::types::EmbeddingEngine;

/// What a worker process is for. Replaces the bare `EmbeddingEngine` on the
/// request: once a non-embedding role exists, keying restart decisions on an
/// embedding engine would be a lie.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerRole {
    Embed(EmbeddingEngine),
    Generate(GenerationEngine),
}

impl WorkerRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkerRole::Embed(_) => "embed",
            WorkerRole::Generate(_) => "generate",
        }
    }

    /// The engine name within the role, for status display.
    pub fn engine_str(&self) -> &'static str {
        match self {
            WorkerRole::Embed(engine) => engine.as_str(),
            WorkerRole::Generate(engine) => engine.as_str(),
        }
    }

    pub fn embedding_engine(&self) -> Option<EmbeddingEngine> {
        match self {
            WorkerRole::Embed(engine) => Some(*engine),
            WorkerRole::Generate(_) => None,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
enum TaggedRole {
    Embed(EmbeddingEngine),
    Generate(GenerationEngine),
}

/// Accepts both the tagged form this version writes and the bare engine string
/// older builds wrote, so a host and a worker binary that disagree across an
/// upgrade still speak to each other.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum RoleRepr {
    Tagged(TaggedRole),
    Legacy(EmbeddingEngine),
}

impl serde::Serialize for WorkerRole {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let tagged = match self {
            WorkerRole::Embed(engine) => TaggedRole::Embed(*engine),
            WorkerRole::Generate(engine) => TaggedRole::Generate(*engine),
        };
        tagged.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for WorkerRole {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match RoleRepr::deserialize(deserializer)? {
            RoleRepr::Tagged(TaggedRole::Embed(engine)) => WorkerRole::Embed(engine),
            RoleRepr::Tagged(TaggedRole::Generate(engine)) => WorkerRole::Generate(engine),
            RoleRepr::Legacy(engine) => WorkerRole::Embed(engine),
        })
    }
}

/// Sent from the host to a worker on stdin to name the work and the model it
/// needs.
///
/// Everything here describes *inference*. A worker is handed texts or a
/// prompt, never a corpus: extraction, chunking and index writing stay in the
/// process that owns the settings, because they are what decides the
/// extraction recipe a document is read under. A field that named a root, an
/// index or a chunk size would be an invitation to move that decision back
/// out here, where the configured image analyzer does not exist.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct WorkerRequest {
    #[serde(default = "default_mode")]
    pub mode: String, // "embed", "info" or "generate"
    /// Kept under the legacy `engine` key so a mixed-version host and worker
    /// still parse each other's requests.
    #[serde(rename = "engine")]
    pub role: WorkerRole,
    pub model: String, // HuggingFace model ID
    /// Where model artefacts are cached. One directory for the whole
    /// installation, never a workspace's own: the cache root is what the
    /// embedding-space identity is derived from, so a per-workspace copy would
    /// mint a second identity for the same model.
    ///
    /// Kept under the legacy `data_dir` key so a mixed-version host and worker
    /// still parse each other's requests.
    #[serde(rename = "data_dir")]
    pub model_dir: PathBuf,
    #[serde(default = "default_device")]
    pub device: String, // "auto", "cpu", "mps", "cuda", etc.
    pub texts: Option<Vec<String>>, // Used by "embed" mode
    /// Used by "generate" mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generate: Option<GenerationRequest>,
}

fn default_mode() -> String {
    "embed".to_string()
}

fn default_device() -> String {
    "auto".to_string()
}

/// Out-of-band line the host writes to worker stdin to stop an in-flight
/// generation. Confined to generation mode: embed and info never poll for it,
/// so their framing is untouched.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct CancelSignal {
    pub cancel: bool,
}

/// Lines emitted by the worker to stdout.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub enum WorkerEvent {
    /// Forwarded from the index build progress channel.
    Progress(EmbedProgress),
    /// Embedding vectors returned by the "embed" mode.
    Embeddings(Vec<Vec<f32>>),
    /// Model metadata returned by the "info" mode.
    Info {
        dimension: usize,
        max_seq_length: usize,
    },
    /// One incremental decode step. Emitted only for "generate" mode.
    Token { text: String },
    /// Realized generation device and model-load cost. Emitted before the first
    /// token so the host never has to present a requested "auto" device as if
    /// it were the device actually in use.
    GenerationRuntime(GenerationRuntime),
    /// Timing breakdown for the completed generation, immediately before its
    /// terminal event.
    GenerationMetrics(GenerationTimings),
    /// Terminal event for a generation request, mirroring `Done` for builds.
    /// Carries no text: the text is the concatenation of the `Token` events,
    /// and a second copy would only invite the two to disagree.
    Completion { tokens: usize, stop: StopReason },
    /// Index build completed successfully.
    Done,
    /// Index build failed.
    Error(String),
}

impl WorkerEvent {
    /// Whether this event ends a request. `send_request` relies on this to know
    /// when the pipe is back at a request boundary.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            WorkerEvent::Done | WorkerEvent::Error(_) | WorkerEvent::Completion { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::{Constraint, Sampling};
    use crate::types::EmbeddingEngine;

    fn sample_request() -> WorkerRequest {
        WorkerRequest {
            mode: "embed".to_string(),
            role: WorkerRole::Embed(EmbeddingEngine::Fastembed),
            model: "model".to_string(),
            model_dir: PathBuf::from("data"),
            device: "cpu".to_string(),
            texts: None,
            generate: None,
        }
    }

    #[test]
    fn test_worker_request_serialization() {
        let json = serde_json::to_string(&sample_request()).unwrap();
        let de: WorkerRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(de.mode, "embed");
        assert_eq!(de.model, "model");
        assert_eq!(de.role, WorkerRole::Embed(EmbeddingEngine::Fastembed));
    }

    #[test]
    fn test_worker_request_defaults_on_missing_fields() {
        let mut json = serde_json::to_value(sample_request()).unwrap();
        let obj = json.as_object_mut().unwrap();
        obj.remove("mode");
        obj.remove("device");

        let de: WorkerRequest = serde_json::from_value(json).unwrap();
        assert_eq!(de.mode, "embed");
        assert_eq!(de.device, "auto");
    }

    /// A host that still sends the fields a build request used to carry must
    /// not break the worker it is paired with. Removal is the safe direction
    /// across a version skew — serde ignores what it does not know — and this
    /// is the test that says so out loud.
    #[test]
    fn a_request_carrying_the_removed_build_fields_still_parses() {
        let mut json = serde_json::to_value(sample_request()).unwrap();
        let obj = json.as_object_mut().unwrap();
        obj.insert("root".into(), serde_json::json!("/corpus"));
        obj.insert("index_dir".into(), serde_json::json!("/index"));
        obj.insert("chunk_size".into(), serde_json::json!(600));
        obj.insert("chunk_overlap".into(), serde_json::json!(128));
        obj.insert("paths".into(), serde_json::json!(["/corpus/a.pdf"]));
        obj.insert("supported_extensions".into(), serde_json::json!(["pdf"]));

        let de: WorkerRequest = serde_json::from_value(json).unwrap();
        assert_eq!(de.mode, "embed");
        assert_eq!(de.model, "model");
    }

    #[test]
    fn role_round_trips_for_both_variants() {
        for role in [
            WorkerRole::Embed(EmbeddingEngine::Candle),
            WorkerRole::Embed(EmbeddingEngine::SBERT),
            WorkerRole::Generate(GenerationEngine::Candle),
        ] {
            let json = serde_json::to_string(&role).unwrap();
            let back: WorkerRole = serde_json::from_str(&json).unwrap();
            assert_eq!(role, back, "round trip failed for {json}");
        }
    }

    #[test]
    fn legacy_bare_engine_string_deserializes_as_an_embed_role() {
        // What a pre-generation build wrote for the `engine` field.
        let legacy = serde_json::json!({
            "mode": "embed",
            "root": "root",
            "engine": "Candle",
            "model": "m",
            "data_dir": "data",
            "device": "cpu",
            "paths": null,
            "texts": null,
        });
        let de: WorkerRequest = serde_json::from_value(legacy).unwrap();
        assert_eq!(de.role, WorkerRole::Embed(EmbeddingEngine::Candle));
    }

    #[test]
    fn generation_requests_round_trip_through_the_wire_format() {
        let mut req = sample_request();
        req.mode = "generate".to_string();
        req.role = WorkerRole::Generate(GenerationEngine::Candle);
        req.generate = Some(GenerationRequest {
            system: None,
            prompt: "hello".to_string(),
            max_tokens: Some(16),
            constraint: Constraint::OneOf(vec!["a".to_string()]),
            sampling: Sampling::default(),
        });

        let json = serde_json::to_string(&req).unwrap();
        let de: WorkerRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(de.role, WorkerRole::Generate(GenerationEngine::Candle));
        let generate = de.generate.unwrap();
        assert_eq!(generate.prompt, "hello");
        assert_eq!(generate.max_tokens, Some(16));
    }

    #[test]
    fn unlimited_generation_requests_round_trip_without_a_token_limit() {
        let mut req = sample_request();
        req.mode = "generate".to_string();
        req.role = WorkerRole::Generate(GenerationEngine::Candle);
        req.generate = Some(GenerationRequest {
            system: None,
            prompt: "continue".to_string(),
            max_tokens: None,
            constraint: Constraint::Text {
                stop: vec!["\n".to_string()],
            },
            sampling: Sampling::default(),
        });

        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("max_tokens"));
        let de: WorkerRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(de.generate.unwrap().max_tokens, None);
    }

    #[test]
    fn test_worker_event_serialization() {
        let events = vec![
            WorkerEvent::Done,
            WorkerEvent::Error("fail".to_string()),
            WorkerEvent::Info {
                dimension: 384,
                max_seq_length: 512,
            },
            WorkerEvent::Embeddings(vec![vec![1.0]]),
            WorkerEvent::Token {
                text: "hi".to_string(),
            },
            WorkerEvent::GenerationRuntime(GenerationRuntime {
                requested_device: "auto".to_string(),
                device: "metal".to_string(),
                fallback_reason: None,
                model_load_micros: 1_000,
            }),
            WorkerEvent::GenerationMetrics(GenerationTimings {
                prompt_micros: 100,
                decode_micros: 200,
                constraint_micros: 50,
            }),
            WorkerEvent::Completion {
                tokens: 3,
                stop: StopReason::Eos,
            },
        ];
        for e in events {
            let json = serde_json::to_string(&e).unwrap();
            let _: WorkerEvent = serde_json::from_str(&json).unwrap();
        }
    }

    #[test]
    fn completion_terminates_a_request_but_a_token_does_not() {
        assert!(WorkerEvent::Completion {
            tokens: 1,
            stop: StopReason::Eos
        }
        .is_terminal());
        assert!(WorkerEvent::Done.is_terminal());
        assert!(WorkerEvent::Error("e".into()).is_terminal());
        assert!(!WorkerEvent::Token { text: "t".into() }.is_terminal());
        assert!(!WorkerEvent::Embeddings(vec![]).is_terminal());
    }

    #[test]
    fn cancel_signal_is_a_single_line() {
        let line = serde_json::to_string(&CancelSignal { cancel: true }).unwrap();
        assert_eq!(line, r#"{"cancel":true}"#);
        assert!(serde_json::from_str::<WorkerRequest>(&line).is_err());
    }
}
