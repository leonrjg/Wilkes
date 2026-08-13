//! Local text generation for short, bounded, verifiable outputs.
//!
//! Sits between raw embeddings and the agent subprocess: everything here
//! produces at most a couple of sentences under a constraint that makes
//! malformed output unrepresentable rather than merely unlikely.
//!
//! See `docs/internal/specs/generation-engine.md`.

pub mod grammar;
pub mod tasks;

#[cfg(feature = "generate")]
pub mod engines;
#[cfg(feature = "generate")]
pub mod worker;

use std::ops::ControlFlow;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub trait Generator: Send + Sync {
    /// The primitive. `sink` is called once per decoded token; returning
    /// `ControlFlow::Break` stops the decode and yields `StopReason::Cancelled`.
    fn generate_stream(
        &self,
        req: GenerationRequest,
        sink: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> anyhow::Result<Generated>;

    /// Convenience for non-streaming callers. The default impl collects the
    /// stream, so there is exactly one decode implementation, never two.
    fn generate(&self, req: GenerationRequest) -> anyhow::Result<Generated> {
        let mut buf = String::new();
        let mut out = self.generate_stream(req, &mut |token| {
            buf.push_str(token);
            ControlFlow::Continue(())
        })?;
        out.text = buf;
        Ok(out)
    }

    fn model_id(&self) -> &str;
    fn context_tokens(&self) -> usize;

    /// Realized execution details for diagnostics. Worker proxies learn this
    /// from the subprocess; in-process engines can report it directly.
    fn runtime(&self) -> Option<GenerationRuntime> {
        None
    }

    /// Timings from the most recent successful request.
    fn last_timings(&self) -> Option<GenerationTimings> {
        None
    }
}

/// The single application-side boundary for generation-request diagnostics.
///
/// Keeping this as a generator decorator means Candle worker requests and
/// in-process Ollama requests are logged identically, without teaching every
/// task or backend about diagnostics. Prompt content is intentionally logged:
/// it is user-authored/library content and this diagnostic is explicitly meant
/// to make the exact model input inspectable in Wilkes' local logs.
struct RequestLoggingGenerator {
    inner: Arc<dyn Generator>,
}

impl Generator for RequestLoggingGenerator {
    fn generate_stream(
        &self,
        req: GenerationRequest,
        sink: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> anyhow::Result<Generated> {
        tracing::info!(
            target: "wilkes_core::generate::request",
            model = self.inner.model_id(),
            system = ?req.system,
            prompt = %req.prompt,
            max_tokens = ?req.max_tokens,
            constraint = ?req.constraint,
            sampling = ?req.sampling,
            "generation request"
        );
        self.inner.generate_stream(req, sink)
    }

    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    fn context_tokens(&self) -> usize {
        self.inner.context_tokens()
    }

    fn runtime(&self) -> Option<GenerationRuntime> {
        self.inner.runtime()
    }

    fn last_timings(&self) -> Option<GenerationTimings> {
        self.inner.last_timings()
    }
}

/// Decorate an attached generator with exact request-content logging.
#[cfg(feature = "generate")]
pub(crate) fn with_request_logging(inner: Arc<dyn Generator>) -> Arc<dyn Generator> {
    Arc::new(RequestLoggingGenerator { inner })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerationRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub prompt: String,
    /// An explicit output ceiling for bounded tasks. `None` lets the task's
    /// semantic stop conditions or the backend context boundary end decoding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,
    pub constraint: Constraint,
    pub sampling: Sampling,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Sampling {
    /// 0.0 selects greedy argmax. Above 0 enables stochastic sampling.
    pub temperature: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<usize>,
    /// `(penalty, window)` applied over the last `window` emitted tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_penalty: Option<(f32, usize)>,
    /// Mandatory, never optional: a fixed seed keeps output reproducible and
    /// therefore cacheable even when `temperature > 0`.
    pub seed: u64,
}

impl Default for Sampling {
    /// Greedy and deterministic.
    fn default() -> Self {
        Self {
            temperature: 0.0,
            top_p: None,
            top_k: None,
            repeat_penalty: None,
            seed: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Constraint {
    /// Decode freely, halting on EOS or any stop string.
    Text { stop: Vec<String> },
    /// Logits masked to token prefixes of the allowed strings. The decode
    /// cannot emit anything outside the set.
    OneOf(Vec<String>),
    /// GBNF-subset grammar; the decode cannot leave the language.
    Grammar(String),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum StopReason {
    Eos,
    StopString,
    /// A task-owned validator found the complete prose unit it requested and
    /// deliberately stopped the underlying decoder.
    ProseBoundary,
    GrammarComplete,
    MaxTokens,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Generated {
    pub text: String,
    pub tokens: usize,
    pub stop: StopReason,
}

impl Generated {
    /// `MaxTokens` is a failure for every task here — a truncated label or
    /// sentence is not a partial success.
    pub fn is_complete(&self) -> bool {
        !matches!(self.stop, StopReason::MaxTokens | StopReason::Cancelled)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenerationRuntime {
    pub requested_device: String,
    pub device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    pub model_load_micros: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenerationTimings {
    /// Prompt framing, tokenization, tensor creation, and prefill forward pass.
    pub prompt_micros: u64,
    /// Wall time after prefill until the terminal decode condition.
    pub decode_micros: u64,
    /// Subset of `decode_micros` spent deriving and selecting constrained
    /// candidates. On asynchronous GPU backends this includes the synchronization
    /// that realizes the preceding decoder forward pass, so it is diagnostic
    /// wall time rather than isolated grammar CPU time.
    pub constraint_micros: u64,
}

/// Held as `Mutex<Option<Arc<dyn Generator>>>` in app state. Only one generator
/// is live at a time because each model occupies significant memory.
pub type ActiveGenerator = std::sync::Mutex<Option<Arc<dyn Generator>>>;

/// The generation implementation selected in settings.
///
/// Candle runs in Wilkes' generation worker. Ollama is an external HTTP
/// service and therefore never enters the worker protocol.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GenerationEngine {
    #[default]
    Candle,
    Ollama,
}

impl GenerationEngine {
    pub fn as_str(&self) -> &'static str {
        match self {
            GenerationEngine::Candle => "candle",
            GenerationEngine::Ollama => "ollama",
        }
    }
}

/// Truncate to at most `max_chars` characters on a char boundary.
/// Never byte-slice runtime strings (see `AGENTS.md`).
pub fn truncate_chars(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((byte_index, _)) => &text[..byte_index],
        None => text,
    }
}

#[cfg(feature = "test-utils")]
pub mod mock {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Scripted generator for task-layer tests. Records every request it saw so
    /// gating tests can assert that zero requests were issued.
    pub struct MockGenerator {
        pub scripted: Mutex<VecDeque<Generated>>,
        pub received: Mutex<Vec<GenerationRequest>>,
        pub model_id: String,
        pub context_tokens: usize,
    }

    impl Default for MockGenerator {
        fn default() -> Self {
            Self {
                scripted: Mutex::new(VecDeque::new()),
                received: Mutex::new(Vec::new()),
                model_id: "mock-generator".to_string(),
                context_tokens: 4096,
            }
        }
    }

    impl MockGenerator {
        /// Build a generator that replays `texts` in order, each as a complete
        /// EOS-terminated generation.
        pub fn scripted<I, S>(texts: I) -> Self
        where
            I: IntoIterator<Item = S>,
            S: Into<String>,
        {
            let scripted = texts
                .into_iter()
                .map(|text| {
                    let text = text.into();
                    Generated {
                        tokens: text.split_whitespace().count(),
                        text,
                        stop: StopReason::Eos,
                    }
                })
                .collect();
            Self {
                scripted: Mutex::new(scripted),
                ..Self::default()
            }
        }

        pub fn request_count(&self) -> usize {
            self.received.lock().unwrap().len()
        }

        pub fn requests(&self) -> Vec<GenerationRequest> {
            self.received.lock().unwrap().clone()
        }
    }

    impl Generator for MockGenerator {
        fn generate_stream(
            &self,
            req: GenerationRequest,
            sink: &mut dyn FnMut(&str) -> ControlFlow<()>,
        ) -> anyhow::Result<Generated> {
            self.received.lock().unwrap().push(req);
            let next = self.scripted.lock().unwrap().pop_front();
            let generated =
                next.ok_or_else(|| anyhow::anyhow!("MockGenerator ran out of scripted output"))?;

            // Emit word by word so streaming callers exercise the sink.
            for (index, word) in generated.text.split_inclusive(' ').enumerate() {
                if sink(word).is_break() {
                    return Ok(Generated {
                        text: String::new(),
                        tokens: index + 1,
                        stop: StopReason::Cancelled,
                    });
                }
            }
            Ok(generated)
        }

        fn model_id(&self) -> &str {
            &self.model_id
        }

        fn context_tokens(&self) -> usize {
            self.context_tokens
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoGenerator;

    impl Generator for EchoGenerator {
        fn generate_stream(
            &self,
            req: GenerationRequest,
            sink: &mut dyn FnMut(&str) -> ControlFlow<()>,
        ) -> anyhow::Result<Generated> {
            for word in req.prompt.split_inclusive(' ') {
                if sink(word).is_break() {
                    break;
                }
            }
            Ok(Generated {
                text: "ignored by the default generate()".to_string(),
                tokens: 3,
                stop: StopReason::Eos,
            })
        }

        fn model_id(&self) -> &str {
            "echo"
        }

        fn context_tokens(&self) -> usize {
            32
        }
    }

    fn request(prompt: &str) -> GenerationRequest {
        GenerationRequest {
            system: None,
            prompt: prompt.to_string(),
            max_tokens: Some(16),
            constraint: Constraint::Text { stop: Vec::new() },
            sampling: Sampling::default(),
        }
    }

    #[test]
    fn default_generate_returns_the_concatenated_stream() {
        let generated = EchoGenerator.generate(request("one two three")).unwrap();
        assert_eq!(generated.text, "one two three");
    }

    #[test]
    fn truncate_chars_respects_char_boundaries() {
        assert_eq!(truncate_chars("héllo wörld", 5), "héllo");
        assert_eq!(truncate_chars("short", 50), "short");
        assert_eq!(truncate_chars("日本語テキスト", 3), "日本語");
    }

    #[test]
    fn max_tokens_and_cancelled_are_not_complete() {
        for stop in [StopReason::MaxTokens, StopReason::Cancelled] {
            let generated = Generated {
                text: "x".into(),
                tokens: 1,
                stop,
            };
            assert!(!generated.is_complete(), "{stop:?} must not be complete");
        }
        for stop in [
            StopReason::Eos,
            StopReason::StopString,
            StopReason::ProseBoundary,
            StopReason::GrammarComplete,
        ] {
            let generated = Generated {
                text: "x".into(),
                tokens: 1,
                stop,
            };
            assert!(generated.is_complete(), "{stop:?} must be complete");
        }
    }

    #[test]
    fn default_sampling_is_greedy() {
        let sampling = Sampling::default();
        assert_eq!(sampling.temperature, 0.0);
        assert!(sampling.top_p.is_none());
        assert!(sampling.top_k.is_none());
    }
}
