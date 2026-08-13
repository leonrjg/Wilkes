//! Produce a concise summary from one bounded, representative document sample.

use std::ops::ControlFlow;

use serde::{Deserialize, Serialize};

use crate::generate::{Constraint, Generated, GenerationRequest, Generator, Sampling};

/// Enough source for the small local model to see the document's opening,
/// development, and conclusion without overflowing its context window.
pub const MAX_DOCUMENT_CHARS: usize = 9_000;
const MAX_TOKENS: usize = 180;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentSummaryInput {
    pub title: String,
    pub text: String,
}

/// Character-aware representative sampling. Long documents contribute equal
/// slices from their beginning, middle, and end rather than only their abstract
/// or introduction.
pub fn sample_document(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    let char_count = trimmed.chars().count();
    if char_count <= max_chars {
        return trimmed.to_string();
    }

    let segment_chars = max_chars / 3;
    let middle_start = char_count.saturating_sub(segment_chars) / 2;
    let end_start = char_count.saturating_sub(segment_chars);
    let take = |start| {
        trimmed
            .chars()
            .skip(start)
            .take(segment_chars)
            .collect::<String>()
    };

    format!(
        "[Beginning]\n{}\n\n[Middle]\n{}\n\n[End]\n{}",
        take(0),
        take(middle_start),
        take(end_start),
    )
}

pub fn build_request(input: &DocumentSummaryInput) -> GenerationRequest {
    let sample = sample_document(&input.text, MAX_DOCUMENT_CHARS);
    let prompt = format!(
        "Summarize the document below in one concise paragraph of three to five \
         sentences. State its purpose, central claims or findings, and conclusion. \
         Use only information in the document. Omit preambles such as \"This \
         document discusses\" and do not mention the supplied sample labels.\n\n\
         Title: {}\n\n{}\n\nSummary:",
        input.title.trim(),
        sample,
    );

    GenerationRequest {
        system: None,
        prompt,
        max_tokens: Some(MAX_TOKENS),
        constraint: Constraint::Text {
            stop: vec!["\n\n".to_string()],
        },
        sampling: Sampling::default(),
    }
}

pub fn summarize_document(
    generator: &dyn Generator,
    input: &DocumentSummaryInput,
    sink: &mut dyn FnMut(&str) -> ControlFlow<()>,
) -> anyhow::Result<Generated> {
    anyhow::ensure!(
        !input.text.trim().is_empty(),
        "document has no extractable text"
    );

    let generated = generator.generate_stream(build_request(input), sink)?;
    anyhow::ensure!(
        generated.is_complete(),
        "document summary did not finish cleanly ({:?})",
        generated.stop
    );
    anyhow::ensure!(
        !generated.text.trim().is_empty(),
        "generator returned an empty document summary"
    );
    Ok(generated)
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::generate::mock::MockGenerator;
    use crate::generate::StopReason;

    fn input(text: &str) -> DocumentSummaryInput {
        DocumentSummaryInput {
            title: "Example".to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn short_documents_are_kept_whole() {
        assert_eq!(sample_document("  alpha βeta  ", 100), "alpha βeta");
    }

    #[test]
    fn long_documents_sample_beginning_middle_and_end_by_character() {
        let text = format!("{}{}{}", "á".repeat(20), "中".repeat(20), "🙂".repeat(20));
        let sampled = sample_document(&text, 18);
        assert!(sampled.contains(&"á".repeat(6)));
        assert!(sampled.contains(&"中".repeat(6)));
        assert!(sampled.contains(&"🙂".repeat(6)));
    }

    #[test]
    fn request_is_greedy_and_bounded() {
        let request = build_request(&input("source text"));
        assert_eq!(request.max_tokens, Some(MAX_TOKENS));
        assert_eq!(request.sampling, Sampling::default());
        assert_eq!(
            request.constraint,
            Constraint::Text {
                stop: vec!["\n\n".to_string()]
            }
        );
    }

    #[test]
    fn streamed_tokens_concatenate_to_the_verified_summary() {
        let generator = MockGenerator::scripted([
            "The study measures cache contention. ",
            "It finds batching reduces stalls.",
        ]);
        let mut streamed = String::new();
        let generated = summarize_document(&generator, &input("source"), &mut |token| {
            streamed.push_str(token);
            ControlFlow::Continue(())
        })
        .unwrap();
        assert_eq!(streamed, generated.text);
        assert_eq!(generated.stop, StopReason::Eos);
    }

    #[test]
    fn empty_or_cancelled_summaries_fail() {
        let unused = MockGenerator::scripted(["unused"]);
        assert!(
            summarize_document(&unused, &input("  "), &mut |_| ControlFlow::Continue(())).is_err()
        );
        assert_eq!(unused.request_count(), 0);

        let generator = MockGenerator::scripted(["partial summary"]);
        assert!(summarize_document(
            &generator,
            &input("source"),
            &mut |_| ControlFlow::Break(())
        )
        .is_err());
    }
}
