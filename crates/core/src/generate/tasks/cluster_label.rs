//! Name a bookmark cluster from its members.
//!
//! The measured raw output for this prompt was a three-item bulleted list, not
//! a label (spec §14). The grammar is what makes that unrepresentable; there is
//! no parse-and-retry path here on purpose.

use crate::generate::{
    truncate_chars, Constraint, GenerationRequest, Generator, Sampling, StopReason,
};

/// Bumping the prompt or the grammar changes what a cached label means, so it
/// must be accompanied by a bump of the persisted recipe version.
pub const LABEL_GRAMMAR: &str = r#"
label ::= word (" " word){1,5} "\n"?
word  ::= [A-Za-z0-9][A-Za-z0-9'-]{0,23}
"#;

const PROMPT_HEADER: &str =
    "These research notes were grouped together because they are about the \
same topic. Reply with a short topic label of 2 to 6 words. No punctuation, no \
list, no explanation.\n\nNotes:\n";

/// Members beyond this add prompt length without changing the label.
pub const MAX_MEMBERS: usize = 12;
/// Per-member character budget. Character-aware, never byte-sliced.
pub const MAX_MEMBER_CHARS: usize = 240;
const MAX_WORD_CHARS: usize = 24;
const MAX_WORDS: usize = 6;
/// The grammar permits at most six 24-character words, five separating spaces,
/// and one terminal newline. Every permitted non-EOS token consumes at least
/// one of those ASCII characters, so this budget can reach every terminal state.
const MAX_TOKENS: usize = MAX_WORDS * MAX_WORD_CHARS + (MAX_WORDS - 1) + 1;

pub fn build_request(members: &[&str]) -> GenerationRequest {
    let mut prompt = String::from(PROMPT_HEADER);
    for member in members.iter().take(MAX_MEMBERS) {
        let trimmed = member.trim();
        if trimmed.is_empty() {
            continue;
        }
        prompt.push_str("- ");
        prompt.push_str(truncate_chars(trimmed, MAX_MEMBER_CHARS).trim_end());
        prompt.push('\n');
    }
    prompt.push_str("\nLabel:");

    GenerationRequest {
        system: None,
        prompt,
        max_tokens: MAX_TOKENS,
        constraint: Constraint::Grammar(LABEL_GRAMMAR.to_string()),
        // Greedy: a cluster must not rename itself when the pane redraws.
        sampling: Sampling::default(),
    }
}

pub fn cluster_label(generator: &dyn Generator, members: &[&str]) -> anyhow::Result<String> {
    anyhow::ensure!(!members.is_empty(), "cluster has no members to label");
    let generated = generator.generate(build_request(members))?;

    if !generated.is_complete() {
        anyhow::bail!(
            "cluster label generation did not finish cleanly ({:?})",
            generated.stop
        );
    }
    if generated.stop == StopReason::MaxTokens {
        anyhow::bail!("cluster label hit the token cap");
    }

    let label = generated.text.trim();
    anyhow::ensure!(!label.is_empty(), "generator returned an empty label");

    // The grammar already guarantees the shape; this check exists so that a
    // generator which ignores the constraint (a mock, or a future engine
    // without masking) fails loudly instead of writing junk to the cache.
    let words = label.split(' ').count();
    anyhow::ensure!(
        (2..=6).contains(&words),
        "cluster label '{label}' has {words} words, expected 2-6"
    );

    Ok(label.to_string())
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::generate::grammar::Grammar;
    use crate::generate::mock::MockGenerator;

    #[test]
    fn returns_the_generated_label() {
        let generator = MockGenerator::scripted(["Cache invalidation strategy"]);
        let label = cluster_label(&generator, &["a note", "another note"]).unwrap();
        assert_eq!(label, "Cache invalidation strategy");
        assert_eq!(generator.request_count(), 1);
    }

    #[test]
    fn caps_members_and_truncates_each_one() {
        let long = "x".repeat(MAX_MEMBER_CHARS + 100);
        let members: Vec<&str> = std::iter::repeat_n(long.as_str(), 30).collect();
        let request = build_request(&members);

        assert_eq!(request.prompt.matches("\n- ").count() + 1, MAX_MEMBERS + 1);
        assert!(!request.prompt.contains(&"x".repeat(MAX_MEMBER_CHARS + 1)));
    }

    #[test]
    fn truncation_is_character_aware() {
        let member = "é".repeat(MAX_MEMBER_CHARS + 10);
        let request = build_request(&[member.as_str()]);
        // Would have panicked on a byte slice; assert the content survived.
        assert!(request.prompt.contains(&"é".repeat(MAX_MEMBER_CHARS)));
    }

    #[test]
    fn request_uses_the_label_grammar_and_greedy_sampling() {
        let request = build_request(&["note"]);
        assert_eq!(
            request.constraint,
            Constraint::Grammar(LABEL_GRAMMAR.to_string())
        );
        assert_eq!(request.sampling, Sampling::default());
        assert_eq!(request.max_tokens, MAX_TOKENS);
    }

    #[test]
    fn the_shipped_grammar_compiles() {
        Grammar::parse(LABEL_GRAMMAR).expect("the label grammar must compile");
    }

    #[test]
    fn grammar_accepts_a_terminal_newline_and_bounds_word_length() {
        let grammar = Grammar::parse(LABEL_GRAMMAR).unwrap();
        let accepts = |text: &str| {
            grammar
                .advance(&grammar.initial_state(), text)
                .is_some_and(|state| grammar.is_complete(&state))
        };

        assert!(accepts("Cache invalidation\n"));
        assert!(accepts(&format!("{} topic", "x".repeat(MAX_WORD_CHARS))));
        assert!(!accepts(&format!(
            "{} topic",
            "x".repeat(MAX_WORD_CHARS + 1)
        )));
    }

    #[test]
    fn token_budget_reaches_the_longest_terminal_label() {
        let grammar = Grammar::parse(LABEL_GRAMMAR).unwrap();
        let longest = std::iter::repeat_n("x".repeat(MAX_WORD_CHARS), MAX_WORDS)
            .collect::<Vec<_>>()
            .join(" ")
            + "\n";
        let state = grammar
            .advance(&grammar.initial_state(), &longest)
            .expect("the longest label must be accepted");

        assert_eq!(longest.chars().count(), MAX_TOKENS);
        assert!(grammar.is_complete(&state));
        assert!(grammar
            .allowed_next(
                &state,
                &["x".to_string(), " ".to_string(), "\n".to_string()],
            )
            .iter()
            .all(|allowed| !allowed));
    }

    #[test]
    fn rejects_a_label_outside_the_word_bounds() {
        let generator = MockGenerator::scripted(["Cache"]);
        assert!(cluster_label(&generator, &["note"]).is_err());

        let generator = MockGenerator::scripted(["one two three four five six seven"]);
        assert!(cluster_label(&generator, &["note"]).is_err());
    }

    #[test]
    fn rejects_an_empty_member_list_without_calling_the_generator() {
        let generator = MockGenerator::scripted(["unused"]);
        assert!(cluster_label(&generator, &[]).is_err());
        assert_eq!(generator.request_count(), 0);
    }

    #[test]
    fn propagates_generator_failure() {
        let generator = MockGenerator::default();
        assert!(cluster_label(&generator, &["note"]).is_err());
    }
}
