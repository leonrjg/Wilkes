//! A GBNF subset compiled to an NFA over characters.
//!
//! Scope is deliberately small: literals, character classes, alternation,
//! sequence, `*` / `+` / `?` / `{m}` / `{m,n}`, grouping, and references to
//! other rules. There is no recursion — a rule may not reference itself, even
//! transitively — which is what lets references be inlined and keeps the
//! machine finite.
//!
//! The point of this module is that invalid output is *unrepresentable*: the
//! decode loop masks the logits to the tokens the grammar can accept, so there
//! is no post-hoc parse-and-reject step to get wrong.

use std::collections::{HashMap, HashSet};

const REPEAT_LIMIT: usize = 128;

// ── Character sets ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
struct CharSet {
    ranges: Vec<(char, char)>,
    negated: bool,
}

impl CharSet {
    fn single(c: char) -> Self {
        Self {
            ranges: vec![(c, c)],
            negated: false,
        }
    }

    fn contains(&self, c: char) -> bool {
        let inside = self.ranges.iter().any(|(lo, hi)| c >= *lo && c <= *hi);
        inside != self.negated
    }
}

// ── NFA ───────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
enum Transition {
    Consume { set: CharSet, target: usize },
    Epsilon { target: usize },
}

#[derive(Clone, Copy)]
struct Fragment {
    start: usize,
    accept: usize,
}

/// A compiled grammar. Cheap to clone-free share; matching state lives in
/// [`GrammarState`].
#[derive(Clone, Debug)]
pub struct Grammar {
    states: Vec<Vec<Transition>>,
    start: usize,
    accept: usize,
}

/// The set of NFA states reachable after the accepted prefix. Small (a handful
/// of entries for the grammars here), so a sorted `Vec` beats a `HashSet`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct GrammarState {
    positions: Vec<usize>,
}

impl GrammarState {
    pub fn is_dead(&self) -> bool {
        self.positions.is_empty()
    }
}

impl Grammar {
    pub fn parse(src: &str) -> anyhow::Result<Self> {
        let rules = parse_rules(src)?;
        let root = rules
            .first()
            .ok_or_else(|| anyhow::anyhow!("grammar has no rules"))?
            .0
            .clone();
        let by_name: HashMap<String, Expr> = rules.into_iter().collect();

        let mut builder = NfaBuilder::default();
        let fragment = builder.compile(&by_name[&root], &by_name, &mut vec![root.clone()])?;
        Ok(Self {
            states: builder.states,
            start: fragment.start,
            accept: fragment.accept,
        })
    }

    /// An alternation of literal strings. `Constraint::OneOf` compiles to this
    /// rather than to a separate masking path.
    pub fn one_of(options: &[String]) -> anyhow::Result<Self> {
        anyhow::ensure!(!options.is_empty(), "OneOf constraint has no options");
        let alternatives = options
            .iter()
            .map(|option| Expr::Literal(option.chars().collect()))
            .collect();
        let expr = Expr::Alternate(alternatives);

        let mut builder = NfaBuilder::default();
        let fragment = builder.compile(&expr, &HashMap::new(), &mut Vec::new())?;
        Ok(Self {
            states: builder.states,
            start: fragment.start,
            accept: fragment.accept,
        })
    }

    pub fn initial_state(&self) -> GrammarState {
        let mut positions = Vec::new();
        self.close(self.start, &mut positions);
        GrammarState { positions }
    }

    /// Advance over one character. Returns `None` when the character cannot
    /// appear at this point in the language.
    pub fn advance_char(&self, state: &GrammarState, c: char) -> Option<GrammarState> {
        let mut positions = Vec::new();
        for &position in &state.positions {
            for transition in &self.states[position] {
                if let Transition::Consume { set, target } = transition {
                    if set.contains(c) {
                        self.close(*target, &mut positions);
                    }
                }
            }
        }
        if positions.is_empty() {
            None
        } else {
            Some(GrammarState { positions })
        }
    }

    /// Advance over every character of a decoded token. All-or-nothing: a token
    /// that leaves the language part-way through is rejected outright.
    pub fn advance(&self, state: &GrammarState, text: &str) -> Option<GrammarState> {
        let mut current = state.clone();
        for c in text.chars() {
            current = self.advance_char(&current, c)?;
        }
        Some(current)
    }

    pub fn is_complete(&self, state: &GrammarState) -> bool {
        state.positions.contains(&self.accept)
    }

    /// Validate a complete output against the same automaton used for Candle's
    /// token masking. External backends use this before publishing a buffered
    /// constrained response.
    pub fn accepts(&self, text: &str) -> bool {
        self.advance(&self.initial_state(), text)
            .is_some_and(|state| self.is_complete(&state))
    }

    /// Indices into `vocab` that the grammar permits as the next token.
    ///
    /// This is linear in the vocabulary. For the short, tightly constrained
    /// generations here (tens of tokens) that is fine; a vocabulary trie would
    /// be the fix if longer constrained decodes are ever added.
    pub fn allowed_next(&self, state: &GrammarState, vocab: &[String]) -> Vec<bool> {
        vocab
            .iter()
            .map(|token| !token.is_empty() && self.advance(state, token).is_some())
            .collect()
    }

    /// Epsilon closure of `state`, appended to `out` without duplicates.
    fn close(&self, state: usize, out: &mut Vec<usize>) {
        let mut stack = vec![state];
        while let Some(current) = stack.pop() {
            if out.contains(&current) {
                continue;
            }
            out.push(current);
            for transition in &self.states[current] {
                if let Transition::Epsilon { target } = transition {
                    stack.push(*target);
                }
            }
        }
    }
}

/// Translate this deliberately non-recursive GBNF subset into an anchored
/// JSON-Schema/ECMAScript regular expression. Ollama accepts JSON Schema as its
/// structured-output contract; keeping the translation beside the parser
/// prevents a second, subtly different grammar implementation.
pub fn json_schema_pattern(src: &str) -> anyhow::Result<String> {
    let rules = parse_rules(src)?;
    let root = rules
        .first()
        .ok_or_else(|| anyhow::anyhow!("grammar has no rules"))?
        .0
        .clone();
    let by_name: HashMap<String, Expr> = rules.into_iter().collect();
    let expression = expression_pattern(&by_name[&root], &by_name, &mut vec![root])?;
    Ok(format!("^(?:{expression})$"))
}

fn expression_pattern(
    expression: &Expr,
    rules: &HashMap<String, Expr>,
    stack: &mut Vec<String>,
) -> anyhow::Result<String> {
    Ok(match expression {
        Expr::Literal(characters) => {
            let mut output = String::new();
            for character in characters {
                output.push_str(&escape_regex_literal(*character));
            }
            output
        }
        Expr::Class(set) => {
            let mut output = String::from("[");
            if set.negated {
                output.push('^');
            }
            for (lower, upper) in &set.ranges {
                output.push_str(&escape_regex_class(*lower));
                if lower != upper {
                    output.push('-');
                    output.push_str(&escape_regex_class(*upper));
                }
            }
            output.push(']');
            output
        }
        Expr::Reference(name) => {
            anyhow::ensure!(
                !stack.contains(name),
                "grammar rule '{name}' is recursive; this subset does not support recursion"
            );
            let referenced = rules
                .get(name)
                .ok_or_else(|| anyhow::anyhow!("grammar references undefined rule '{name}'"))?;
            stack.push(name.clone());
            let rendered = expression_pattern(referenced, rules, stack)?;
            stack.pop();
            format!("(?:{rendered})")
        }
        Expr::Sequence(items) => {
            let mut output = String::new();
            for item in items {
                output.push_str(&expression_pattern(item, rules, stack)?);
            }
            output
        }
        Expr::Alternate(options) => {
            let rendered = options
                .iter()
                .map(|option| expression_pattern(option, rules, stack))
                .collect::<anyhow::Result<Vec<_>>>()?;
            format!("(?:{})", rendered.join("|"))
        }
        Expr::Repeat { inner, min, max } => {
            anyhow::ensure!(
                *min <= REPEAT_LIMIT && max.unwrap_or(0) <= REPEAT_LIMIT,
                "grammar repetition bound exceeds {REPEAT_LIMIT}"
            );
            let rendered = expression_pattern(inner, rules, stack)?;
            let quantifier = match max {
                None if *min == 0 => "*".to_string(),
                None if *min == 1 => "+".to_string(),
                None => format!("{{{min},}}"),
                Some(max) if *min == 0 && *max == 1 => "?".to_string(),
                Some(max) if min == max => format!("{{{min}}}"),
                Some(max) => format!("{{{min},{max}}}"),
            };
            format!("(?:{rendered}){quantifier}")
        }
    })
}

fn escape_regex_literal(character: char) -> String {
    match character {
        '\n' => "\\n".to_string(),
        '\r' => "\\r".to_string(),
        '\t' => "\\t".to_string(),
        '\\' | '.' | '*' | '+' | '?' | '^' | '$' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
        | '/' => format!("\\{character}"),
        _ => character.to_string(),
    }
}

fn escape_regex_class(character: char) -> String {
    match character {
        '\n' => "\\n".to_string(),
        '\r' => "\\r".to_string(),
        '\t' => "\\t".to_string(),
        '\\' | ']' | '^' | '-' => format!("\\{character}"),
        _ => character.to_string(),
    }
}

// ── Token vocabulary trie ─────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct VocabTrieNode {
    children: Vec<(char, usize)>,
    token_ids: Vec<u32>,
}

/// Prefix-sharing index over the decoded token vocabulary.
///
/// A grammar state normally permits a very small part of a 150k-token
/// vocabulary. Walking only trie edges accepted by the NFA avoids advancing the
/// grammar independently over every token on every decode step.
#[derive(Debug, Default)]
pub struct VocabTrie {
    nodes: Vec<VocabTrieNode>,
}

impl VocabTrie {
    pub fn new(vocab: &[String]) -> Self {
        let mut trie = Self {
            nodes: vec![VocabTrieNode::default()],
        };
        for (token_id, text) in vocab.iter().enumerate() {
            if !text.is_empty() {
                trie.insert(text, token_id as u32);
            }
        }
        trie
    }

    fn insert(&mut self, text: &str, token_id: u32) {
        let mut node = 0;
        for character in text.chars() {
            let child = self.nodes[node]
                .children
                .iter()
                .find_map(|(candidate, child)| (*candidate == character).then_some(*child));
            node = match child {
                Some(child) => child,
                None => {
                    let child = self.nodes.len();
                    self.nodes.push(VocabTrieNode::default());
                    self.nodes[node].children.push((character, child));
                    child
                }
            };
        }
        self.nodes[node].token_ids.push(token_id);
    }

    /// Token ids whose complete decoded text is accepted from `state`.
    pub fn allowed_token_ids(&self, grammar: &Grammar, state: &GrammarState) -> Vec<u32> {
        if self.nodes.is_empty() {
            return Vec::new();
        }

        let mut allowed = Vec::new();
        let mut stack = vec![(0usize, state.clone())];
        while let Some((node, grammar_state)) = stack.pop() {
            for &(character, child) in &self.nodes[node].children {
                let Some(next_state) = grammar.advance_char(&grammar_state, character) else {
                    continue;
                };
                allowed.extend_from_slice(&self.nodes[child].token_ids);
                stack.push((child, next_state));
            }
        }
        allowed.sort_unstable();
        allowed
    }
}

// ── AST ───────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
enum Expr {
    Literal(Vec<char>),
    Class(CharSet),
    Reference(String),
    Sequence(Vec<Expr>),
    Alternate(Vec<Expr>),
    /// `min` repetitions required, then up to `max` more; `None` is unbounded.
    Repeat {
        inner: Box<Expr>,
        min: usize,
        max: Option<usize>,
    },
}

// ── NFA construction ──────────────────────────────────────────────────────────

#[derive(Default)]
struct NfaBuilder {
    states: Vec<Vec<Transition>>,
}

impl NfaBuilder {
    fn new_state(&mut self) -> usize {
        self.states.push(Vec::new());
        self.states.len() - 1
    }

    fn connect(&mut self, from: usize, transition: Transition) {
        self.states[from].push(transition);
    }

    fn compile(
        &mut self,
        expr: &Expr,
        rules: &HashMap<String, Expr>,
        stack: &mut Vec<String>,
    ) -> anyhow::Result<Fragment> {
        match expr {
            Expr::Literal(chars) => {
                let start = self.new_state();
                let mut current = start;
                for c in chars {
                    let next = self.new_state();
                    self.connect(
                        current,
                        Transition::Consume {
                            set: CharSet::single(*c),
                            target: next,
                        },
                    );
                    current = next;
                }
                Ok(Fragment {
                    start,
                    accept: current,
                })
            }
            Expr::Class(set) => {
                let start = self.new_state();
                let accept = self.new_state();
                self.connect(
                    start,
                    Transition::Consume {
                        set: set.clone(),
                        target: accept,
                    },
                );
                Ok(Fragment { start, accept })
            }
            Expr::Reference(name) => {
                anyhow::ensure!(
                    !stack.contains(name),
                    "grammar rule '{name}' is recursive; this subset does not support recursion"
                );
                let inner = rules
                    .get(name)
                    .ok_or_else(|| anyhow::anyhow!("grammar references undefined rule '{name}'"))?;
                stack.push(name.clone());
                let fragment = self.compile(inner, rules, stack)?;
                stack.pop();
                Ok(fragment)
            }
            Expr::Sequence(items) => {
                let start = self.new_state();
                let mut current = start;
                for item in items {
                    let fragment = self.compile(item, rules, stack)?;
                    self.connect(
                        current,
                        Transition::Epsilon {
                            target: fragment.start,
                        },
                    );
                    current = fragment.accept;
                }
                Ok(Fragment {
                    start,
                    accept: current,
                })
            }
            Expr::Alternate(options) => {
                let start = self.new_state();
                let accept = self.new_state();
                for option in options {
                    let fragment = self.compile(option, rules, stack)?;
                    self.connect(
                        start,
                        Transition::Epsilon {
                            target: fragment.start,
                        },
                    );
                    self.connect(fragment.accept, Transition::Epsilon { target: accept });
                }
                Ok(Fragment { start, accept })
            }
            Expr::Repeat { inner, min, max } => {
                self.compile_repeat(inner, *min, *max, rules, stack)
            }
        }
    }

    fn compile_repeat(
        &mut self,
        inner: &Expr,
        min: usize,
        max: Option<usize>,
        rules: &HashMap<String, Expr>,
        stack: &mut Vec<String>,
    ) -> anyhow::Result<Fragment> {
        anyhow::ensure!(
            min <= REPEAT_LIMIT && max.unwrap_or(0) <= REPEAT_LIMIT,
            "grammar repetition bound exceeds {REPEAT_LIMIT}"
        );

        let start = self.new_state();
        let accept = self.new_state();
        let mut current = start;

        // Mandatory repetitions, unrolled.
        for _ in 0..min {
            let fragment = self.compile(inner, rules, stack)?;
            self.connect(
                current,
                Transition::Epsilon {
                    target: fragment.start,
                },
            );
            current = fragment.accept;
        }

        match max {
            // Unbounded: loop back over one more copy.
            None => {
                let fragment = self.compile(inner, rules, stack)?;
                self.connect(
                    current,
                    Transition::Epsilon {
                        target: fragment.start,
                    },
                );
                self.connect(
                    fragment.accept,
                    Transition::Epsilon {
                        target: fragment.start,
                    },
                );
                self.connect(fragment.accept, Transition::Epsilon { target: accept });
                self.connect(current, Transition::Epsilon { target: accept });
            }
            // Bounded: unroll the optional tail, each copy able to exit.
            Some(max) => {
                anyhow::ensure!(
                    max >= min,
                    "grammar repetition maximum {max} is below minimum {min}"
                );
                self.connect(current, Transition::Epsilon { target: accept });
                for _ in min..max {
                    let fragment = self.compile(inner, rules, stack)?;
                    self.connect(
                        current,
                        Transition::Epsilon {
                            target: fragment.start,
                        },
                    );
                    current = fragment.accept;
                    self.connect(current, Transition::Epsilon { target: accept });
                }
            }
        }

        Ok(Fragment { start, accept })
    }
}

// ── Parsing ───────────────────────────────────────────────────────────────────

fn parse_rules(src: &str) -> anyhow::Result<Vec<(String, Expr)>> {
    let mut rules = Vec::new();
    let mut seen = HashSet::new();
    for line in src.lines() {
        let line = strip_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        let (name, body) = line
            .split_once("::=")
            .ok_or_else(|| anyhow::anyhow!("grammar line is not a rule: {line}"))?;
        let name = name.trim().to_string();
        anyhow::ensure!(!name.is_empty(), "grammar rule has an empty name");
        anyhow::ensure!(
            seen.insert(name.clone()),
            "grammar defines rule '{name}' twice"
        );
        let mut parser = Parser {
            chars: body.chars().collect(),
            position: 0,
        };
        let expr = parser.parse_alternation()?;
        parser.skip_whitespace();
        anyhow::ensure!(
            parser.position == parser.chars.len(),
            "unexpected trailing input in rule '{name}'"
        );
        rules.push((name, expr));
    }
    anyhow::ensure!(!rules.is_empty(), "grammar has no rules");
    Ok(rules)
}

fn strip_comment(line: &str) -> &str {
    match line.split_once('#') {
        Some((before, _)) => before,
        None => line,
    }
}

struct Parser {
    chars: Vec<char>,
    position: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.position).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.position += 1;
        Some(c)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.position += 1;
        }
    }

    fn parse_alternation(&mut self) -> anyhow::Result<Expr> {
        let mut options = vec![self.parse_sequence()?];
        loop {
            self.skip_whitespace();
            if self.peek() == Some('|') {
                self.position += 1;
                options.push(self.parse_sequence()?);
            } else {
                break;
            }
        }
        Ok(if options.len() == 1 {
            options.pop().expect("checked length")
        } else {
            Expr::Alternate(options)
        })
    }

    fn parse_sequence(&mut self) -> anyhow::Result<Expr> {
        let mut items = Vec::new();
        loop {
            self.skip_whitespace();
            match self.peek() {
                None | Some('|') | Some(')') => break,
                _ => items.push(self.parse_term()?),
            }
        }
        anyhow::ensure!(!items.is_empty(), "empty grammar sequence");
        Ok(if items.len() == 1 {
            items.pop().expect("checked length")
        } else {
            Expr::Sequence(items)
        })
    }

    fn parse_term(&mut self) -> anyhow::Result<Expr> {
        let atom = self.parse_atom()?;
        match self.peek() {
            Some('*') => {
                self.position += 1;
                Ok(Expr::Repeat {
                    inner: Box::new(atom),
                    min: 0,
                    max: None,
                })
            }
            Some('+') => {
                self.position += 1;
                Ok(Expr::Repeat {
                    inner: Box::new(atom),
                    min: 1,
                    max: None,
                })
            }
            Some('?') => {
                self.position += 1;
                Ok(Expr::Repeat {
                    inner: Box::new(atom),
                    min: 0,
                    max: Some(1),
                })
            }
            Some('{') => {
                self.position += 1;
                let (min, max) = self.parse_repeat_bounds()?;
                Ok(Expr::Repeat {
                    inner: Box::new(atom),
                    min,
                    max,
                })
            }
            _ => Ok(atom),
        }
    }

    fn parse_repeat_bounds(&mut self) -> anyhow::Result<(usize, Option<usize>)> {
        let min = self.parse_number()?;
        match self.bump() {
            Some('}') => Ok((min, Some(min))),
            Some(',') => {
                if self.peek() == Some('}') {
                    self.position += 1;
                    return Ok((min, None));
                }
                let max = self.parse_number()?;
                anyhow::ensure!(
                    self.bump() == Some('}'),
                    "unterminated repetition bound in grammar"
                );
                Ok((min, Some(max)))
            }
            other => anyhow::bail!("unexpected {other:?} in grammar repetition bound"),
        }
    }

    fn parse_number(&mut self) -> anyhow::Result<usize> {
        let start = self.position;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.position += 1;
        }
        anyhow::ensure!(
            self.position > start,
            "expected a number in grammar repetition bound"
        );
        let digits: String = self.chars[start..self.position].iter().collect();
        Ok(digits.parse()?)
    }

    fn parse_atom(&mut self) -> anyhow::Result<Expr> {
        self.skip_whitespace();
        match self.peek() {
            Some('"') => self.parse_literal(),
            Some('[') => self.parse_class(),
            Some('(') => {
                self.position += 1;
                let inner = self.parse_alternation()?;
                self.skip_whitespace();
                anyhow::ensure!(self.bump() == Some(')'), "unbalanced '(' in grammar");
                Ok(inner)
            }
            Some(c) if c.is_alphanumeric() || c == '_' || c == '-' => {
                let start = self.position;
                while matches!(self.peek(), Some(c) if c.is_alphanumeric() || c == '_' || c == '-')
                {
                    self.position += 1;
                }
                Ok(Expr::Reference(
                    self.chars[start..self.position].iter().collect(),
                ))
            }
            other => anyhow::bail!("unexpected {other:?} in grammar"),
        }
    }

    fn parse_literal(&mut self) -> anyhow::Result<Expr> {
        anyhow::ensure!(self.bump() == Some('"'), "expected a quoted literal");
        let mut chars = Vec::new();
        loop {
            match self.bump() {
                Some('"') => break,
                Some('\\') => chars
                    .push(unescape(self.bump().ok_or_else(|| {
                        anyhow::anyhow!("trailing escape in grammar literal")
                    })?)),
                Some(c) => chars.push(c),
                None => anyhow::bail!("unterminated grammar literal"),
            }
        }
        anyhow::ensure!(!chars.is_empty(), "empty grammar literal");
        Ok(Expr::Literal(chars))
    }

    fn parse_class(&mut self) -> anyhow::Result<Expr> {
        anyhow::ensure!(self.bump() == Some('['), "expected a character class");
        let negated = if self.peek() == Some('^') {
            self.position += 1;
            true
        } else {
            false
        };
        let mut ranges = Vec::new();
        loop {
            let lo = match self.bump() {
                Some(']') => break,
                Some('\\') => unescape(
                    self.bump()
                        .ok_or_else(|| anyhow::anyhow!("trailing escape in character class"))?,
                ),
                Some(c) => c,
                None => anyhow::bail!("unterminated character class"),
            };
            if self.peek() == Some('-') && self.chars.get(self.position + 1) != Some(&']') {
                self.position += 1;
                let hi = match self.bump() {
                    Some('\\') => unescape(
                        self.bump()
                            .ok_or_else(|| anyhow::anyhow!("trailing escape in character class"))?,
                    ),
                    Some(c) => c,
                    None => anyhow::bail!("unterminated character range"),
                };
                anyhow::ensure!(lo <= hi, "inverted character range '{lo}-{hi}'");
                ranges.push((lo, hi));
            } else {
                ranges.push((lo, lo));
            }
        }
        anyhow::ensure!(!ranges.is_empty(), "empty character class");
        Ok(Expr::Class(CharSet { ranges, negated }))
    }
}

fn unescape(c: char) -> char {
    match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::tasks::cluster_label::LABEL_GRAMMAR;

    fn accepts(grammar: &Grammar, text: &str) -> bool {
        grammar
            .advance(&grammar.initial_state(), text)
            .map(|state| grammar.is_complete(&state))
            .unwrap_or(false)
    }

    #[test]
    fn label_grammar_accepts_two_to_twelve_words_with_fixed_frame() {
        let grammar = Grammar::parse(LABEL_GRAMMAR).unwrap();
        assert!(accepts(&grammar, "Topic: Cache invalidation"));
        assert!(accepts(&grammar, "Topic: Cache invalidation and staleness"));
        assert!(accepts(
            &grammar,
            "Topic: One two three four five six seven eight nine ten eleven twelve"
        ));
    }

    #[test]
    fn label_grammar_rejects_a_missing_frame_single_word_and_thirteen_words() {
        let grammar = Grammar::parse(LABEL_GRAMMAR).unwrap();
        assert!(!accepts(&grammar, "Cache invalidation"));
        assert!(!accepts(&grammar, "Topic: Cache"));
        assert!(!accepts(
            &grammar,
            "Topic: One two three four five six seven eight nine ten eleven twelve thirteen"
        ));
    }

    #[test]
    fn label_grammar_rejects_the_observed_bulleted_list_output() {
        let grammar = Grammar::parse(LABEL_GRAMMAR).unwrap();
        // This is what the real model produced for the label prompt (spec §14).
        let observed = "- Cache invalidation\n- Stale reads\n- TTL policy";
        assert!(!accepts(&grammar, observed));
        // It cannot even be started: the fixed frame is the only legal prefix.
        assert!(grammar.advance(&grammar.initial_state(), "-").is_none());
    }

    #[test]
    fn json_schema_pattern_preserves_the_label_language() {
        let pattern = json_schema_pattern(LABEL_GRAMMAR).unwrap();
        let regex = regex::Regex::new(&pattern).unwrap();
        assert!(regex.is_match("Topic: Cache invalidation"));
        assert!(regex.is_match("Topic: Cache invalidation and staleness\n"));
        assert!(!regex.is_match("Cache invalidation"));
        assert!(!regex.is_match("Topic: Cache"));
    }

    #[test]
    fn masking_forbids_every_token_outside_the_language() {
        let grammar = Grammar::parse(LABEL_GRAMMAR).unwrap();
        let vocab: Vec<String> = ["Topic: ", "Cache", "-", "\n", "*", " ", "9", ""]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let allowed = grammar.allowed_next(&grammar.initial_state(), &vocab);
        assert_eq!(
            allowed,
            vec![true, false, false, false, false, false, false, false]
        );
    }

    #[test]
    fn vocabulary_trie_matches_linear_masking_at_successive_states() {
        let grammar = Grammar::parse(LABEL_GRAMMAR).unwrap();
        let vocab: Vec<String> = [
            "Cache",
            " invalidation",
            " policy",
            " ",
            "9",
            "-",
            "\n",
            "é",
            "",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let trie = VocabTrie::new(&vocab);

        for prefix in ["", "Topic: ", "Topic: Cache invalidation"] {
            let state = grammar
                .advance(&grammar.initial_state(), prefix)
                .unwrap_or_else(|| grammar.initial_state());
            let linear: Vec<u32> = grammar
                .allowed_next(&state, &vocab)
                .iter()
                .enumerate()
                .filter_map(|(id, allowed)| allowed.then_some(id as u32))
                .collect();
            assert_eq!(trie.allowed_token_ids(&grammar, &state), linear);
        }
    }

    #[test]
    fn one_of_can_only_produce_one_of_its_inputs() {
        let options = vec!["rust".to_string(), "ruby".to_string(), "python".to_string()];
        let grammar = Grammar::one_of(&options).unwrap();

        for option in &options {
            assert!(accepts(&grammar, option), "{option} must be accepted");
        }
        assert!(!accepts(&grammar, "rus"));
        assert!(!accepts(&grammar, "rustacean"));
        assert!(!accepts(&grammar, "go"));
    }

    #[test]
    fn one_of_masks_to_shared_prefixes_only() {
        let grammar = Grammar::one_of(&["rust".to_string(), "ruby".to_string()]).unwrap();
        let state = grammar.advance(&grammar.initial_state(), "ru").unwrap();
        let vocab: Vec<String> = ["s", "b", "x"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            grammar.allowed_next(&state, &vocab),
            vec![true, true, false]
        );
    }

    #[test]
    fn is_complete_is_false_mid_word() {
        let grammar = Grammar::parse(LABEL_GRAMMAR).unwrap();
        let state = grammar
            .advance(&grammar.initial_state(), "Topic: Cach")
            .unwrap();
        assert!(!grammar.is_complete(&state));
    }

    #[test]
    fn recursion_is_rejected_rather_than_hanging() {
        let err = Grammar::parse("a ::= \"x\" a").unwrap_err();
        assert!(err.to_string().contains("recursive"), "{err}");
    }

    #[test]
    fn undefined_rule_references_are_rejected() {
        let err = Grammar::parse("a ::= b").unwrap_err();
        assert!(err.to_string().contains("undefined rule"), "{err}");
    }

    #[test]
    fn supports_alternation_grouping_and_optionals() {
        let grammar = Grammar::parse(r#"root ::= ("cat" | "dog") "s"?"#).unwrap();
        assert!(accepts(&grammar, "cat"));
        assert!(accepts(&grammar, "dogs"));
        assert!(!accepts(&grammar, "cow"));
        assert!(!accepts(&grammar, "catss"));
    }

    #[test]
    fn supports_negated_character_classes() {
        let grammar = Grammar::parse(r#"root ::= [^\n]+"#).unwrap();
        assert!(accepts(&grammar, "any text at all"));
        assert!(!accepts(&grammar, "line\nbreak"));
    }

    #[test]
    fn exact_repetition_bound() {
        let grammar = Grammar::parse(r#"root ::= "ab"{3}"#).unwrap();
        assert!(accepts(&grammar, "ababab"));
        assert!(!accepts(&grammar, "abab"));
        assert!(!accepts(&grammar, "abababab"));
    }

    #[test]
    fn finite_repetition_limit_covers_summary_sentences_without_unbounded_nfas() {
        Grammar::parse(&format!(r#"root ::= "x"{{1,{REPEAT_LIMIT}}}"#)).unwrap();
        assert!(Grammar::parse(&format!(r#"root ::= "x"{{1,{}}}"#, REPEAT_LIMIT + 1)).is_err());
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let grammar = Grammar::parse("# leading comment\n\nroot ::= \"ok\" # trailing\n").unwrap();
        assert!(accepts(&grammar, "ok"));
    }

    #[test]
    fn malformed_grammars_report_rather_than_panic() {
        for src in ["root = \"x\"", "root ::= \"unterminated", "root ::= [a", ""] {
            assert!(Grammar::parse(src).is_err(), "{src:?} should not parse");
        }
    }

    #[test]
    fn one_of_rejects_an_empty_option_set() {
        assert!(Grammar::one_of(&[]).is_err());
    }
}
