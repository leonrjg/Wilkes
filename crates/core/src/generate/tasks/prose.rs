//! Shared termination for sentence-scale free-text tasks.
//!
//! Generation engines only understand protocol stops, fixed strings, token
//! exhaustion, and consumer cancellation. Prose tasks additionally understand
//! sentence boundaries. This adapter lets a task stop at such a boundary
//! without misreporting the deliberate stop as user cancellation.

use std::ops::ControlFlow;

use crate::generate::{Generated, GenerationRequest, Generator, StopReason};

/// Generate at most one complete sentence, while continuing to accept an
/// earlier engine-owned EOS or fixed stop. Text after the first validated
/// boundary is neither returned nor forwarded to the caller.
pub fn generate_to_sentence_boundary(
    generator: &dyn Generator,
    request: GenerationRequest,
    sink: &mut dyn FnMut(&str) -> ControlFlow<()>,
) -> anyhow::Result<Generated> {
    let mut observed = String::new();
    let mut boundary = None;
    let mut consumer_cancelled = false;

    let mut generated = generator.generate_stream(request, &mut |chunk| {
        let chunk_start = observed.len();
        observed.push_str(chunk);

        if let Some(cutoff) = first_sentence_boundary(&observed) {
            let visible_end = cutoff.saturating_sub(chunk_start).min(chunk.len());
            if visible_end > 0 && sink(&chunk[..visible_end]).is_break() {
                consumer_cancelled = true;
                return ControlFlow::Break(());
            }
            boundary = Some(cutoff);
            return ControlFlow::Break(());
        }

        if sink(chunk).is_break() {
            consumer_cancelled = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    })?;

    if consumer_cancelled {
        generated.stop = StopReason::Cancelled;
    } else if let Some(cutoff) = boundary {
        generated.text = observed[..cutoff].trim().to_string();
        generated.stop = StopReason::ProseBoundary;
    }
    Ok(generated)
}

/// Return the byte immediately after the first sentence terminator and any
/// closing quote/bracket. Every returned offset is obtained from
/// `char_indices`, so callers never slice arbitrary UTF-8 at a byte guess.
fn first_sentence_boundary(text: &str) -> Option<usize> {
    let characters = text.char_indices().collect::<Vec<_>>();
    for (position, &(_, character)) in characters.iter().enumerate() {
        if !matches!(character, '.' | '!' | '?') {
            continue;
        }
        if character == '.' && is_non_terminal_period(&characters, position, text) {
            continue;
        }

        let mut after = position + 1;
        while characters.get(after).is_some_and(|(_, character)| {
            matches!(
                character,
                '"' | '\'' | '\u{2019}' | '\u{201d}' | ')' | ']' | '}'
            )
        }) {
            after += 1;
        }
        if characters
            .get(after)
            .is_some_and(|(_, character)| !character.is_whitespace())
        {
            continue;
        }
        return Some(
            characters
                .get(after)
                .map_or(text.len(), |(next_byte, _)| *next_byte),
        );
    }
    None
}

fn is_non_terminal_period(characters: &[(usize, char)], position: usize, text: &str) -> bool {
    let previous = position
        .checked_sub(1)
        .and_then(|index| characters.get(index));
    let next = characters.get(position + 1);
    if previous.is_some_and(|(_, character)| character.is_ascii_digit())
        && next.is_some_and(|(_, character)| character.is_ascii_digit())
    {
        return true;
    }
    if next.is_some_and(|(_, character)| *character == '.') {
        return true;
    }

    let byte = characters[position].0;
    let word = text[..byte]
        .chars()
        .rev()
        .take_while(|character| character.is_alphabetic())
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .to_lowercase();
    word.chars().count() == 1
        || matches!(
            word.as_str(),
            "mr" | "mrs" | "ms" | "dr" | "prof" | "sr" | "jr" | "st" | "vs" | "etc"
        )
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::generate::mock::MockGenerator;
    use crate::generate::{Constraint, Sampling};

    fn request() -> GenerationRequest {
        GenerationRequest {
            system: None,
            prompt: "continue".to_string(),
            max_tokens: Some(12),
            constraint: Constraint::Text { stop: Vec::new() },
            sampling: Sampling::default(),
        }
    }

    fn scripted(text: &str, stop: StopReason) -> MockGenerator {
        let generator = MockGenerator::default();
        generator.scripted.lock().unwrap().push_back(Generated {
            text: text.to_string(),
            tokens: 12,
            stop,
        });
        generator
    }

    #[test]
    fn a_sentence_boundary_wins_before_token_exhaustion() {
        let generator = scripted(
            "A complete sentence. A second sentence keeps running",
            StopReason::MaxTokens,
        );
        let mut streamed = String::new();
        let generated = generate_to_sentence_boundary(&generator, request(), &mut |chunk| {
            streamed.push_str(chunk);
            ControlFlow::Continue(())
        })
        .unwrap();

        assert_eq!(generated.text, "A complete sentence.");
        assert_eq!(streamed, generated.text);
        assert_eq!(generated.stop, StopReason::ProseBoundary);
    }

    #[test]
    fn abbreviations_decimals_and_unicode_closers_do_not_split_early() {
        let generator = scripted(
            "Dr. Chen measured 3.5 units and said \u{201c}It worked!\u{201d} More text",
            StopReason::MaxTokens,
        );
        let generated =
            generate_to_sentence_boundary(
                &generator,
                request(),
                &mut |_| ControlFlow::Continue(()),
            )
            .unwrap();

        assert_eq!(
            generated.text,
            "Dr. Chen measured 3.5 units and said \u{201c}It worked!\u{201d}"
        );
        assert_eq!(generated.stop, StopReason::ProseBoundary);
    }

    #[test]
    fn an_external_consumer_break_remains_cancellation() {
        let generator = scripted("A complete sentence. More text", StopReason::MaxTokens);
        let generated =
            generate_to_sentence_boundary(&generator, request(), &mut |_| ControlFlow::Break(()))
                .unwrap();
        assert_eq!(generated.stop, StopReason::Cancelled);
    }

    #[test]
    fn unfinished_token_exhaustion_is_preserved() {
        let generator = scripted("An unfinished clause", StopReason::MaxTokens);
        let generated =
            generate_to_sentence_boundary(
                &generator,
                request(),
                &mut |_| ControlFlow::Continue(()),
            )
            .unwrap();
        assert_eq!(generated.stop, StopReason::MaxTokens);
    }
}
