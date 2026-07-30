//! Answer one completed, ranked search-result snapshot, grounded in citations.
//!
//! Every sentence must close with the bracketed number of a supplied source.
//! The per-request grammar is what makes that unrepresentable rather than
//! merely requested: an uncited claim and a citation to a source that was never
//! supplied are both outside the language, so there is no parse-and-retry path.
//!
//! Source numbering follows the caller's file order exactly -- the first
//! excerpt of every file is always emitted, so no file is silently dropped and
//! `[k]` always denotes the caller's k-th file. The UI relies on that to turn
//! each citation into a link to the right document.

use std::ops::ControlFlow;

use serde::{Deserialize, Serialize};

use crate::generate::{
    grammar::Grammar, truncate_chars, Constraint, Generated, GenerationRequest, Generator, Sampling,
};

pub const MAX_FILES: usize = 5;
pub const MAX_EXCERPTS_PER_FILE: usize = 2;
pub const MAX_EXCERPT_CHARS: usize = 420;
pub const MAX_TOTAL_EXCERPT_CHARS: usize = MAX_FILES * MAX_EXCERPTS_PER_FILE * MAX_EXCERPT_CHARS;
const MAX_QUERY_CHARS: usize = 500;
const MAX_TITLE_CHARS: usize = 200;
pub const MAX_SENTENCE_BODY_CHARS: usize = 128;
pub const MAX_SENTENCES: usize = 3;
/// Each non-EOS token accepted by the grammar consumes at least one character.
/// This is the exact maximum character count: three bodies, three
/// three-character citations, three periods, two separators, and one optional
/// final newline.
const MAX_TOKENS: usize =
    MAX_SENTENCES * (MAX_SENTENCE_BODY_CHARS + 3 + 1) + (MAX_SENTENCES - 1) + 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResultsSummaryFile {
    pub title: String,
    pub excerpts: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResultsSummaryInput {
    pub query: String,
    pub files: Vec<SearchResultsSummaryFile>,
}

const PROMPT_HEADER: &str =
    "Read the numbered evidence and answer the question at the end. Use only \
factual findings from the evidence. Write one to three short sentences in plain \
prose. End every sentence with its supporting document number in brackets, then \
a period.\n\n\
Evidence:\n";

/// `source_numbers` is the closed set of caller positions that actually carry
/// evidence. Keeping those positions rather than renumbering non-empty files is
/// what makes a generated `[k]` open the UI's k-th document.
pub fn build_grammar(source_numbers: &[usize]) -> String {
    let digits = source_numbers
        .iter()
        .copied()
        .filter(|number| *number > 0)
        .map(|number| format!("\"{number}\""))
        .collect::<Vec<_>>()
        .join(" | ");
    let digits = if digits.is_empty() {
        "\"1\"".to_string()
    } else {
        digits
    };
    let mut grammar = String::new();
    grammar.push_str(&format!(
        "answer ::= sentence (\" \" sentence){{0,{}}} \"\\n\"?\n",
        MAX_SENTENCES - 1
    ));
    grammar.push_str("sentence ::= body cite \".\"\n");
    grammar.push_str(&format!(
        "body ::= [^\\[\\]\\n.!?]{{1,{MAX_SENTENCE_BODY_CHARS}}}\n"
    ));
    grammar.push_str("cite ::= \"[\" digit \"]\"\n");
    grammar.push_str(&format!("digit ::= {digits}\n"));
    grammar
}

fn normalized_excerpt(text: &str) -> String {
    truncate_chars(text.trim(), MAX_EXCERPT_CHARS * 2)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn source_numbers(input: &SearchResultsSummaryInput) -> Vec<usize> {
    input
        .files
        .iter()
        .take(MAX_FILES)
        .enumerate()
        .filter_map(|(index, file)| {
            file.excerpts
                .iter()
                .any(|excerpt| !excerpt.trim().is_empty())
                .then_some(index + 1)
        })
        .collect()
}

fn matches_grammar(grammar_source: &str, text: &str) -> anyhow::Result<bool> {
    let grammar = Grammar::parse(grammar_source)?;
    Ok(grammar
        .advance(&grammar.initial_state(), text)
        .is_some_and(|state| grammar.is_complete(&state)))
}

/// Catch degeneration that is technically shaped like an answer, such as
/// `14141414[1].`. The scan is character-based, so arbitrary Unicode remains
/// safe and a repeated multi-byte character cannot create an invalid slice.
fn has_pathological_repetition(text: &str) -> bool {
    let chars = text.chars().collect::<Vec<_>>();
    for width in 2..=8 {
        let run_chars = width * 4;
        if chars.len() < run_chars {
            continue;
        }
        for start in 0..=chars.len() - run_chars {
            let pattern = &chars[start..start + width];
            if (1..4).all(|repeat| {
                let offset = start + repeat * width;
                chars[offset..offset + width] == *pattern
            }) {
                return true;
            }
        }
    }
    false
}

pub fn build_request(input: &SearchResultsSummaryInput) -> GenerationRequest {
    let mut prompt = String::from(PROMPT_HEADER);

    let mut total_chars = 0;
    for (index, file) in input.files.iter().take(MAX_FILES).enumerate() {
        let mut excerpts = Vec::new();
        for excerpt in &file.excerpts {
            if excerpts.len() == MAX_EXCERPTS_PER_FILE {
                break;
            }
            let normalized = normalized_excerpt(excerpt);
            if normalized.is_empty() {
                continue;
            }
            // The first excerpt of a file is emitted unconditionally so the
            // source is never dropped and citation numbers stay aligned with the
            // caller's file order; later excerpts yield to the shared budget.
            if !excerpts.is_empty() && total_chars >= MAX_TOTAL_EXCERPT_CHARS {
                break;
            }
            let bounded = truncate_chars(&normalized, MAX_EXCERPT_CHARS)
                .trim_end()
                .to_string();
            total_chars += bounded.chars().count();
            excerpts.push(bounded);
        }
        if excerpts.is_empty() {
            continue;
        }
        prompt.push_str(&format!(
            "\n[{}] {}\n",
            index + 1,
            truncate_chars(file.title.trim(), MAX_TITLE_CHARS)
        ));
        for excerpt in excerpts {
            prompt.push_str(&excerpt);
            prompt.push('\n');
        }
    }
    prompt.push_str(&format!(
        "\nEnd evidence.\n\nQuestion: {}\nAnswer:",
        truncate_chars(input.query.trim(), MAX_QUERY_CHARS),
    ));

    let source_numbers = source_numbers(input);

    GenerationRequest {
        system: None,
        prompt,
        max_tokens: MAX_TOKENS,
        constraint: Constraint::Grammar(build_grammar(&source_numbers)),
        sampling: Sampling {
            temperature: 0.2,
            top_p: None,
            top_k: Some(32),
            repeat_penalty: Some((1.15, 64)),
            seed: 0,
        },
    }
}

pub fn summarize_search_results(
    generator: &dyn Generator,
    input: &SearchResultsSummaryInput,
    sink: &mut dyn FnMut(&str) -> ControlFlow<()>,
) -> anyhow::Result<Generated> {
    anyhow::ensure!(!input.query.trim().is_empty(), "search query is empty");
    let source_numbers = source_numbers(input);
    anyhow::ensure!(
        !source_numbers.is_empty(),
        "search results contain no excerpts"
    );

    let request = build_request(input);
    let Constraint::Grammar(grammar_source) = &request.constraint else {
        unreachable!("search result answers always use a grammar");
    };
    let grammar_source = grammar_source.clone();
    // This task withholds its short decode until verification. Streaming an
    // invalid prefix would briefly show the exact repetition/OCR failures these
    // checks are meant to prevent, even though the terminal event later failed.
    // `Generator::generate` still uses the one shared streaming decoder.
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

    // Re-run the exact task contract so a mock or future engine that ignores
    // constraints cannot surface an uncited, overlong, or out-of-range answer.
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
            query: "cache coherence".to_string(),
            files: vec![SearchResultsSummaryFile {
                title: "paper.pdf".to_string(),
                excerpts: vec!["first finding".to_string(), "second finding".to_string()],
            }],
        }
    }

    fn accepts(grammar: &Grammar, text: &str) -> bool {
        grammar
            .advance(&grammar.initial_state(), text)
            .is_some_and(|state| grammar.is_complete(&state))
    }

    #[test]
    fn prompt_numbers_every_file_by_position_and_bounds_excerpts() {
        let repeated = "é".repeat(MAX_EXCERPT_CHARS + 50);
        let mut files = Vec::new();
        for index in 0..(MAX_FILES + 2) {
            files.push(SearchResultsSummaryFile {
                title: format!("file-{index}"),
                excerpts: vec![
                    repeated.clone(),
                    format!("unique-{index}"),
                    format!("second-{index}"),
                    format!("ignored-{index}"),
                ],
            });
        }
        let request = build_request(&SearchResultsSummaryInput {
            query: "query".to_string(),
            files,
        });

        // Numbered by position, capped at MAX_FILES, never renumbered.
        assert!(request.prompt.contains("[1] file-0"));
        assert!(request.prompt.contains("[5] file-4"));
        assert!(!request.prompt.contains("file-5"));
        // At most MAX_EXCERPTS_PER_FILE excerpts survive per file.
        assert!(!request.prompt.contains("ignored-0"));
        // Each excerpt is truncated to the character budget.
        assert!(!request.prompt.contains(&"é".repeat(MAX_EXCERPT_CHARS + 1)));
        assert_eq!(request.max_tokens, MAX_TOKENS);
        assert_eq!(
            request.sampling,
            Sampling {
                temperature: 0.2,
                top_p: None,
                top_k: Some(32),
                repeat_penalty: Some((1.15, 64)),
                seed: 0,
            }
        );
        assert!(matches!(request.constraint, Constraint::Grammar(_)));
        assert!(
            request.prompt.rfind("Question: query").unwrap()
                > request.prompt.rfind("End evidence.").unwrap()
        );
        assert!(!request.prompt.contains("no preamble"));
    }

    #[test]
    fn request_uses_a_grammar_over_the_supplied_source_numbers() {
        let request = build_request(&input());
        let Constraint::Grammar(grammar) = &request.constraint else {
            panic!("expected a grammar constraint");
        };
        let grammar = Grammar::parse(grammar).expect("the answer grammar must compile");
        assert!(accepts(&grammar, "Caches stay coherent [1]."));
        // Uncited claims and out-of-range sources are outside the language.
        assert!(!accepts(&grammar, "Caches stay coherent."));
        assert!(!accepts(&grammar, "Caches stay coherent [2]."));
        assert!(!accepts(
            &grammar,
            "Caches stay coherent. Batching reduces stalls [1]."
        ));
    }

    #[test]
    fn grammar_admits_up_to_max_sentences_of_grounded_prose() {
        let grammar = Grammar::parse(&build_grammar(&[1, 2])).unwrap();
        assert!(accepts(&grammar, "First point [1]. Second point [2]."));
        let longest = (0..MAX_SENTENCES)
            .map(|_| "point [1].")
            .collect::<Vec<_>>()
            .join(" ");
        assert!(accepts(&grammar, &longest));
        let too_many = (0..MAX_SENTENCES + 1)
            .map(|_| "point [1].")
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!accepts(&grammar, &too_many));
    }

    #[test]
    fn every_supplied_source_count_produces_a_compiling_grammar() {
        for sources in 1..=MAX_FILES {
            let numbers = (1..=sources).collect::<Vec<_>>();
            Grammar::parse(&build_grammar(&numbers))
                .unwrap_or_else(|_| panic!("grammar for {sources} sources must compile"));
        }
    }

    #[test]
    fn sentence_bodies_are_finite_and_the_token_budget_reaches_the_longest_answer() {
        let grammar = Grammar::parse(&build_grammar(&[1])).unwrap();
        let longest_body = "x".repeat(MAX_SENTENCE_BODY_CHARS);
        let longest = (0..MAX_SENTENCES)
            .map(|_| format!("{longest_body}[1]."))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(accepts(&grammar, &longest));
        assert!(!accepts(
            &grammar,
            &format!("{}[1].", "x".repeat(MAX_SENTENCE_BODY_CHARS + 1))
        ));
        assert_eq!(longest.chars().count() + 1, MAX_TOKENS);
    }

    #[test]
    fn empty_files_keep_their_caller_positions_out_of_the_citation_language() {
        let input = SearchResultsSummaryInput {
            query: "query".to_string(),
            files: vec![
                SearchResultsSummaryFile {
                    title: "empty.pdf".to_string(),
                    excerpts: vec![" ".to_string()],
                },
                SearchResultsSummaryFile {
                    title: "evidence.pdf".to_string(),
                    excerpts: vec!["relevant evidence".to_string()],
                },
            ],
        };
        let request = build_request(&input);
        assert!(request.prompt.contains("[2] evidence.pdf"));
        let Constraint::Grammar(source) = request.constraint else {
            panic!("expected grammar");
        };
        let grammar = Grammar::parse(&source).unwrap();
        assert!(accepts(&grammar, "Relevant evidence supports this [2]."));
        assert!(!accepts(&grammar, "Relevant evidence supports this [1]."));
    }

    #[test]
    fn streams_and_verifies_the_answer() {
        let generator = MockGenerator::scripted(["Caching reduces stalls [1]."]);
        let mut streamed = String::new();
        let generated = summarize_search_results(&generator, &input(), &mut |token| {
            streamed.push_str(token);
            ControlFlow::Continue(())
        })
        .unwrap();

        assert_eq!(streamed, generated.text);
        assert_eq!(generated.stop, StopReason::Eos);
    }

    #[test]
    fn rejects_an_answer_without_any_citation() {
        let generator = MockGenerator::scripted(["Caching reduces stalls everywhere."]);
        assert!(
            summarize_search_results(&generator, &input(), &mut |_| ControlFlow::Continue(()))
                .is_err()
        );
    }

    #[test]
    fn rejects_a_citation_outside_the_supplied_sources() {
        let generator = MockGenerator::scripted(["Caching reduces stalls [3]."]);
        assert!(
            summarize_search_results(&generator, &input(), &mut |_| ControlFlow::Continue(()))
                .is_err()
        );
    }

    #[test]
    fn rejects_numeric_and_repeating_degeneration_even_when_it_is_cited() {
        for answer in [
            "14141414141414141414[1].",
            "Econometric evidence 14141414141414141414[1].",
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
            assert!(
                streamed.is_empty(),
                "invalid output must never be displayed"
            );
        }
    }

    #[test]
    fn cancellation_after_validation_is_still_an_error() {
        let generator = MockGenerator::scripted(["Caching reduces stalls [1]."]);
        assert!(
            summarize_search_results(&generator, &input(), &mut |_| ControlFlow::Break(()))
                .is_err()
        );
    }

    #[test]
    fn empty_inputs_issue_no_request() {
        let generator = MockGenerator::scripted(["unused"]);
        let mut empty = input();
        empty.files[0].excerpts = vec![" ".to_string()];
        assert!(
            summarize_search_results(&generator, &empty, &mut |_| ControlFlow::Continue(()))
                .is_err()
        );
        assert_eq!(generator.request_count(), 0);
    }
}
