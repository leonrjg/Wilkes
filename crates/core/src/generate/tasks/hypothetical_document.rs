//! HyDE: generate a hypothetical answer passage for a search query.
//!
//! The generated text is never shown to the user. It exists only to be
//! embedded, so the vector the index is queried with sits in *document* space
//! rather than terse-question space (Gao et al., 2022, "Precise Zero-Shot Dense
//! Retrieval without Relevance Labels"). Unlike the other tasks in this module,
//! there is no grammar and no rejected output: a passage that hits the token
//! ceiling is still a usable embedding target, so only an empty or cancelled
//! generation is treated as a failure.

use crate::generate::{
    truncate_chars, Constraint, GenerationRequest, Generator, Sampling, StopReason,
};

/// The query beyond this length is truncated; a HyDE prompt is anchored by the
/// first clause, not by a pasted document.
const MAX_QUERY_CHARS: usize = 500;
/// A hypothetical passage is a short paragraph. Room for 2-4 sentences.
const MAX_TOKENS: usize = 160;
/// Temperature used when more than one passage is requested, so the samples
/// actually diverge. A single passage is generated greedily (see below).
const MULTI_TEMPERATURE: f32 = 0.8;

const PROMPT_HEADER: &str = "Write a short, factual passage of two to four sentences that \
directly answers the question below, as if it were an excerpt from a relevant document. \
Do not hedge, do not say it is hypothetical, and do not repeat the question.\n\nQuestion: ";

/// Build the request for one hypothetical passage.
///
/// `seed`/`temperature` are threaded so the multi-passage caller can vary them
/// per index; a fixed seed keeps any single generation reproducible and
/// therefore cacheable.
pub fn build_request(query: &str, seed: u64, temperature: f32) -> GenerationRequest {
    let mut prompt = String::from(PROMPT_HEADER);
    prompt.push_str(truncate_chars(query.trim(), MAX_QUERY_CHARS));
    prompt.push_str("\n\nPassage:");

    GenerationRequest {
        system: None,
        prompt,
        max_tokens: MAX_TOKENS,
        // Free text: the passage is an embedding target, not a parsed value, so
        // there is nothing to constrain. EOS ends it; the token ceiling caps it.
        constraint: Constraint::Text { stop: Vec::new() },
        sampling: Sampling {
            temperature,
            seed,
            ..Sampling::default()
        },
    }
}

/// Generate `count` hypothetical passages for `query`, returning the non-empty
/// ones in generation order.
///
/// A single passage is generated greedily with a fixed seed so an identical
/// query yields an identical search. Several passages raise the temperature and
/// vary the seed per index, because averaging identical greedy samples would add
/// nothing. Returns `Err` only when the query is empty or the generator yields
/// nothing usable — a truncated passage is acceptable.
pub fn hypothetical_documents(
    generator: &dyn Generator,
    query: &str,
    count: usize,
) -> anyhow::Result<Vec<String>> {
    anyhow::ensure!(
        !query.trim().is_empty(),
        "cannot generate a hypothetical document for an empty query"
    );

    let count = count.max(1);
    let mut passages = Vec::with_capacity(count);
    for index in 0..count {
        let (temperature, seed) = if count == 1 {
            (0.0, 0)
        } else {
            (MULTI_TEMPERATURE, index as u64)
        };
        let generated = generator.generate(build_request(query, seed, temperature))?;
        anyhow::ensure!(
            generated.stop != StopReason::Cancelled,
            "hypothetical document generation was cancelled"
        );
        let text = generated.text.trim();
        if !text.is_empty() {
            passages.push(text.to_string());
        }
    }

    anyhow::ensure!(
        !passages.is_empty(),
        "generator produced no usable hypothetical document"
    );
    Ok(passages)
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::generate::mock::MockGenerator;

    #[test]
    fn build_request_embeds_query_and_omits_stop_strings() {
        let req = build_request("what causes cache invalidation", 0, 0.0);
        assert!(req.prompt.contains("what causes cache invalidation"));
        assert!(matches!(req.constraint, Constraint::Text { ref stop } if stop.is_empty()));
        assert_eq!(req.max_tokens, MAX_TOKENS);
    }

    #[test]
    fn single_passage_is_greedy_and_deterministic() {
        let generator =
            MockGenerator::scripted(["Cache entries expire when their source rows change."]);
        let passages = hypothetical_documents(&generator, "cache invalidation", 1).unwrap();
        assert_eq!(passages.len(), 1);
        assert_eq!(
            passages[0],
            "Cache entries expire when their source rows change."
        );
        // Greedy, fixed seed for a single passage.
        let req = &generator.requests()[0];
        assert_eq!(req.sampling.temperature, 0.0);
        assert_eq!(req.sampling.seed, 0);
    }

    #[test]
    fn multiple_passages_vary_seed_and_temperature() {
        let generator = MockGenerator::scripted(["passage one", "passage two"]);
        let passages = hypothetical_documents(&generator, "topic", 2).unwrap();
        assert_eq!(passages, vec!["passage one", "passage two"]);
        let reqs = generator.requests();
        assert_eq!(reqs.len(), 2);
        assert!(reqs[0].sampling.temperature > 0.0);
        assert_ne!(reqs[0].sampling.seed, reqs[1].sampling.seed);
    }

    #[test]
    fn empty_query_is_rejected_without_calling_the_generator() {
        let generator = MockGenerator::default();
        let err = hypothetical_documents(&generator, "   ", 1).unwrap_err();
        assert!(err.to_string().contains("empty query"));
        assert_eq!(generator.request_count(), 0);
    }

    #[test]
    fn blank_generation_is_an_error() {
        let generator = MockGenerator::scripted(["   "]);
        let err = hypothetical_documents(&generator, "topic", 1).unwrap_err();
        assert!(err.to_string().contains("no usable hypothetical document"));
    }

    #[test]
    fn cancelled_generation_is_rejected_even_with_partial_text() {
        let generator = MockGenerator::default();
        generator
            .scripted
            .lock()
            .unwrap()
            .push_back(crate::generate::Generated {
                text: "partial hypothetical passage".to_string(),
                tokens: 3,
                stop: StopReason::Cancelled,
            });

        let err = hypothetical_documents(&generator, "topic", 1).unwrap_err();

        assert!(err.to_string().contains("cancelled"));
    }
}
