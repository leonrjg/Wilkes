//! Synthesize a short cited answer from a bounded, rank-preserving set of
//! cleaned search passages.
//!
//! Search relevance belongs to the search provider. The caller may remove
//! malformed evidence, but it must not promote passages with a second lexical
//! or model-based score. This task therefore consumes passages in caller order
//! and lets the configured LLM interpret the open-ended query. A grammar limits
//! every generated sentence to citations from the supplied sources.

use std::ops::ControlFlow;

use serde::{Deserialize, Serialize};

use crate::generate::{
    grammar::Grammar, Constraint, Generated, GenerationRequest, Generator, Sampling,
};

pub const MAX_SOURCES: usize = 5;
pub const MAX_PASSAGES: usize = 6;
pub const MAX_PASSAGE_CHARS: usize = 700;
pub const MAX_SENTENCES: usize = 3;
const MAX_QUERY_CHARS: usize = 500;
const MAX_TITLE_CHARS: usize = 200;
const MAX_TOKENS: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResultsSummarySource {
    pub title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResultsSummaryPassage {
    pub text: String,
    /// Zero-based index into `SearchResultsSummaryInput::sources`.
    pub source_index: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResultsSummaryInput {
    pub query: String,
    pub sources: Vec<SearchResultsSummarySource>,
    pub passages: Vec<SearchResultsSummaryPassage>,
}

const PROMPT_HEADER: &str = "Answer the search question using only the ranked evidence below. The \
evidence is ordered from most to least relevant. If it does not directly answer \
the question, say that rather than filling gaps with outside knowledge. Write \
one to three concise sentences. End every sentence with the supporting source \
number in brackets.\n\nRanked evidence:\n";

fn normalized_passage(text: &str) -> anyhow::Result<String> {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    anyhow::ensure!(!normalized.is_empty(), "search passage is empty");
    anyhow::ensure!(
        normalized.chars().count() <= MAX_PASSAGE_CHARS,
        "search passage exceeds {MAX_PASSAGE_CHARS} characters"
    );
    anyhow::ensure!(
        normalized
            .chars()
            .filter(|character| character.is_alphabetic())
            .count()
            >= 20,
        "search passage contains no substantive prose"
    );
    Ok(normalized)
}

fn validated_passages(input: &SearchResultsSummaryInput) -> anyhow::Result<Vec<(usize, String)>> {
    anyhow::ensure!(
        input.sources.len() <= MAX_SOURCES,
        "search summary has more than {MAX_SOURCES} sources"
    );
    anyhow::ensure!(
        input.passages.len() <= MAX_PASSAGES,
        "search summary has more than {MAX_PASSAGES} passages"
    );
    input
        .passages
        .iter()
        .map(|passage| {
            anyhow::ensure!(
                passage.source_index < input.sources.len(),
                "search passage refers to missing source {}",
                passage.source_index
            );
            Ok((passage.source_index, normalized_passage(&passage.text)?))
        })
        .collect()
}

fn source_numbers(passages: &[(usize, String)], source_count: usize) -> Vec<usize> {
    (0..source_count)
        .filter(|source_index| {
            passages
                .iter()
                .any(|(passage_source, _)| passage_source == source_index)
        })
        .map(|source_index| source_index + 1)
        .collect()
}

pub fn build_grammar(source_numbers: &[usize]) -> String {
    let digits = source_numbers
        .iter()
        .map(|number| format!("\"{number}\""))
        .collect::<Vec<_>>()
        .join(" | ");
    let mut grammar = String::new();
    grammar.push_str(&format!(
        "answer ::= sentence (\" \" sentence){{0,{}}} \"\\n\"?\n",
        MAX_SENTENCES - 1
    ));
    grammar.push_str("sentence ::= body \" [\" digit \"].\"\n");
    grammar.push_str("body ::= [^\\[\\]\\n.!?]+\n");
    grammar.push_str(&format!("digit ::= {digits}\n"));
    grammar
}

fn matches_grammar(grammar_source: &str, text: &str) -> anyhow::Result<bool> {
    let grammar = Grammar::parse(grammar_source)?;
    Ok(grammar
        .advance(&grammar.initial_state(), text)
        .is_some_and(|state| grammar.is_complete(&state)))
}

/// Reject decoder degeneration even when it happens to satisfy citation shape.
/// The scan is character-based and therefore safe for arbitrary Unicode.
fn has_pathological_repetition(text: &str) -> bool {
    let characters = text.chars().collect::<Vec<_>>();
    for width in 2..=8 {
        let run_chars = width * 4;
        if characters.len() < run_chars {
            continue;
        }
        for start in 0..=characters.len() - run_chars {
            let pattern = &characters[start..start + width];
            if (1..4).all(|repeat| {
                let offset = start + repeat * width;
                characters[offset..offset + width] == *pattern
            }) {
                return true;
            }
        }
    }
    false
}

pub fn build_request(input: &SearchResultsSummaryInput) -> anyhow::Result<GenerationRequest> {
    anyhow::ensure!(!input.query.trim().is_empty(), "search query is empty");
    let passages = validated_passages(input)?;
    anyhow::ensure!(!passages.is_empty(), "search results contain no passages");
    let source_numbers = source_numbers(&passages, input.sources.len());

    let mut prompt = String::from(PROMPT_HEADER);
    for (source_index, passage) in &passages {
        let source = &input.sources[*source_index];
        let title = source
            .title
            .trim()
            .chars()
            .take(MAX_TITLE_CHARS)
            .collect::<String>();
        prompt.push_str(&format!(
            "\n[{}] {}\n{}\n",
            source_index + 1,
            title,
            passage
        ));
    }
    let query = input
        .query
        .trim()
        .chars()
        .take(MAX_QUERY_CHARS)
        .collect::<String>();
    prompt.push_str(&format!(
        "\nEnd evidence.\n\nSearch question: {query}\nAnswer:"
    ));

    Ok(GenerationRequest {
        system: None,
        prompt,
        max_tokens: Some(MAX_TOKENS),
        constraint: Constraint::Grammar(build_grammar(&source_numbers)),
        sampling: Sampling::default(),
    })
}

pub fn summarize_search_results(
    generator: &dyn Generator,
    input: &SearchResultsSummaryInput,
    sink: &mut dyn FnMut(&str) -> ControlFlow<()>,
) -> anyhow::Result<Generated> {
    let request = build_request(input)?;
    let Constraint::Grammar(grammar_source) = &request.constraint else {
        unreachable!("search result answers always use a citation grammar");
    };
    let grammar_source = grammar_source.clone();

    // Verify the complete short answer before displaying any part of it.
    let mut generated = generator.generate(request)?;
    anyhow::ensure!(
        generated.is_complete(),
        "search results summary did not finish cleanly ({:?})",
        generated.stop
    );
    let trimmed = generated.text.trim();
    anyhow::ensure!(
        !trimmed.is_empty(),
        "generator returned an empty search results summary"
    );
    anyhow::ensure!(
        matches_grammar(&grammar_source, trimmed)?,
        "search results answer violates the citation grammar"
    );
    anyhow::ensure!(
        trimmed
            .chars()
            .filter(|character| character.is_alphabetic())
            .count()
            >= 12,
        "search results answer contains no substantive prose"
    );
    anyhow::ensure!(
        !has_pathological_repetition(trimmed),
        "search results answer contains pathological repetition"
    );

    generated.text = trimmed.to_string();
    anyhow::ensure!(
        sink(&generated.text).is_continue(),
        "search results summary was cancelled"
    );
    Ok(generated)
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::generate::grammar::Grammar;
    use crate::generate::mock::MockGenerator;
    use crate::generate::StopReason;

    fn input() -> SearchResultsSummaryInput {
        SearchResultsSummaryInput {
            query: "how caching affects execution".to_string(),
            sources: vec![
                SearchResultsSummarySource {
                    title: "cache.pdf".to_string(),
                },
                SearchResultsSummarySource {
                    title: "runtime.pdf".to_string(),
                },
            ],
            passages: vec![
                SearchResultsSummaryPassage {
                    text: "The measured cache reduced repeated work during program execution."
                        .to_string(),
                    source_index: 0,
                },
                SearchResultsSummaryPassage {
                    text: "Runtime stalls declined after the cache was enabled in the experiment."
                        .to_string(),
                    source_index: 1,
                },
            ],
        }
    }

    fn accepts(grammar: &Grammar, text: &str) -> bool {
        grammar
            .advance(&grammar.initial_state(), text)
            .is_some_and(|state| grammar.is_complete(&state))
    }

    #[test]
    fn request_preserves_passage_order_and_uses_open_ended_grammar() {
        let request = build_request(&input()).unwrap();
        let first = request.prompt.find("The measured cache").unwrap();
        let second = request.prompt.find("Runtime stalls").unwrap();
        assert!(first < second);
        assert!(request
            .prompt
            .contains("ordered from most to least relevant"));
        assert_eq!(request.max_tokens, Some(MAX_TOKENS));
        assert_eq!(request.sampling, Sampling::default());
        assert!(matches!(request.constraint, Constraint::Grammar(_)));
    }

    #[test]
    fn grammar_requires_valid_citations_and_at_most_three_sentences() {
        let grammar = Grammar::parse(&build_grammar(&[1, 2])).unwrap();
        assert!(accepts(&grammar, "Caching reduces repeated work [1]."));
        assert!(accepts(
            &grammar,
            "Caching reduces work [1]. Runtime stalls decline [2]."
        ));
        assert!(!accepts(&grammar, "Caching reduces repeated work."));
        assert!(!accepts(&grammar, "Caching reduces repeated work [3]."));
        assert!(!accepts(&grammar, "One [1]. Two [1]. Three [2]. Four [2]."));
    }

    #[test]
    fn source_language_excludes_sources_without_passages() {
        let mut input = input();
        input.passages.truncate(1);
        let request = build_request(&input).unwrap();
        let Constraint::Grammar(source) = request.constraint else {
            panic!("expected grammar");
        };
        let grammar = Grammar::parse(&source).unwrap();
        assert!(accepts(&grammar, "Supported answer [1]."));
        assert!(!accepts(&grammar, "Unsupported answer [2]."));
    }

    #[test]
    fn validates_bounds_and_sources_before_generation() {
        let generator = MockGenerator::scripted(["unused"]);
        let mut invalid = input();
        invalid.passages[0].source_index = 5;
        assert!(summarize_search_results(&generator, &invalid, &mut |_| {
            ControlFlow::Continue(())
        })
        .is_err());

        invalid = input();
        invalid.passages[0].text = "é".repeat(MAX_PASSAGE_CHARS + 1);
        assert!(summarize_search_results(&generator, &invalid, &mut |_| {
            ControlFlow::Continue(())
        })
        .is_err());
        assert_eq!(generator.request_count(), 0);
    }

    #[test]
    fn streams_only_a_verified_cited_answer() {
        let generator =
            MockGenerator::scripted(["Caching reduces work [1]. Runtime stalls decline [2]."]);
        let mut streamed = String::new();
        let generated = summarize_search_results(&generator, &input(), &mut |delta| {
            streamed.push_str(delta);
            ControlFlow::Continue(())
        })
        .unwrap();
        assert_eq!(streamed, generated.text);
        assert_eq!(generated.stop, StopReason::Eos);
    }

    #[test]
    fn rejects_uncited_out_of_range_and_repeating_answers() {
        for answer in [
            "Caching reduces repeated work.",
            "Caching reduces repeated work [3].",
            "Caching 1414141414141414 reduces repeated work [1].",
        ] {
            let generator = MockGenerator::scripted([answer]);
            let mut streamed = String::new();
            assert!(
                summarize_search_results(&generator, &input(), &mut |delta| {
                    streamed.push_str(delta);
                    ControlFlow::Continue(())
                })
                .is_err(),
                "{answer}"
            );
            assert!(streamed.is_empty());
        }
    }

    #[test]
    fn empty_inputs_issue_no_request() {
        let generator = MockGenerator::scripted(["unused"]);
        let mut empty = input();
        empty.passages.clear();
        assert!(
            summarize_search_results(&generator, &empty, &mut |_| ControlFlow::Continue(()))
                .is_err()
        );
        assert_eq!(generator.request_count(), 0);
    }

    #[test]
    fn cancellation_after_validation_is_an_error() {
        let generator = MockGenerator::scripted(["Caching reduces repeated work [1]."]);
        assert!(
            summarize_search_results(&generator, &input(), &mut |_| ControlFlow::Break(()))
                .is_err()
        );
    }
}
