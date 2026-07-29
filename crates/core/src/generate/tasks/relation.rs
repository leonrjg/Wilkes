//! Explain, in one sentence, why two documents are related.
//!
//! This is the streaming task: the sentence is displayed as it arrives, which
//! is why it takes a sink while `cluster_label` does not.

use std::ops::ControlFlow;

use serde::{Deserialize, Serialize};

use crate::generate::{
    truncate_chars, Constraint, Generated, GenerationRequest, Generator, Sampling,
};

/// One side of the pair. `excerpt` always comes from the extraction cache —
/// hovering a row is not a licence to parse a PDF.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentSummary {
    /// Resolved metadata title, else the file stem.
    pub title: String,
    /// Leading text of the document, already extracted.
    pub excerpt: String,
}

pub const MAX_EXCERPT_CHARS: usize = 600;
const MAX_TOKENS: usize = 60;

pub fn build_request(anchor: &DocumentSummary, related: &DocumentSummary) -> GenerationRequest {
    let prompt = format!(
        "Two documents from the same library were matched as related. In one \
         sentence, say what connects them. Do not restate the titles.\n\n\
         Document A: {}\n{}\n\nDocument B: {}\n{}\n\nConnection:",
        anchor.title.trim(),
        truncate_chars(anchor.excerpt.trim(), MAX_EXCERPT_CHARS),
        related.title.trim(),
        truncate_chars(related.excerpt.trim(), MAX_EXCERPT_CHARS),
    );

    GenerationRequest {
        system: None,
        prompt,
        max_tokens: MAX_TOKENS,
        // Free text rather than a grammar, and the reason cuts against the
        // cluster label: a label has a shape worth enforcing, an explanatory
        // sentence does not. The newline stop is the whole contract — the model
        // cannot emit a bulleted list because it cannot emit a newline.
        constraint: Constraint::Text {
            stop: vec!["\n".to_string()],
        },
        // Greedy, so re-expanding a row shows the same sentence.
        sampling: Sampling::default(),
    }
}

/// Stream one explanatory sentence. Returns `Err` on a partial stream rather
/// than the text accumulated so far: a sentence cut mid-clause is worse than no
/// sentence, and the reader cannot tell the two apart.
pub fn explain_relation(
    generator: &dyn Generator,
    anchor: &DocumentSummary,
    related: &DocumentSummary,
    sink: &mut dyn FnMut(&str) -> ControlFlow<()>,
) -> anyhow::Result<Generated> {
    anyhow::ensure!(
        !anchor.excerpt.trim().is_empty() && !related.excerpt.trim().is_empty(),
        "no cached text for one side of the pair"
    );

    let generated = generator.generate_stream(build_request(anchor, related), sink)?;
    anyhow::ensure!(
        generated.is_complete(),
        "relation explanation did not finish cleanly ({:?})",
        generated.stop
    );
    anyhow::ensure!(
        !generated.text.trim().is_empty(),
        "generator returned an empty explanation"
    );
    Ok(generated)
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::generate::mock::MockGenerator;
    use crate::generate::StopReason;

    fn summary(title: &str, excerpt: &str) -> DocumentSummary {
        DocumentSummary {
            title: title.to_string(),
            excerpt: excerpt.to_string(),
        }
    }

    fn collect(
        generator: &dyn Generator,
        a: &DocumentSummary,
        b: &DocumentSummary,
    ) -> (String, anyhow::Result<Generated>) {
        let mut streamed = String::new();
        let result = explain_relation(generator, a, b, &mut |token| {
            streamed.push_str(token);
            ControlFlow::Continue(())
        });
        (streamed, result)
    }

    #[test]
    fn streamed_tokens_concatenate_to_the_final_text() {
        let generator = MockGenerator::scripted(["Both examine cache coherence under write skew."]);
        let (streamed, result) =
            collect(&generator, &summary("A", "text a"), &summary("B", "text b"));
        let generated = result.unwrap();
        assert_eq!(streamed, generated.text);
        assert_eq!(generated.stop, StopReason::Eos);
    }

    #[test]
    fn missing_cached_text_issues_no_request() {
        let generator = MockGenerator::scripted(["unused"]);
        let (_, result) = collect(&generator, &summary("A", "   "), &summary("B", "text"));
        assert!(result.is_err());
        assert_eq!(generator.request_count(), 0);
    }

    #[test]
    fn excerpts_are_truncated_character_aware() {
        let excerpt = "ü".repeat(MAX_EXCERPT_CHARS + 50);
        let request = build_request(&summary("A", &excerpt), &summary("B", &excerpt));
        assert!(request.prompt.contains(&"ü".repeat(MAX_EXCERPT_CHARS)));
        assert!(!request.prompt.contains(&"ü".repeat(MAX_EXCERPT_CHARS + 1)));
    }

    #[test]
    fn stops_at_a_newline_and_stays_greedy() {
        let request = build_request(&summary("A", "a"), &summary("B", "b"));
        assert_eq!(
            request.constraint,
            Constraint::Text {
                stop: vec!["\n".to_string()]
            }
        );
        assert_eq!(request.sampling, Sampling::default());
        assert_eq!(request.max_tokens, MAX_TOKENS);
    }

    #[test]
    fn a_cancelled_stream_is_an_error_not_partial_text() {
        let generator = MockGenerator::scripted(["one two three four"]);
        let mut streamed = String::new();
        let result = explain_relation(
            &generator,
            &summary("A", "a"),
            &summary("B", "b"),
            &mut |token| {
                streamed.push_str(token);
                ControlFlow::Break(())
            },
        );
        assert!(!streamed.is_empty(), "tokens did reach the sink");
        assert!(result.is_err(), "but the task must not return partial text");
    }
}
