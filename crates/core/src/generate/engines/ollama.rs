//! Generation through an Ollama HTTP service.
//!
//! Ollama owns model installation and process residency. Wilkes owns request
//! semantics: sampling, cancellation, stop handling, and the guarantee that a
//! constrained result is validated before a task can observe it.

use std::io::{BufRead, BufReader};
use std::ops::ControlFlow;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use reqwest::blocking::{Client, Response};
use reqwest::Url;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::generate::grammar::{json_schema_pattern, Grammar};
use crate::generate::{
    Constraint, Generated, GenerationEngine, GenerationRequest, GenerationRuntime,
    GenerationTimings, Generator, StopReason,
};
use crate::types::GeneratorDescriptor;

const OLLAMA_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const OLLAMA_CATALOG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
// A cold remote model load and long-context prefill can legitimately take
// several minutes. Keep a finite deadline so a wedged Ollama process still
// returns control to Wilkes.
const OLLAMA_GENERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);

#[derive(Debug, Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TaggedModel>,
}

#[derive(Debug, Deserialize)]
struct TaggedModel {
    #[serde(default)]
    model: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    details: ModelDetails,
}

#[derive(Debug, Default, Deserialize)]
struct ModelDetails {
    #[serde(default)]
    family: String,
    #[serde(default)]
    parameter_size: String,
    #[serde(default)]
    quantization_level: String,
}

#[derive(Debug, Deserialize)]
struct ShowResponse {
    #[serde(default)]
    model_info: Map<String, Value>,
}

#[derive(Debug, Default, Deserialize)]
struct GenerateResponse {
    #[serde(default)]
    response: String,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    done_reason: String,
    #[serde(default)]
    eval_count: usize,
    #[serde(default)]
    load_duration: u64,
    #[serde(default)]
    prompt_eval_duration: u64,
    #[serde(default)]
    eval_duration: u64,
    #[serde(default)]
    error: Option<String>,
}

pub struct OllamaGenerator {
    client: Client,
    base_url: Url,
    model_id: String,
    context_tokens: usize,
    runtime: Mutex<GenerationRuntime>,
    timings: Mutex<Option<GenerationTimings>>,
}

impl OllamaGenerator {
    pub fn connect(
        base_url: &str,
        model_id: &str,
        requested_context_tokens: Option<usize>,
    ) -> anyhow::Result<Arc<Self>> {
        let base_url = normalize_base_url(base_url)?;
        let client = ollama_client(OLLAMA_GENERATION_TIMEOUT)?;
        let show: ShowResponse = checked_response(
            client
                .post(endpoint(&base_url, "api/show")?)
                .json(&json!({ "model": model_id, "verbose": false }))
                .send()
                .with_context(|| format!("could not connect to Ollama at {base_url}"))?,
            "inspect Ollama model",
        )?
        .json()
        .context("Ollama returned invalid model details")?;
        let model_context_tokens = context_length(&show.model_info).ok_or_else(|| {
            anyhow::anyhow!("Ollama did not report a context length for model '{model_id}'")
        })?;
        let context_tokens = requested_context_tokens.unwrap_or(model_context_tokens);
        anyhow::ensure!(
            context_tokens > 0,
            "Ollama context window must be greater than zero"
        );
        anyhow::ensure!(
            context_tokens <= model_context_tokens,
            "Requested Ollama context window ({context_tokens}) exceeds the model maximum ({model_context_tokens})"
        );

        Ok(Arc::new(Self {
            client,
            base_url,
            model_id: model_id.to_string(),
            context_tokens,
            runtime: Mutex::new(GenerationRuntime {
                requested_device: "ollama".to_string(),
                device: "external".to_string(),
                fallback_reason: None,
                model_load_micros: 0,
            }),
            timings: Mutex::new(None),
        }))
    }

    fn request_body(&self, request: &GenerationRequest, stream: bool) -> anyhow::Result<Value> {
        let mut options = Map::new();
        options.insert(
            "num_predict".to_string(),
            request
                .max_tokens
                .map_or_else(|| json!(-1), |limit| json!(limit)),
        );
        // Ollama otherwise applies its much smaller service default and can
        // silently truncate a prompt even though the model supports more.
        options.insert("num_ctx".to_string(), json!(self.context_tokens));
        options.insert(
            "temperature".to_string(),
            json!(request.sampling.temperature),
        );
        options.insert("seed".to_string(), json!(request.sampling.seed));
        if let Some(top_p) = request.sampling.top_p {
            options.insert("top_p".to_string(), json!(top_p));
        }
        if let Some(top_k) = request.sampling.top_k {
            options.insert("top_k".to_string(), json!(top_k));
        }
        if let Some((penalty, window)) = request.sampling.repeat_penalty {
            options.insert("repeat_penalty".to_string(), json!(penalty));
            options.insert("repeat_last_n".to_string(), json!(window));
        }
        if let Constraint::Text { stop } = &request.constraint {
            if !stop.is_empty() {
                options.insert("stop".to_string(), json!(stop));
            }
        }

        let mut body = Map::from_iter([
            ("model".to_string(), json!(self.model_id)),
            ("prompt".to_string(), json!(request.prompt)),
            ("stream".to_string(), json!(stream)),
            ("think".to_string(), json!(false)),
            ("keep_alive".to_string(), json!("5m")),
            ("options".to_string(), Value::Object(options)),
        ]);
        if let Some(system) = &request.system {
            body.insert("system".to_string(), json!(system));
        }
        match &request.constraint {
            Constraint::Text { .. } => {}
            Constraint::OneOf(options) => {
                anyhow::ensure!(!options.is_empty(), "OneOf constraint has no options");
                body.insert(
                    "format".to_string(),
                    json!({ "type": "string", "enum": options }),
                );
            }
            Constraint::Grammar(source) => {
                body.insert(
                    "format".to_string(),
                    json!({
                        "type": "string",
                        "pattern": json_schema_pattern(source)?,
                    }),
                );
            }
        }
        Ok(Value::Object(body))
    }

    fn send(&self, body: &Value) -> anyhow::Result<Response> {
        let response = self
            .client
            .post(endpoint(&self.base_url, "api/generate")?)
            .json(body)
            .send()
            .with_context(|| format!("could not connect to Ollama at {}", self.base_url))?;
        checked_response(response, "generate with Ollama")
    }

    fn record_metrics(&self, response: &GenerateResponse) {
        *self
            .runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = GenerationRuntime {
            requested_device: "ollama".to_string(),
            device: "external".to_string(),
            fallback_reason: None,
            model_load_micros: nanos_to_micros(response.load_duration),
        };
        *self
            .timings
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(GenerationTimings {
            prompt_micros: nanos_to_micros(response.prompt_eval_duration),
            decode_micros: nanos_to_micros(response.eval_duration),
            constraint_micros: 0,
        });
    }

    fn generate_text(
        &self,
        request: GenerationRequest,
        sink: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> anyhow::Result<Generated> {
        let body = self.request_body(&request, true)?;
        let response = self.send(&body)?;
        let mut reader = BufReader::new(response);
        let mut line = String::new();
        let mut text = String::new();

        loop {
            line.clear();
            if reader
                .read_line(&mut line)
                .context("could not read Ollama generation stream")?
                == 0
            {
                anyhow::bail!("Ollama ended the response without a completion event");
            }
            let chunk: GenerateResponse = serde_json::from_str(line.trim_end())
                .context("Ollama returned invalid streaming JSON")?;
            if let Some(error) = chunk.error {
                anyhow::bail!("Ollama generation failed: {error}");
            }
            if !chunk.response.is_empty() {
                text.push_str(&chunk.response);
                if sink(&chunk.response).is_break() {
                    return Ok(Generated {
                        text,
                        tokens: 0,
                        stop: StopReason::Cancelled,
                    });
                }
            }
            if chunk.done {
                self.record_metrics(&chunk);
                return Ok(Generated {
                    text,
                    tokens: chunk.eval_count,
                    stop: map_stop_reason(&chunk.done_reason, &request.constraint),
                });
            }
        }
    }

    fn generate_constrained(
        &self,
        request: GenerationRequest,
        sink: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> anyhow::Result<Generated> {
        let body = self.request_body(&request, false)?;
        let response: GenerateResponse = self
            .send(&body)?
            .json()
            .context("Ollama returned invalid generation JSON")?;
        if let Some(error) = response.error.as_deref() {
            anyhow::bail!("Ollama generation failed: {error}");
        }
        anyhow::ensure!(response.done, "Ollama response was not complete");
        let decoded: String = serde_json::from_str(&response.response)
            .context("Ollama constrained response was not a JSON string")?;
        match &request.constraint {
            Constraint::OneOf(options) => anyhow::ensure!(
                options.contains(&decoded),
                "Ollama response violated the OneOf constraint"
            ),
            Constraint::Grammar(source) => anyhow::ensure!(
                Grammar::parse(source)?.accepts(&decoded),
                "Ollama response violated the generation grammar"
            ),
            Constraint::Text { .. } => unreachable!("text requests use the streaming path"),
        }
        self.record_metrics(&response);
        if sink(&decoded).is_break() {
            return Ok(Generated {
                text: decoded,
                tokens: response.eval_count,
                stop: StopReason::Cancelled,
            });
        }
        Ok(Generated {
            text: decoded,
            tokens: response.eval_count,
            stop: map_stop_reason(&response.done_reason, &request.constraint),
        })
    }
}

impl Generator for OllamaGenerator {
    fn generate_stream(
        &self,
        request: GenerationRequest,
        sink: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> anyhow::Result<Generated> {
        if matches!(request.constraint, Constraint::Text { .. }) {
            self.generate_text(request, sink)
        } else {
            self.generate_constrained(request, sink)
        }
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn context_tokens(&self) -> usize {
        self.context_tokens
    }

    fn runtime(&self) -> Option<GenerationRuntime> {
        Some(
            self.runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone(),
        )
    }

    fn last_timings(&self) -> Option<GenerationTimings> {
        self.timings
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

pub fn list_models(base_url: &str) -> anyhow::Result<Vec<GeneratorDescriptor>> {
    let base_url = normalize_base_url(base_url)?;
    let client = ollama_client(OLLAMA_CATALOG_TIMEOUT)?;
    let response: TagsResponse = checked_response(
        client
            .get(endpoint(&base_url, "api/tags")?)
            .send()
            .with_context(|| format!("could not connect to Ollama at {base_url}"))?,
        "list Ollama models",
    )?
    .json()
    .context("Ollama returned an invalid model list")?;

    let mut models = response
        .models
        .into_iter()
        .map(|model| {
            // Current Ollama returns both fields with the same value. Older
            // releases returned only `name`; keeping them separate avoids
            // Serde treating the pair as a duplicate assignment.
            let model_id = if model.model.trim().is_empty() {
                model.name
            } else {
                model.model
            };
            anyhow::ensure!(
                !model_id.trim().is_empty(),
                "Ollama model list contains an entry without a model name"
            );
            let description = [
                model.details.parameter_size,
                model.details.quantization_level,
                model.details.family,
            ]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
            Ok(GeneratorDescriptor {
                engine: GenerationEngine::Ollama,
                display_name: model_id.clone(),
                model_id,
                description,
                context_tokens: 0,
                is_cached: true,
                is_default: false,
                is_recommended: false,
                size_bytes: Some(model.size),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    models.sort_by(|left, right| left.model_id.cmp(&right.model_id));
    Ok(models)
}

/// The blocking client every Ollama caller uses, extraction included.
pub(crate) fn ollama_client(timeout: std::time::Duration) -> anyhow::Result<Client> {
    Client::builder()
        .connect_timeout(OLLAMA_CONNECT_TIMEOUT)
        .timeout(timeout)
        .build()
        .context("could not create Ollama HTTP client")
}

/// What a usable Ollama server URL is, decided once for every caller.
pub(crate) fn normalize_base_url(input: &str) -> anyhow::Result<Url> {
    let mut url = Url::parse(input.trim()).context("Ollama URL is invalid")?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https"),
        "Ollama URL must use http or https"
    );
    anyhow::ensure!(url.host_str().is_some(), "Ollama URL has no host");
    anyhow::ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Ollama URL must not contain credentials"
    );
    anyhow::ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "Ollama URL must not contain a query or fragment"
    );
    anyhow::ensure!(
        matches!(url.path(), "" | "/"),
        "Ollama URL must not contain a path"
    );
    url.set_path("/");
    Ok(url)
}

pub(crate) fn endpoint(base_url: &Url, path: &str) -> anyhow::Result<Url> {
    base_url
        .join(path)
        .with_context(|| format!("could not build Ollama endpoint '{path}'"))
}

pub(crate) fn checked_response(response: Response, operation: &str) -> anyhow::Result<Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.text().unwrap_or_default();
    let detail = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.trim().to_string());
    if detail.is_empty() {
        anyhow::bail!("could not {operation}: Ollama returned {status}")
    }
    anyhow::bail!("could not {operation}: Ollama returned {status}: {detail}")
}

fn context_length(info: &Map<String, Value>) -> Option<usize> {
    info.iter()
        .find(|(key, value)| key.ends_with(".context_length") && value.as_u64().is_some())
        .and_then(|(_, value)| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
}

fn nanos_to_micros(value: u64) -> u64 {
    value / 1_000
}

fn map_stop_reason(reason: &str, constraint: &Constraint) -> StopReason {
    if matches!(reason, "length" | "max_tokens") {
        return StopReason::MaxTokens;
    }
    match constraint {
        Constraint::Text { stop } if !stop.is_empty() && reason == "stop" => StopReason::StopString,
        Constraint::Text { .. } => StopReason::Eos,
        Constraint::OneOf(_) | Constraint::Grammar(_) => StopReason::GrammarComplete,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::tasks::cluster_label::LABEL_GRAMMAR;
    use crate::generate::Sampling;
    use mockito::Matcher;

    fn show_body() -> Value {
        json!({
            "model_info": {
                "test.context_length": 8192
            }
        })
    }

    fn request(constraint: Constraint) -> GenerationRequest {
        GenerationRequest {
            system: Some("system".to_string()),
            prompt: "prompt".to_string(),
            max_tokens: Some(32),
            constraint,
            sampling: Sampling {
                temperature: 0.2,
                top_p: Some(0.9),
                top_k: Some(20),
                repeat_penalty: Some((1.1, 16)),
                seed: 42,
            },
        }
    }

    #[test]
    fn generation_timeout_allows_cold_load_and_prefill_beyond_http_default() {
        assert_eq!(OLLAMA_CONNECT_TIMEOUT, std::time::Duration::from_secs(5));
        assert_eq!(
            OLLAMA_GENERATION_TIMEOUT,
            std::time::Duration::from_secs(900)
        );
        assert!(OLLAMA_GENERATION_TIMEOUT > OLLAMA_CATALOG_TIMEOUT);
    }

    fn connected(server: &mut mockito::ServerGuard) -> Arc<OllamaGenerator> {
        let show = server
            .mock("POST", "/api/show")
            .match_body(Matcher::PartialJson(json!({ "model": "test-model" })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(show_body().to_string())
            .create();
        let generator = OllamaGenerator::connect(&server.url(), "test-model", None).unwrap();
        show.assert();
        generator
    }

    #[test]
    fn lists_models_owned_by_ollama() {
        let mut server = mockito::Server::new();
        let tags = server
            .mock("GET", "/api/tags")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "models": [{
                        "name": "gemma3:4b",
                        "model": "gemma3:4b",
                        "size": 1234,
                        "details": {
                            "family": "gemma3",
                            "parameter_size": "4.3B",
                            "quantization_level": "Q4_K_M"
                        }
                    }, {
                        "name": "legacy:latest",
                        "size": 4321
                    }]
                })
                .to_string(),
            )
            .create();

        let models = list_models(&server.url()).unwrap();
        tags.assert();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].engine, GenerationEngine::Ollama);
        assert_eq!(models[0].model_id, "gemma3:4b");
        assert_eq!(models[0].size_bytes, Some(1234));
        assert!(models[0].description.contains("Q4_K_M"));
        assert_eq!(models[1].model_id, "legacy:latest");
        assert_eq!(models[1].size_bytes, Some(4321));
    }

    #[test]
    fn streams_text_and_maps_sampling_options() {
        let mut server = mockito::Server::new();
        let generator = connected(&mut server);
        let generation_request = request(Constraint::Text {
            stop: vec!["\n".to_string()],
        });
        let body = generator.request_body(&generation_request, true).unwrap();
        assert_eq!(body["options"]["num_predict"], 32);
        assert_eq!(body["options"]["num_ctx"], 8192);
        assert_eq!(body["options"]["seed"], 42);
        assert_eq!(body["options"]["top_k"], 20);
        assert_eq!(body["options"]["repeat_last_n"], 16);
        assert_eq!(body["options"]["stop"], json!(["\n"]));
        let one_of = generator
            .request_body(
                &request(Constraint::OneOf(vec!["yes".to_string(), "no".to_string()])),
                false,
            )
            .unwrap();
        assert_eq!(one_of["format"]["type"], "string");
        assert_eq!(one_of["format"]["enum"], json!(["yes", "no"]));
        let generate = server
            .mock("POST", "/api/generate")
            .match_body(Matcher::PartialJson(json!({
                "model": "test-model",
                "stream": true,
                "think": false
            })))
            .with_status(200)
            .with_body(
                "{\"response\":\"hello \"}\n{\"response\":\"world\"}\n{\"done\":true,\"done_reason\":\"stop\",\"eval_count\":2}\n",
            )
            .create();

        let mut streamed = String::new();
        let output = generator
            .generate_stream(generation_request, &mut |chunk| {
                streamed.push_str(chunk);
                ControlFlow::Continue(())
            })
            .unwrap();
        generate.assert();
        assert_eq!(streamed, "hello world");
        assert_eq!(output.text, streamed);
        assert_eq!(output.tokens, 2);
        assert_eq!(output.stop, StopReason::StopString);
        assert_eq!(generator.context_tokens(), 8192);
    }

    #[test]
    fn unlimited_requests_use_ollamas_infinite_generation_setting() {
        let mut server = mockito::Server::new();
        let generator = connected(&mut server);
        let mut generation_request = request(Constraint::Text { stop: Vec::new() });
        generation_request.max_tokens = None;

        let body = generator.request_body(&generation_request, true).unwrap();

        assert_eq!(body["options"]["num_predict"], -1);
    }

    #[test]
    fn explicit_context_window_is_bounded_by_the_model_maximum() {
        let mut server = mockito::Server::new();
        let first_show = server
            .mock("POST", "/api/show")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(show_body().to_string())
            .create();
        let generator = OllamaGenerator::connect(&server.url(), "test-model", Some(4096)).unwrap();
        first_show.assert();
        assert_eq!(generator.context_tokens(), 4096);
        assert_eq!(
            generator
                .request_body(&request(Constraint::Text { stop: Vec::new() }), true)
                .unwrap()["options"]["num_ctx"],
            4096
        );

        let second_show = server
            .mock("POST", "/api/show")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(show_body().to_string())
            .create();
        let error = OllamaGenerator::connect(&server.url(), "test-model", Some(16_384))
            .err()
            .expect("oversized context should fail");
        second_show.assert();
        assert!(error.to_string().contains("exceeds the model maximum"));
    }

    #[test]
    fn validates_grammar_output_before_publishing_it() {
        let mut server = mockito::Server::new();
        let generator = connected(&mut server);
        let encoded = serde_json::to_string("Topic: Cache invalidation").unwrap();
        let generate = server
            .mock("POST", "/api/generate")
            .match_body(Matcher::PartialJson(json!({
                "stream": false,
                "format": { "type": "string" }
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "response": encoded,
                    "done": true,
                    "done_reason": "stop",
                    "eval_count": 5
                })
                .to_string(),
            )
            .create();

        let mut published = String::new();
        let output = generator
            .generate_stream(
                request(Constraint::Grammar(LABEL_GRAMMAR.to_string())),
                &mut |chunk| {
                    published.push_str(chunk);
                    ControlFlow::Continue(())
                },
            )
            .unwrap();
        generate.assert();
        assert_eq!(published, "Topic: Cache invalidation");
        assert_eq!(output.stop, StopReason::GrammarComplete);
    }

    #[test]
    fn rejects_invalid_constrained_output_without_publishing_it() {
        let mut server = mockito::Server::new();
        let generator = connected(&mut server);
        let generate = server
            .mock("POST", "/api/generate")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "response": serde_json::to_string("not a label").unwrap(),
                    "done": true,
                    "done_reason": "stop"
                })
                .to_string(),
            )
            .create();

        let mut published = String::new();
        let result = generator.generate_stream(
            request(Constraint::Grammar(LABEL_GRAMMAR.to_string())),
            &mut |chunk| {
                published.push_str(chunk);
                ControlFlow::Continue(())
            },
        );
        generate.assert();
        assert!(result.is_err());
        assert!(published.is_empty());
    }

    #[test]
    fn cancellation_drops_the_stream_and_returns_no_successful_partial_result() {
        let mut server = mockito::Server::new();
        let generator = connected(&mut server);
        let generate = server
            .mock("POST", "/api/generate")
            .with_status(200)
            .with_body(
                "{\"response\":\"first\"}\n{\"response\":\"second\"}\n{\"done\":true,\"done_reason\":\"stop\",\"eval_count\":2}\n",
            )
            .create();

        let mut published = String::new();
        let output = generator
            .generate_stream(
                request(Constraint::Text { stop: Vec::new() }),
                &mut |chunk| {
                    published.push_str(chunk);
                    ControlFlow::Break(())
                },
            )
            .unwrap();
        generate.assert();
        assert_eq!(published, "first");
        assert_eq!(output.text, "first");
        assert_eq!(output.stop, StopReason::Cancelled);
        assert!(!output.is_complete());
    }

    #[test]
    fn endpoint_validation_rejects_credentials_and_paths() {
        assert!(normalize_base_url("ftp://localhost:11434").is_err());
        assert!(normalize_base_url("http://user:secret@localhost:11434").is_err());
        assert!(normalize_base_url("http://localhost:11434/custom").is_err());
        assert_eq!(
            normalize_base_url("http://localhost:11434")
                .unwrap()
                .as_str(),
            "http://localhost:11434/"
        );
    }
}
