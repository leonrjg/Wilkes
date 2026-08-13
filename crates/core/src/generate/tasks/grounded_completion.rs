//! Sentence-scale synthesis and the verification gate for grounded completion.

use std::collections::HashSet;
use std::ops::ControlFlow;

use crate::completion::{CompletionMode, PromptFormat};
use crate::generate::{Constraint, Generated, GenerationRequest, Generator, Sampling};

use super::prose::generate_to_sentence_boundary;

#[derive(Clone, Debug)]
pub struct GroundedCompletionInput {
    pub prompt: String,
    pub mode: CompletionMode,
    pub prompt_format: PromptFormat,
    pub prefix_tail: String,
    pub suffix_head: String,
    pub grounding_text: String,
    pub avoid_suggestions: Vec<String>,
    pub seed: u64,
    pub at_paragraph_start: bool,
    pub at_sentence_start: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuppressionReason {
    Empty,
    PrefixEcho,
    Repetition,
    BrokenSuffixJoin,
    UngroundedEntity,
    DuplicateSuggestion,
    Incomplete,
}

impl SuppressionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::PrefixEcho => "prefix_echo",
            Self::Repetition => "repetition",
            Self::BrokenSuffixJoin => "broken_suffix_join",
            Self::UngroundedEntity => "ungrounded_entity",
            Self::DuplicateSuggestion => "duplicate_suggestion",
            Self::Incomplete => "incomplete",
        }
    }
}

pub fn build_request(input: &GroundedCompletionInput) -> GenerationRequest {
    let stop = if input.at_paragraph_start {
        vec!["\n\n".to_string()]
    } else if input.at_sentence_start {
        vec!["\n\n".to_string()]
    } else {
        vec!["\n".to_string()]
    };
    let prompt = match input.prompt_format {
        PromptFormat::InstructContinue => input.prompt.clone(),
        PromptFormat::InstructInfill => format!(
            "The completion must join cleanly to the text after <CURSOR>.\n\n{}",
            input.prompt
        ),
        PromptFormat::NativeFim => format!(
            "<|fim_prefix|>{}<|fim_middle|>",
            input.prompt.replace("<CURSOR>", "<|fim_suffix|>")
        ),
    };
    GenerationRequest {
        system: None,
        prompt,
        max_tokens: None,
        constraint: Constraint::Text { stop },
        sampling: Sampling {
            temperature: 0.5,
            repeat_penalty: Some((1.1, 64)),
            seed: input.seed,
            ..Sampling::default()
        },
    }
}

pub fn generate_and_verify(
    generator: &dyn Generator,
    input: &GroundedCompletionInput,
    sink: &mut dyn FnMut(&str) -> ControlFlow<()>,
) -> Result<Generated, SuppressionReason> {
    let generated =
        generate_to_sentence_boundary(generator, build_request(input), sink).map_err(|error| {
            tracing::warn!(
                task = "grounded_completion",
                model = generator.model_id(),
                error = %format!("{error:#}"),
                "prose generation failed"
            );
            SuppressionReason::Incomplete
        })?;
    if !generated.is_complete() {
        tracing::info!(
            task = "grounded_completion",
            model = generator.model_id(),
            stop = ?generated.stop,
            tokens = generated.tokens,
            "prose generation did not finish cleanly"
        );
        return Err(SuppressionReason::Incomplete);
    }
    if let Err(reason) = verify_candidate(&generated.text, input) {
        tracing::info!(
            task = "grounded_completion",
            model = generator.model_id(),
            reason = reason.as_str(),
            stop = ?generated.stop,
            tokens = generated.tokens,
            candidate_chars = generated.text.chars().count(),
            "grounded completion candidate rejected"
        );
        return Err(reason);
    }
    Ok(generated)
}

pub fn verify_candidate(
    text: &str,
    input: &GroundedCompletionInput,
) -> Result<String, SuppressionReason> {
    let candidate = text.trim();
    if candidate.is_empty() {
        return Err(SuppressionReason::Empty);
    }
    if is_prefix_echo(candidate, &input.prefix_tail) {
        return Err(SuppressionReason::PrefixEcho);
    }
    if is_repetitive(candidate) {
        return Err(SuppressionReason::Repetition);
    }
    if input
        .avoid_suggestions
        .iter()
        .any(|previous| same_suggestion(candidate, previous))
    {
        return Err(SuppressionReason::DuplicateSuggestion);
    }
    if input.mode == CompletionMode::Bridge && broken_suffix_join(candidate, &input.suffix_head) {
        return Err(SuppressionReason::BrokenSuffixJoin);
    }
    if has_ungrounded_entity(candidate, &input.grounding_text) {
        return Err(SuppressionReason::UngroundedEntity);
    }
    Ok(candidate.to_string())
}

fn same_suggestion(left: &str, right: &str) -> bool {
    let normalize = |text: &str| words(text).join(" ");
    let left = normalize(left);
    !left.is_empty() && left == normalize(right)
}

fn words(text: &str) -> Vec<String> {
    text.split(|character: char| {
        !character.is_alphanumeric() && character != '-' && character != '\''
    })
    .filter(|word| word.chars().count() >= 2)
    .map(|word| word.to_lowercase())
    .collect()
}

fn is_prefix_echo(candidate: &str, prefix: &str) -> bool {
    let candidate_words = words(candidate);
    let prefix_words = words(prefix);
    let span = candidate_words.len().min(prefix_words.len()).min(8);
    span >= 3 && candidate_words[..span] == prefix_words[prefix_words.len() - span..]
}

fn is_repetitive(candidate: &str) -> bool {
    let words = words(candidate);
    if words.len() < 8 {
        return false;
    }
    let mut seen = HashSet::new();
    let mut duplicates = 0;
    for window in words.windows(3) {
        if !seen.insert(window.join(" ")) {
            duplicates += 1;
        }
    }
    duplicates >= 2 || words.windows(6).any(|window| window[..3] == window[3..])
}

fn broken_suffix_join(candidate: &str, suffix: &str) -> bool {
    let suffix = suffix.trim_start();
    if suffix.is_empty() {
        return false;
    }
    let candidate_words = words(candidate);
    let suffix_words = words(suffix);
    if let (Some(last), Some(first)) = (candidate_words.last(), suffix_words.first()) {
        if last == first {
            return true;
        }
    }
    let candidate_last = candidate
        .chars()
        .rev()
        .find(|character| !character.is_whitespace());
    let suffix_first = suffix.chars().next();
    matches!((candidate_last, suffix_first), (Some('.' | '!' | '?'), Some(character)) if character.is_lowercase())
}

fn has_ungrounded_entity(candidate: &str, grounding: &str) -> bool {
    let grounding_lower = grounding.to_lowercase();
    let mut sentence_start = true;
    for raw in candidate.split_whitespace() {
        let token =
            raw.trim_matches(|character: char| !character.is_alphanumeric() && character != '-');
        let first = token.chars().next();
        let acronym = token.chars().count() >= 3
            && token
                .chars()
                .all(|character| !character.is_alphabetic() || character.is_uppercase());
        let title_case = token.chars().skip(1).any(char::is_lowercase);
        let looks_named = token.chars().count() >= 3
            && first.is_some_and(char::is_uppercase)
            && (acronym || (!sentence_start && title_case));
        if looks_named && !grounding_lower.contains(&token.to_lowercase()) {
            return true;
        }
        sentence_start = raw.ends_with('.') || raw.ends_with('!') || raw.ends_with('?');
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::mock::MockGenerator;
    use crate::generate::{Generated, StopReason};

    fn input(mode: CompletionMode) -> GroundedCompletionInput {
        GroundedCompletionInput {
            prompt: "prompt".into(),
            mode,
            prompt_format: PromptFormat::InstructContinue,
            prefix_tail: "Earlier material about cache invalidation".into(),
            suffix_head: "and the caller resumes.".into(),
            grounding_text: "Cache invalidation is described by Martin Fowler.".into(),
            avoid_suggestions: Vec::new(),
            seed: 0,
            at_paragraph_start: false,
            at_sentence_start: false,
        }
    }

    #[test]
    fn accepts_grounded_prose_and_rejects_core_failure_modes() {
        assert!(verify_candidate(
            "The cache entry then expires.",
            &input(CompletionMode::Append)
        )
        .is_ok());
        assert_eq!(
            verify_candidate(
                "Earlier material about cache invalidation remains.",
                &input(CompletionMode::Append)
            ),
            Err(SuppressionReason::PrefixEcho)
        );
        assert_eq!(
            verify_candidate(
                "aa bb cc aa bb cc aa bb cc.",
                &input(CompletionMode::Append)
            ),
            Err(SuppressionReason::Repetition)
        );
        assert_eq!(
            verify_candidate(
                "It follows. NASA confirms this.",
                &input(CompletionMode::Append)
            ),
            Err(SuppressionReason::UngroundedEntity)
        );
    }

    #[test]
    fn bridge_rejects_duplicate_suffix_opening() {
        assert_eq!(
            verify_candidate("It concludes with and", &input(CompletionMode::Bridge)),
            Err(SuppressionReason::BrokenSuffixJoin)
        );
    }

    #[test]
    fn regeneration_rejects_a_previous_suggestion_despite_case_or_punctuation() {
        let mut value = input(CompletionMode::Append);
        value.avoid_suggestions = vec!["The cache entry then expires!".into()];
        assert_eq!(
            verify_candidate("the cache entry then expires.", &value),
            Err(SuppressionReason::DuplicateSuggestion)
        );
    }

    #[test]
    fn request_is_unbounded_and_uses_completion_sampling() {
        let mut value = input(CompletionMode::Append);
        value.seed = 42;
        let request = build_request(&value);
        assert_eq!(request.max_tokens, None);
        assert_eq!(request.sampling.temperature, 0.5);
        assert_eq!(request.sampling.seed, 42);
    }

    #[test]
    fn native_fim_format_uses_model_control_tokens() {
        let mut value = input(CompletionMode::Bridge);
        value.prompt = "before<CURSOR>after".to_string();
        value.prompt_format = PromptFormat::NativeFim;
        let request = build_request(&value);
        assert!(request.prompt.starts_with("<|fim_prefix|>"));
        assert!(request.prompt.contains("<|fim_suffix|>"));
        assert!(request.prompt.ends_with("<|fim_middle|>"));
    }

    #[test]
    fn synthesis_accepts_a_verified_sentence_before_token_exhaustion() {
        let generator = MockGenerator::default();
        generator.scripted.lock().unwrap().push_back(Generated {
            text: "Cache invalidation improves reliability. Extra output continues".into(),
            tokens: 120,
            stop: StopReason::MaxTokens,
        });
        let mut value = input(CompletionMode::Append);
        value.grounding_text = "Cache invalidation improves reliability.".into();

        let generated =
            generate_and_verify(&generator, &value, &mut |_| ControlFlow::Continue(())).unwrap();
        assert_eq!(generated.text, "Cache invalidation improves reliability.");
        assert_eq!(generated.stop, StopReason::ProseBoundary);
    }
}
