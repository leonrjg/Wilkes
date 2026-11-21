//! Reading order: which documents of a library build on which others, read
//! off the words on their pages. No model, no embeddings, no network.
//!
//! **The signal.** A book that builds on another names its subject, and the
//! reverse rarely happens: a calculus text says "algebra" throughout, an
//! algebra text almost never says "calculus". So each document is given the
//! subjects its title, file name and folders name it by, and a pair of
//! documents is ordered by which of the two names the other's subject.
//!
//! **What was measured, and what the rules below come from.** On three folders
//! of a real library (Math, Statistics, AI: 44 textbooks and course books)
//! this ordering came out close to the curriculum's: arithmetic, algebra,
//! calculus, linear algebra, then the applied text that uses them all. Three
//! alternatives were tried on the same folders and are deliberately absent:
//!
//! * *Which document uses the other's distinctive vocabulary.* It tracks how
//!   long a book is: an arithmetic primer landed near the end.
//! * *Every word of a title as a subject.* "Mathematics", "model" and "data"
//!   became subjects and calculus sorted before algebra. Subjects are whole
//!   phrases, and a single word is not one when the library shows it to be
//!   ordinary ([`ORDINARY_WORD_SHARE`], [`DISTINCTIVE_FACTOR`]).
//! * *Placing a document just before the first one that needs it.* With a
//!   dozen books the pairs are too sparse for that: the arithmetic primer
//!   fell below the algebra books that happen not to name it. Documents are
//!   ordered by their total over all pairs instead, which no single missing
//!   pair can move far.
//!
//! **The rules for reading a name in text** are the ones the eos project
//! arrived at for the same question: a use is a name appearing in two
//! passages or more, never one mention ([`USE_MIN_PASSAGES`]); a plural is the
//! same name ([`fold_word`]); and a pair that names each other about equally
//! says nothing about which comes first ([`BOTH_WAYS_RATIO`]).
//!
//! **What it cannot do.** Matching is literal, so "ML" is not "machine
//! learning". The level words are English. A document whose title and folders
//! name nothing its own text uses has no subject, and is ordered only by the
//! subjects it names; one with neither is reported as unplaced rather than
//! given a position.

use std::collections::{BTreeMap, HashMap, HashSet};

/// Characters per passage. About a page of a book: small enough that a
/// passing mention occupies one passage, large enough that a book's own
/// subject reaches most of them.
pub const PASSAGE_CHARS: usize = 2_000;

/// Passages of its own text a document must use a phrase in before the phrase
/// is one of its subjects. This is what discards a title the text never
/// repeats, such as the producer string some PDFs carry as their title.
pub const OWN_MIN_PASSAGES: u32 = 3;

/// The share of its own passages a document must use a single word in before
/// the word is one of its subjects. A phrase is a name wherever it appears; a
/// word is a subject only of a text that keeps saying it. On the library the
/// other thresholds were measured on, the words a title holds without being
/// about them ("everyone", an author's surname) sat at 1-2% and its one-word
/// subjects at 13% and above, with "precalculus" at 1% the one real subject
/// this gives up.
pub const OWN_WORD_SHARE: f32 = 0.05;

/// Passages a document must use another's subject in before that counts.
pub const USE_MIN_PASSAGES: u32 = 2;

/// The most one subject can count for in a pair: the share of a document's
/// passages that name it, capped here. Past it the document is plainly built
/// on that subject, and more mentions say no more.
pub const MENTION_CAP: f32 = 0.15;

/// Two documents that name each other's subjects decide nothing unless one
/// does so at most this fraction as much as the other.
pub const BOTH_WAYS_RATIO: f32 = 1.0 / 3.0;

/// A one-word subject used by more than this share of the library's documents
/// is ordinary vocabulary ("data", "model", "effect") and no document is
/// credited for another naming it. Phrases of two words or more are names
/// however common they are.
///
/// Measured on a 94-document library: the words a title uses without naming
/// a subject by them sit at 65% and above ("method" 98%, "data" 89%, "effect"
/// 81%, "mathematics" 65%), the one-word subjects at 47% and below
/// ("statistics", "arithmetic", "algebra", "calculus").
pub const ORDINARY_WORD_SHARE: f32 = 0.5;

/// Fewest documents with text for [`ORDINARY_WORD_SHARE`] to be read at all.
/// In a handful of books on one topic most of them use the topic's own words,
/// and counting cannot tell those from ordinary ones.
pub const ORDINARY_MIN_LIBRARY: usize = 20;

/// A one-word subject is a document's own only when the document uses it in
/// at least this many times the share of passages the rest of the library
/// does on average. A folder called "Papers" makes "paper" a candidate
/// subject of everything in it, and every paper anywhere says "paper" as
/// often: that is a word, not a subject. This holds at any library size,
/// which the share above does not.
pub const DISTINCTIVE_FACTOR: f32 = 3.0;

/// The longest subject, in words.
pub const MAX_SUBJECT_WORDS: usize = 4;

/// Closed-class words. A subject neither starts nor ends with one, and one
/// alone is never a subject.
const FUNCTION_WORDS: &[&str] = &[
    "a", "about", "above", "after", "against", "among", "an", "and", "are", "as", "at", "be",
    "before", "below", "between", "but", "by", "can", "could", "did", "do", "does", "down",
    "during", "for", "from", "had", "has", "have", "he", "her", "here", "his", "how", "if", "in",
    "into", "is", "it", "its", "may", "me", "might", "must", "my", "no", "nor", "not", "of", "off",
    "on", "onto", "or", "our", "out", "over", "shall", "she", "should", "so", "such", "than",
    "that", "the", "their", "them", "then", "there", "these", "they", "this", "those", "through",
    "to", "under", "up", "upon", "us", "was", "we", "were", "what", "when", "where", "which",
    "who", "why", "will", "with", "would", "you", "your",
];

/// Words at which a title moves from one subject to the next: "Statistics for
/// Everyone", "Probability and Descriptive Statistics".
const CONNECTORS: &[&str] = &["and", "for", "in", "or", "with"];

/// Words that say a document is a way in to its subject, in the folded form
/// [`fold_word`] gives. They frame a subject and are not part of its name.
const INTRODUCTORY: &[&str] = &[
    "basic",
    "beginner",
    "element",
    "elementary",
    "essential",
    "foundation",
    "fundamental",
    "intro",
    "introduction",
    "introductory",
    "primer",
    "principle",
    "school",
];

/// Words that say a document assumes its subject's basics.
const ADVANCED: &[&str] = &["advanced", "graduate"];

/// Words that say how a subject is treated, not which subject or at what
/// level: "Applied Statistics" is a book on statistics.
const TREATMENT: &[&str] = &["applied", "general", "modern", "practical"];

/// Words that describe the book or the file, not what it is about.
const BIBLIOGRAPHIC: &[&str] = &[
    "book", "edition", "epub", "part", "pdf", "vol", "volume", "web",
];

/// Folds a lowercase word so that a plural is the same word as its singular.
///
/// Three rules and no stemmer, the rules eos settled on for the same purpose:
/// a name must match its own plural and nothing else, and `bias`, `analysis`
/// and `loss` are singular already.
pub fn fold_word(word: &mut String) {
    let count = word.chars().count();
    if count > 4 && word.ends_with("ies") {
        for _ in 0..3 {
            word.pop();
        }
        word.push('y');
    } else if count > 4 && word.ends_with("sses") {
        word.pop();
        word.pop();
    } else if count > 3 && word.ends_with('s') {
        let before = word.chars().rev().nth(1);
        if !matches!(before, Some('a' | 'i' | 'o' | 'u' | 's')) {
            word.pop();
        }
    }
}

/// One word of a label: as written (lowercased) and as matched.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LabelWord {
    shown: String,
    key: String,
}

/// A subject: the name to show and the folded words it is matched by.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Subject {
    pub name: String,
    pub key: String,
}

/// What may name a document's subject.
#[derive(Clone, Debug, Default)]
pub struct Labels {
    /// The title its metadata gives, when it gives one.
    pub title: Option<String>,
    /// The file's name without its extension.
    pub file_stem: String,
    /// The folders it sits in below its library root, outermost first. The
    /// root's own name is left out: every document under it would share it.
    pub folders: Vec<String>,
}

/// A document as its labels describe it, before any text is read.
#[derive(Clone, Debug, Default)]
pub struct Described {
    /// One per phrase of a label. A document takes at most one subject from
    /// each: the longest stretch of it that its own text uses.
    segments: Vec<Vec<LabelWord>>,
    /// -1 for a way in, +1 for a text that assumes the basics, 0 otherwise.
    pub level: i8,
    /// "Volume 2", "Part 3", a trailing "II".
    pub volume: Option<u32>,
}

impl Described {
    pub fn from_labels(labels: &Labels) -> Self {
        let stem = tidy_file_stem(&labels.file_stem);
        let mut texts: Vec<&str> = Vec::new();
        if let Some(title) = labels.title.as_deref() {
            texts.push(title);
        }
        texts.push(&stem);
        let volume = texts.iter().find_map(|text| volume_of(text));
        texts.extend(labels.folders.iter().map(String::as_str));

        let mut segments = Vec::new();
        let (mut introductory, mut advanced) = (false, false);
        for text in texts {
            for word in label_words(text).into_iter().flatten() {
                introductory |= INTRODUCTORY.contains(&word.key.as_str());
                advanced |= ADVANCED.contains(&word.key.as_str());
            }
            segments.extend(segments_of(text));
        }
        Self {
            segments,
            level: i8::from(advanced) - i8::from(introductory),
            volume,
        }
    }

    /// Every phrase that could turn out to be one of this document's
    /// subjects: what a reading of any text must count.
    pub fn candidates(&self) -> impl Iterator<Item = String> + '_ {
        self.segments
            .iter()
            .flat_map(|segment| stretches(segment))
            .map(key_of)
    }

    /// The subjects a reading of the document's own text confirms: from each
    /// label phrase, the longest stretch the text uses.
    fn subjects(&self, reading: &Reading) -> Vec<Subject> {
        let mut subjects: Vec<Subject> = Vec::new();
        for segment in &self.segments {
            let confirmed = stretches(segment).into_iter().find(|stretch| {
                let key = key_of(stretch);
                reading.uses(&key) >= OWN_MIN_PASSAGES
                    && (stretch.len() > 1 || reading.share(&key) >= OWN_WORD_SHARE)
            });
            let Some(stretch) = confirmed else { continue };
            let subject = Subject {
                name: stretch
                    .iter()
                    .map(|word| word.shown.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                key: key_of(stretch),
            };
            if !subjects.iter().any(|known| known.key == subject.key) {
                subjects.push(subject);
            }
        }
        subjects
    }
}

fn key_of(words: &[LabelWord]) -> String {
    words
        .iter()
        .map(|word| word.key.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A file name as a label: without what a download site appended after
/// " -- ", without parenthesised asides, and with the separators a file name
/// uses where a title would use spaces.
fn tidy_file_stem(stem: &str) -> String {
    let stem = stem.split(" -- ").next().unwrap_or_default();
    // No spaces at all: the hyphens and underscores are the spaces.
    let unspaced = !stem.trim().contains(char::is_whitespace);
    let mut out = String::new();
    let mut depth = 0usize;
    let mut previous: Option<char> = None;
    for c in stem.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            '-' | '_' if unspaced => out.push(' '),
            _ => {
                // camelCase is two words.
                if c.is_uppercase() && previous.is_some_and(char::is_lowercase) {
                    out.push(' ');
                }
                out.push(c);
            }
        }
        previous = Some(c);
    }
    out
}

/// The words of a label, in runs broken wherever the label itself breaks: at
/// punctuation. Lowercased, and folded for matching.
fn label_words(label: &str) -> Vec<Vec<LabelWord>> {
    let mut runs: Vec<Vec<LabelWord>> = vec![Vec::new()];
    let mut word = String::new();
    let end_word = |word: &mut String, runs: &mut Vec<Vec<LabelWord>>| {
        if word.is_empty() {
            return;
        }
        let shown = std::mem::take(word);
        let mut key = shown.clone();
        fold_word(&mut key);
        runs.last_mut()
            .expect("runs is never empty")
            .push(LabelWord { shown, key });
    };
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_alphanumeric() {
            word.extend(c.to_lowercase());
            continue;
        }
        // "Multi-Agent" is one name of two words; " - " separates two names.
        let compound =
            c == '-' && !word.is_empty() && chars.peek().is_some_and(|next| next.is_alphanumeric());
        end_word(&mut word, &mut runs);
        if !(compound || c.is_whitespace() || c == '_' || c == '\'' || c == '’') {
            runs.push(Vec::new());
        }
    }
    end_word(&mut word, &mut runs);
    runs.retain(|run| !run.is_empty());
    runs
}

/// The phrases of a label that may each name a subject: its runs, broken
/// again at connectors, without the words that frame a subject rather than
/// name it, and without function words at either end.
fn segments_of(label: &str) -> Vec<Vec<LabelWord>> {
    let mut segments = Vec::new();
    for run in label_words(label) {
        let mut current: Vec<LabelWord> = Vec::new();
        let mut flush = |current: &mut Vec<LabelWord>| {
            let mut words = std::mem::take(current);
            while words
                .last()
                .is_some_and(|word| FUNCTION_WORDS.contains(&word.key.as_str()))
            {
                words.pop();
            }
            let leading = words
                .iter()
                .take_while(|word| FUNCTION_WORDS.contains(&word.key.as_str()))
                .count();
            words.drain(..leading);
            if !words.is_empty() {
                segments.push(words);
            }
        };
        for word in run {
            let key = word.key.as_str();
            if CONNECTORS.contains(&key) {
                flush(&mut current);
            } else if !(INTRODUCTORY.contains(&key)
                || ADVANCED.contains(&key)
                || TREATMENT.contains(&key)
                || BIBLIOGRAPHIC.contains(&key)
                || key.chars().any(|c| c.is_numeric())
                || is_roman_numeral(key))
            {
                current.push(word);
            }
        }
        flush(&mut current);
    }
    segments
}

fn is_roman_numeral(word: &str) -> bool {
    matches!(word, "i" | "ii" | "iii" | "iv" | "v" | "vi")
}

/// The stretches of a segment that could be a subject, longest first, and at
/// equal length the one that opens the segment before the one that closes it:
/// "statistics open" names its subject first, "data science" second in
/// "of data science".
fn stretches(segment: &[LabelWord]) -> Vec<&[LabelWord]> {
    let mut out: Vec<&[LabelWord]> = Vec::new();
    let longest = segment.len().min(MAX_SUBJECT_WORDS);
    for length in (1..=longest).rev() {
        let opening = &segment[..length];
        let closing = &segment[segment.len() - length..];
        for stretch in [opening, closing] {
            if is_nameable(stretch) && !out.iter().any(|known| std::ptr::eq(*known, stretch)) {
                out.push(stretch);
            }
        }
    }
    out
}

/// Whether a stretch can stand for a subject: it neither opens nor closes on
/// a function word, and a single word is at least two characters.
fn is_nameable(stretch: &[LabelWord]) -> bool {
    let (Some(first), Some(last)) = (stretch.first(), stretch.last()) else {
        return false;
    };
    if FUNCTION_WORDS.contains(&first.key.as_str()) || FUNCTION_WORDS.contains(&last.key.as_str()) {
        return false;
    }
    stretch.len() > 1 || first.key.chars().count() >= 2
}

/// "Volume 2", "Vol. 3", "Part 1", or a trailing "2" or "II".
fn volume_of(label: &str) -> Option<u32> {
    let tokens: Vec<&str> = label
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    for pair in tokens.windows(2) {
        let marker = pair[0].to_lowercase();
        if matches!(marker.as_str(), "volume" | "vol" | "part") {
            if let Some(number) = number_of(pair[1]) {
                return Some(number);
            }
        }
    }
    let last = tokens.last()?;
    if last.chars().count() == 1 || last.chars().all(|c| matches!(c, 'I' | 'V')) {
        return number_of(last);
    }
    None
}

fn number_of(token: &str) -> Option<u32> {
    if let Ok(number) = token.parse::<u32>() {
        return (1..=99).contains(&number).then_some(number);
    }
    match token {
        "I" => Some(1),
        "II" => Some(2),
        "III" => Some(3),
        "IV" => Some(4),
        "V" => Some(5),
        "VI" => Some(6),
        _ => None,
    }
}

/// The phrases a text is read for.
#[derive(Debug, Default)]
pub struct Lexicon {
    phrases: Vec<(String, Vec<String>)>,
    /// Last word → the phrases that end on it.
    by_last: HashMap<String, Vec<usize>>,
}

impl Lexicon {
    /// `keys` are phrases of folded words, as [`Described::candidates`] and
    /// [`theme_key`] give them.
    pub fn new(keys: impl IntoIterator<Item = String>) -> Self {
        let mut lexicon = Self::default();
        let mut seen = HashSet::new();
        for key in keys {
            let words: Vec<String> = key.split(' ').map(str::to_string).collect();
            let Some(last) = words.last().cloned() else {
                continue;
            };
            if key.is_empty() || words.len() > MAX_SUBJECT_WORDS || !seen.insert(key.clone()) {
                continue;
            }
            lexicon
                .by_last
                .entry(last)
                .or_default()
                .push(lexicon.phrases.len());
            lexicon.phrases.push((key, words));
        }
        lexicon
    }

    pub fn len(&self) -> usize {
        self.phrases.len()
    }

    pub fn is_empty(&self) -> bool {
        self.phrases.is_empty()
    }
}

/// How one document's text uses the phrases of a lexicon.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    /// Passages of [`PASSAGE_CHARS`] the text spans.
    pub passages: u32,
    /// Phrase → the passages it appears in.
    uses: HashMap<String, u32>,
}

impl Reading {
    /// Passages the phrase appears in.
    pub fn uses(&self, key: &str) -> u32 {
        self.uses.get(key).copied().unwrap_or(0)
    }

    /// The share of the text's passages the phrase appears in.
    pub fn share(&self, key: &str) -> f32 {
        if self.passages == 0 {
            return 0.0;
        }
        self.uses(key) as f32 / self.passages as f32
    }
}

/// Reads a text for every phrase of the lexicon in one pass.
pub fn read_text(text: &str, lexicon: &Lexicon) -> Reading {
    // (passages the phrase was seen in, last such passage + 1)
    let mut seen: Vec<(u32, u32)> = vec![(0, 0); lexicon.phrases.len()];
    // The last few words, oldest first, so a phrase is matched where it ends.
    let mut recent: Vec<String> = Vec::with_capacity(MAX_SUBJECT_WORDS);
    let mut word = String::new();
    let mut characters = 0usize;
    let mut any_word = false;

    let mut end_word = |word: &mut String, recent: &mut Vec<String>, characters: usize| {
        if word.is_empty() {
            return;
        }
        fold_word(word);
        if recent.len() == MAX_SUBJECT_WORDS {
            let mut reused = recent.remove(0);
            reused.clear();
            reused.push_str(word);
            recent.push(reused);
        } else {
            recent.push(word.clone());
        }
        word.clear();
        let Some(ending) = lexicon.by_last.get(recent.last().expect("just pushed")) else {
            return;
        };
        let passage = (characters / PASSAGE_CHARS) as u32 + 1;
        for &index in ending {
            let words = &lexicon.phrases[index].1;
            if words.len() <= recent.len()
                && recent[recent.len() - words.len()..] == words[..]
                && seen[index].1 != passage
            {
                seen[index] = (seen[index].0 + 1, passage);
            }
        }
    };

    for c in text.chars() {
        if c.is_alphanumeric() {
            word.extend(c.to_lowercase());
            any_word = true;
        } else {
            end_word(&mut word, &mut recent, characters);
            // A sentence does not carry a name across its end.
            if matches!(c, '.' | '!' | '?' | ';' | ':' | '\n') {
                recent.clear();
            }
        }
        characters += 1;
    }
    end_word(&mut word, &mut recent, characters);

    Reading {
        passages: if any_word {
            characters.div_ceil(PASSAGE_CHARS) as u32
        } else {
            0
        },
        uses: seen
            .into_iter()
            .enumerate()
            .filter(|(_, (count, _))| *count > 0)
            .map(|(index, (count, _))| (lexicon.phrases[index].0.clone(), count))
            .collect(),
    }
}

/// A theme as typed, as the phrase a text is read for. `None` when it holds
/// no word, or more than a subject may.
pub fn theme_key(theme: &str) -> Option<String> {
    let words: Vec<LabelWord> = label_words(theme).into_iter().flatten().collect();
    if words.is_empty() || words.len() > MAX_SUBJECT_WORDS {
        return None;
    }
    Some(key_of(&words))
}

/// One document of the library, as [`order`] takes it.
#[derive(Clone, Copy, Debug)]
pub struct Entry<'a> {
    pub described: &'a Described,
    /// Its text read against a lexicon holding every entry's candidates, or
    /// `None` when there is no text to read.
    pub reading: Option<&'a Reading>,
    /// Whether it is one of the documents being ordered. The rest of the
    /// library is there to say which words are ordinary.
    pub in_scope: bool,
    /// "YYYY-MM"; between documents nothing else separates, the earlier first.
    pub publication: Option<&'a str>,
    /// What the last tie is broken by.
    pub name: &'a str,
}

/// Where a document stands among the ones it was ordered with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Others build on it and it builds on none of them.
    Foundation,
    /// It builds on some and others build on it.
    Bridge,
    /// It builds on others and none builds on it.
    Specialised,
    /// It has a subject, and no pair with any other document was decided.
    Standalone,
    /// Its labels name nothing its text uses, and it names no other's subject.
    NoSubject,
    /// There is no text to read.
    NoText,
}

impl Placement {
    pub fn is_ordered(self) -> bool {
        matches!(self, Self::Foundation | Self::Bridge | Self::Specialised)
    }
}

/// Why one document comes after another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    /// It names these subjects of the other.
    Names(Vec<Subject>),
    /// Same subject, and its labels put it at a higher level.
    Level,
    /// Same subject, and it is a later volume.
    Volume,
    /// Its subject is a narrower case of the other's.
    Narrower,
    /// It cites the other.
    Cites,
}

/// One document a document comes after.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// Index into the entries given.
    pub document: usize,
    pub reasons: Vec<Reason>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub placement: Placement,
    pub subjects: Vec<Subject>,
    /// The documents it builds on, earliest in the order first.
    pub after: Vec<Link>,
    /// How many documents build on it.
    pub needed_by: usize,
    /// Its total over all pairs: positive when others build on it.
    pub score: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    /// One per entry given, in the same order. Entries out of scope are
    /// [`Placement::NoText`] placeholders and appear in no sequence.
    pub documents: Vec<Placed>,
    /// The entries in scope, by index: first to last, then the standalone
    /// ones, then the unplaced ones.
    pub sequence: Vec<usize>,
    /// The subjects of the documents in scope, with how many of those
    /// documents have or name each, most first.
    pub subjects: Vec<(Subject, usize)>,
}

/// Orders the entries in scope. `cites` holds `(citing, cited)` pairs of
/// entry indices.
pub fn order(entries: &[Entry<'_>], cites: &[(usize, usize)]) -> Outcome {
    let subjects: Vec<Vec<Subject>> = entries
        .iter()
        .map(|entry| match entry.reading {
            Some(reading) => entry.described.subjects(reading),
            None => Vec::new(),
        })
        .collect();
    let credited = credited_subjects(entries, &subjects);
    let scope: Vec<usize> = (0..entries.len())
        .filter(|&index| entries[index].in_scope)
        .collect();

    // Every pair of documents that anything was said about, keyed lower index
    // first. Ordered, so totals are summed in the same order on every run.
    let mut votes: BTreeMap<(usize, usize), PairVotes> = BTreeMap::new();
    let mut vote = |first: usize, later: usize, amount: f32, reason: Reason| {
        let slot = votes
            .entry((first.min(later), first.max(later)))
            .or_default();
        if first < later {
            slot.amount += amount;
            slot.lower_first.push(reason);
        } else {
            slot.amount -= amount;
            slot.higher_first.push(reason);
        }
    };

    for (position, &a) in scope.iter().enumerate() {
        let Some(reading_a) = entries[a].reading else {
            continue;
        };
        for &b in &scope[position + 1..] {
            let Some(reading_b) = entries[b].reading else {
                continue;
            };
            // b names a's subjects, and a names b's.
            let (b_on_a, named_by_b) = names(reading_b, &subjects[b], &subjects[a], &credited[a]);
            let (a_on_b, named_by_a) = names(reading_a, &subjects[a], &subjects[b], &credited[b]);
            let both_ways = b_on_a > 0.0 && a_on_b > 0.0;
            let lopsided = b_on_a.min(a_on_b) <= BOTH_WAYS_RATIO * b_on_a.max(a_on_b);
            let mut decided_by_names = false;
            if (!both_ways || lopsided) && b_on_a != a_on_b {
                decided_by_names = true;
                if b_on_a > a_on_b {
                    vote(a, b, b_on_a - a_on_b, Reason::Names(named_by_b));
                } else {
                    vote(b, a, a_on_b - b_on_a, Reason::Names(named_by_a));
                }
            }

            let same_subject = subjects[a]
                .iter()
                .any(|subject| subjects[b].iter().any(|other| other.key == subject.key));
            if same_subject {
                let (level_a, level_b) = (entries[a].described.level, entries[b].described.level);
                if level_a != level_b {
                    let steps = f32::from((level_b - level_a).abs());
                    if level_a < level_b {
                        vote(a, b, MENTION_CAP * steps, Reason::Level);
                    } else {
                        vote(b, a, MENTION_CAP * steps, Reason::Level);
                    }
                }
                match (entries[a].described.volume, entries[b].described.volume) {
                    (Some(volume_a), Some(volume_b)) if volume_a < volume_b => {
                        vote(a, b, MENTION_CAP, Reason::Volume);
                    }
                    (Some(volume_a), Some(volume_b)) if volume_a > volume_b => {
                        vote(b, a, MENTION_CAP, Reason::Volume);
                    }
                    _ => {}
                }
            } else if !decided_by_names {
                let a_broader = is_broader(&subjects[a], &subjects[b]);
                let b_broader = is_broader(&subjects[b], &subjects[a]);
                if a_broader && !b_broader {
                    vote(a, b, MENTION_CAP, Reason::Narrower);
                } else if b_broader && !a_broader {
                    vote(b, a, MENTION_CAP, Reason::Narrower);
                }
            }
        }
    }
    for &(citing, cited) in cites {
        let in_scope = |index: usize| entries.get(index).is_some_and(|entry| entry.in_scope);
        if citing != cited && in_scope(citing) && in_scope(cited) {
            vote(cited, citing, MENTION_CAP, Reason::Cites);
        }
    }

    let mut score = vec![0.0f32; entries.len()];
    for (&(a, b), pair) in &votes {
        score[a] += pair.amount;
        score[b] -= pair.amount;
    }

    // First to last by total. A document nothing was decided about keeps a
    // total of zero and is taken out below.
    let mut ranked = scope.clone();
    ranked.sort_by(|&a, &b| {
        score[b]
            .total_cmp(&score[a])
            .then_with(|| entries[a].described.level.cmp(&entries[b].described.level))
            .then_with(|| absent_last(entries[a].described.volume, entries[b].described.volume))
            .then_with(|| absent_last(entries[a].publication, entries[b].publication))
            .then_with(|| entries[a].name.cmp(entries[b].name))
    });
    let rank: HashMap<usize, usize> = ranked
        .iter()
        .enumerate()
        .map(|(position, &index)| (index, position))
        .collect();

    // A decided pair stands only when the order as a whole agrees with it:
    // one the totals overrule would show a document "after" one listed below.
    let mut after: Vec<Vec<Link>> = vec![Vec::new(); entries.len()];
    let mut needed_by = vec![0usize; entries.len()];
    for (&(a, b), pair) in &votes {
        if pair.amount == 0.0 {
            continue;
        }
        let (first, later, reasons) = if pair.amount > 0.0 {
            (a, b, &pair.lower_first)
        } else {
            (b, a, &pair.higher_first)
        };
        if rank[&first] < rank[&later] {
            after[later].push(Link {
                document: first,
                reasons: reasons.clone(),
            });
            needed_by[first] += 1;
        }
    }
    for links in &mut after {
        links.sort_by_key(|link| rank[&link.document]);
    }

    let documents: Vec<Placed> = (0..entries.len())
        .map(|index| {
            let entry = &entries[index];
            let placement = if !entry.in_scope || entry.reading.is_none() {
                Placement::NoText
            } else {
                match (after[index].is_empty(), needed_by[index] == 0) {
                    (true, false) => Placement::Foundation,
                    (false, false) => Placement::Bridge,
                    (false, true) => Placement::Specialised,
                    (true, true) if subjects[index].is_empty() => Placement::NoSubject,
                    (true, true) => Placement::Standalone,
                }
            };
            Placed {
                placement,
                subjects: subjects[index].clone(),
                after: std::mem::take(&mut after[index]),
                needed_by: needed_by[index],
                score: score[index],
            }
        })
        .collect();

    let by_name = |a: &usize, b: &usize| entries[*a].name.cmp(entries[*b].name);
    let mut sequence: Vec<usize> = ranked
        .iter()
        .copied()
        .filter(|&index| documents[index].placement.is_ordered())
        .collect();
    for group in [
        Placement::Standalone,
        Placement::NoSubject,
        Placement::NoText,
    ] {
        let mut members: Vec<usize> = scope
            .iter()
            .copied()
            .filter(|&index| documents[index].placement == group)
            .collect();
        members.sort_by(by_name);
        sequence.extend(members);
    }

    let mut counted: HashMap<&str, (&Subject, usize)> = HashMap::new();
    for &owner in &scope {
        for (subject, credited) in subjects[owner].iter().zip(&credited[owner]) {
            if !credited || counted.contains_key(subject.key.as_str()) {
                continue;
            }
            let documents = scope
                .iter()
                .filter(|&&index| {
                    subjects[index].iter().any(|own| own.key == subject.key)
                        || entries[index]
                            .reading
                            .is_some_and(|reading| reading.uses(&subject.key) >= USE_MIN_PASSAGES)
                })
                .count();
            counted.insert(subject.key.as_str(), (subject, documents));
        }
    }
    let mut scope_subjects: Vec<(Subject, usize)> = counted
        .into_values()
        .map(|(subject, documents)| (subject.clone(), documents))
        .collect();
    scope_subjects.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.name.cmp(&b.0.name)));

    Outcome {
        documents,
        sequence,
        subjects: scope_subjects,
    }
}

/// What was said about one pair of documents.
#[derive(Default)]
struct PairVotes {
    /// Positive when the document with the lower index comes first.
    amount: f32,
    /// Why the lower index would come first, and why the higher would.
    lower_first: Vec<Reason>,
    higher_first: Vec<Reason>,
}

/// `None` after every `Some`.
fn absent_last<T: Ord>(a: Option<T>, b: Option<T>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// Which of each entry's subjects another document can be credited for
/// naming, aligned with `subjects`. A phrase always is. A single word is
/// unless the library shows it to be an ordinary one: most documents use it
/// ([`ORDINARY_WORD_SHARE`]), or its owner uses it no more than the rest do
/// ([`DISTINCTIVE_FACTOR`]).
fn credited_subjects(entries: &[Entry<'_>], subjects: &[Vec<Subject>]) -> Vec<Vec<bool>> {
    let with_text = entries
        .iter()
        .filter(|entry| entry.reading.is_some())
        .count();
    // Per one-word subject: how many documents use it, and the mean share of
    // passages among the documents it is not a subject of.
    let mut measured: HashMap<&str, (usize, f32)> = HashMap::new();
    for subject in subjects.iter().flatten() {
        if subject.key.contains(' ') || measured.contains_key(subject.key.as_str()) {
            continue;
        }
        let (mut users, mut others, mut share) = (0usize, 0usize, 0.0f32);
        for (entry, own) in entries.iter().zip(subjects) {
            let Some(reading) = entry.reading else {
                continue;
            };
            users += usize::from(reading.uses(&subject.key) >= USE_MIN_PASSAGES);
            if !own.iter().any(|s| s.key == subject.key) {
                others += 1;
                share += reading.share(&subject.key);
            }
        }
        let rest = if others == 0 {
            0.0
        } else {
            share / others as f32
        };
        measured.insert(subject.key.as_str(), (users, rest));
    }
    entries
        .iter()
        .zip(subjects)
        .map(|(entry, own)| {
            own.iter()
                .map(|subject| {
                    let Some(&(users, rest)) = measured.get(subject.key.as_str()) else {
                        return true;
                    };
                    let widely_used = with_text >= ORDINARY_MIN_LIBRARY
                        && users as f32 > ORDINARY_WORD_SHARE * with_text as f32;
                    let own_share = entry
                        .reading
                        .map_or(0.0, |reading| reading.share(&subject.key));
                    !widely_used && own_share >= DISTINCTIVE_FACTOR * rest
                })
                .collect()
        })
        .collect()
}

/// How much a document names another's subjects, and which. A subject the two
/// share is neither's to be credited for, and an ordinary word is no one's.
fn names(
    reading: &Reading,
    own: &[Subject],
    theirs: &[Subject],
    credited: &[bool],
) -> (f32, Vec<Subject>) {
    let mut total = 0.0;
    let mut named = Vec::new();
    for (subject, credited) in theirs.iter().zip(credited) {
        if !credited || own.iter().any(|s| s.key == subject.key) {
            continue;
        }
        if reading.uses(&subject.key) >= USE_MIN_PASSAGES {
            total += reading.share(&subject.key).min(MENTION_CAP);
            named.push(subject.clone());
        }
    }
    (total, named)
}

/// Whether a subject of `broad` is contained, as whole words, in a longer
/// subject of `narrow`: "statistics" in "inferential statistics".
fn is_broader(broad: &[Subject], narrow: &[Subject]) -> bool {
    broad.iter().any(|general| {
        narrow.iter().any(|specific| {
            specific.key != general.key
                && format!(" {} ", specific.key).contains(&format!(" {} ", general.key))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A text of `passages` passages. Each phrase given appears in the first
    /// so many of them, as a sentence of its own.
    fn book(passages: usize, uses: &[(&str, usize)]) -> String {
        (0..passages)
            .map(|passage| {
                let said: String = uses
                    .iter()
                    .filter(|(_, count)| *count > passage)
                    .map(|(phrase, _)| format!("{phrase}. "))
                    .collect();
                let padding = PASSAGE_CHARS.saturating_sub(said.chars().count());
                format!("{said}{}", " ".repeat(padding))
            })
            .collect()
    }

    fn described(title: &str, folders: &[&str]) -> Described {
        Described::from_labels(&Labels {
            title: Some(title.to_string()),
            file_stem: title.to_string(),
            folders: folders.iter().map(|folder| folder.to_string()).collect(),
        })
    }

    #[derive(Default)]
    struct Shelf {
        described: Vec<Described>,
        texts: Vec<Option<String>>,
        names: Vec<String>,
    }

    impl Shelf {
        fn with(mut self, title: &str, text: Option<String>) -> Self {
            self.described.push(described(title, &[]));
            self.texts.push(text);
            self.names.push(title.to_string());
            self
        }

        fn order(&self, cites: &[(usize, usize)]) -> Outcome {
            self.order_scope(cites, |_| true)
        }

        fn order_scope(
            &self,
            cites: &[(usize, usize)],
            in_scope: impl Fn(usize) -> bool,
        ) -> Outcome {
            let lexicon = Lexicon::new(self.described.iter().flat_map(Described::candidates));
            let readings: Vec<Option<Reading>> = self
                .texts
                .iter()
                .map(|text| text.as_deref().map(|text| read_text(text, &lexicon)))
                .collect();
            let entries: Vec<Entry<'_>> = (0..self.described.len())
                .map(|index| Entry {
                    described: &self.described[index],
                    reading: readings[index].as_ref(),
                    in_scope: in_scope(index),
                    publication: None,
                    name: &self.names[index],
                })
                .collect();
            order(&entries, cites)
        }

        fn titles(&self, outcome: &Outcome) -> Vec<&str> {
            outcome
                .sequence
                .iter()
                .map(|&index| self.names[index].as_str())
                .collect()
        }

        fn placement(&self, outcome: &Outcome, title: &str) -> Placement {
            let index = self.names.iter().position(|name| name == title).unwrap();
            outcome.documents[index].placement
        }
    }

    /// A book wholly about one subject.
    fn about(subject: &str) -> Option<String> {
        Some(book(20, &[(subject, 20)]))
    }

    /// Arithmetic, algebra that names arithmetic, and calculus that names
    /// both: the order a curriculum gives them. Given out of order.
    fn curriculum() -> Shelf {
        Shelf::default()
            .with(
                "Calculus",
                Some(book(
                    20,
                    &[("calculus", 20), ("algebra", 4), ("arithmetic", 2)],
                )),
            )
            .with(
                "Algebra",
                Some(book(20, &[("algebra", 20), ("arithmetic", 3)])),
            )
            .with("Arithmetic", about("arithmetic"))
    }

    #[test]
    fn a_plural_folds_to_its_singular_and_a_singular_is_left_alone() {
        for (word, folded) in [
            ("statistics", "statistic"),
            ("policies", "policy"),
            ("classes", "class"),
            ("analysis", "analysis"),
            ("bias", "bias"),
            ("loss", "loss"),
            ("gas", "gas"),
            ("is", "is"),
            ("señales", "señale"),
        ] {
            let mut owned = word.to_string();
            fold_word(&mut owned);
            assert_eq!(owned, folded, "{word}");
        }
    }

    #[test]
    fn a_label_gives_whole_phrases_without_the_words_that_frame_them() {
        let keys = |title: &str| -> Vec<String> {
            segments_of(title)
                .iter()
                .map(|segment| key_of(segment))
                .collect()
        };
        assert_eq!(
            keys("Calculus & Real Analysis"),
            ["calculus", "real analysis"]
        );
        assert_eq!(
            keys("Introduction to Reinforcement Learning"),
            ["reinforcement learning"]
        );
        assert_eq!(keys("Principles of Data Science"), ["data science"]);
        assert_eq!(keys("Advanced Research Methods"), ["research method"]);
        assert_eq!(keys("DLBCSM201 - Mathematics II"), ["mathematic"]);
        assert_eq!(keys("Multi-Agent Systems"), ["multi agent system"]);
        assert_eq!(
            keys("Applied Statistics & Probability"),
            ["statistic", "probability"]
        );
        assert_eq!(keys("High School Statistics"), ["high statistic"]);
        assert_eq!(
            keys("Statistics – Probability and Descriptive Statistics"),
            ["statistic", "probability", "descriptive statistic"]
        );
    }

    #[test]
    fn a_file_name_reads_as_the_title_it_stands_for() {
        assert_eq!(
            tidy_file_stem("calculus-volume-1_-_WEB"),
            "calculus volume 1   WEB"
        );
        assert_eq!(
            tidy_file_stem("Basic Mathematics -- Serge Lang -- 1971 -- Addison Wesley"),
            "Basic Mathematics"
        );
        assert_eq!(
            tidy_file_stem("Arithmetic for the Practical Man, 3ed (Mathematics for Self Study)"),
            "Arithmetic for the Practical Man, 3ed "
        );
        assert_eq!(
            tidy_file_stem("justinmath-linearAlgebra"),
            "justinmath linear Algebra"
        );
    }

    #[test]
    fn level_and_volume_are_read_from_the_labels() {
        assert_eq!(described("Basic Algebra", &[]).level, -1);
        assert_eq!(described("Advanced Statistics", &[]).level, 1);
        assert_eq!(
            described("Reinforcement Learning", &["RL Foundations"]).level,
            -1
        );
        assert_eq!(described("Linear Algebra", &[]).level, 0);
        assert_eq!(described("Calculus Volume 2", &[]).volume, Some(2));
        assert_eq!(described("Mathematics II", &[]).volume, Some(2));
        assert_eq!(described("Linear Algebra", &[]).volume, None);
        // A year is not a volume.
        assert_eq!(described("Statistics 2026", &[]).volume, None);
    }

    #[test]
    fn a_reading_counts_passages_not_mentions_and_matches_a_plural() {
        let lexicon = Lexicon::new(["random variable".to_string(), "algebra".to_string()]);
        let pad = |passage: &str| format!("{passage:<width$}", width = PASSAGE_CHARS);
        let text = [
            pad("A random variable. Two random variables. Random variables again."),
            pad("Nothing here."),
            pad("Algebra, and one more random variable."),
        ]
        .concat();

        let reading = read_text(&text, &lexicon);

        assert_eq!(reading.passages, 3);
        assert_eq!(reading.uses("random variable"), 2);
        assert_eq!(reading.uses("algebra"), 1);
        assert_eq!(reading.uses("calculus"), 0);
        assert!((reading.share("random variable") - 2.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn a_name_does_not_run_across_the_end_of_a_sentence() {
        let lexicon = Lexicon::new(["linear algebra".to_string()]);
        let reading = read_text("The map is linear. Algebra comes later.", &lexicon);
        assert_eq!(reading.uses("linear algebra"), 0);
    }

    #[test]
    fn an_empty_text_spans_no_passage() {
        let reading = read_text("  \n ", &Lexicon::new(["algebra".to_string()]));
        assert_eq!(reading.passages, 0);
        assert_eq!(reading.share("algebra"), 0.0);
    }

    #[test]
    fn a_subject_is_the_longest_stretch_of_a_label_the_text_uses() {
        // The title's whole phrase never appears; its opening word does.
        let described = described("Statistics Open", &[]);
        let lexicon = Lexicon::new(described.candidates());
        let text = book(3, &[("statistics", 3)]);

        let subjects = described.subjects(&read_text(&text, &lexicon));

        assert_eq!(
            subjects,
            [Subject {
                name: "statistics".into(),
                key: "statistic".into()
            }]
        );
    }

    #[test]
    fn a_single_word_is_a_subject_only_of_a_text_that_keeps_saying_it() {
        // An author's surname in the file name, on three of a hundred pages.
        let described = described("Peter - Statistics", &[]);
        let lexicon = Lexicon::new(described.candidates());
        let text = book(
            100,
            &[("statistics", 40), ("peter", 3), ("peter principle", 3)],
        );

        let reading = read_text(&text, &lexicon);
        let names: Vec<String> = described
            .subjects(&reading)
            .into_iter()
            .map(|subject| subject.name)
            .collect();

        assert_eq!(names, ["statistics"]);
    }

    #[test]
    fn a_title_the_text_never_repeats_names_no_subject() {
        let described = described("interactive-books", &[]);
        let lexicon = Lexicon::new(described.candidates());
        let text = book(3, &[("algebra", 3)]);
        assert!(described.subjects(&read_text(&text, &lexicon)).is_empty());
    }

    #[test]
    fn a_document_comes_after_the_ones_whose_subjects_it_names() {
        let shelf = curriculum();
        let outcome = shelf.order(&[]);

        assert_eq!(
            shelf.titles(&outcome),
            ["Arithmetic", "Algebra", "Calculus"]
        );
        assert_eq!(
            shelf.placement(&outcome, "Arithmetic"),
            Placement::Foundation
        );
        assert_eq!(shelf.placement(&outcome, "Algebra"), Placement::Bridge);
        assert_eq!(
            shelf.placement(&outcome, "Calculus"),
            Placement::Specialised
        );
        assert_eq!(outcome.documents[2].needed_by, 2);
        // Calculus comes after both, the earlier in the order first.
        let after: Vec<usize> = outcome.documents[0]
            .after
            .iter()
            .map(|link| link.document)
            .collect();
        assert_eq!(after, [2, 1]);
        assert_eq!(
            outcome.documents[1].after[0].reasons,
            [Reason::Names(vec![Subject {
                name: "arithmetic".into(),
                key: "arithmetic".into()
            }])]
        );
    }

    #[test]
    fn one_mention_is_not_a_use() {
        let shelf = Shelf::default()
            .with(
                "Algebra",
                Some(book(20, &[("algebra", 20), ("calculus", 1)])),
            )
            .with("Calculus", about("calculus"));
        let outcome = shelf.order(&[]);
        assert_eq!(shelf.placement(&outcome, "Algebra"), Placement::Standalone);
        assert_eq!(shelf.placement(&outcome, "Calculus"), Placement::Standalone);
    }

    #[test]
    fn documents_that_name_each_other_about_equally_are_not_ordered() {
        let shelf = Shelf::default()
            .with(
                "Probability",
                Some(book(40, &[("probability", 40), ("statistics", 3)])),
            )
            .with(
                "Statistics",
                Some(book(40, &[("statistics", 40), ("probability", 2)])),
            );
        let outcome = shelf.order(&[]);
        assert_eq!(
            shelf.placement(&outcome, "Probability"),
            Placement::Standalone
        );
        assert_eq!(
            shelf.placement(&outcome, "Statistics"),
            Placement::Standalone
        );
    }

    #[test]
    fn a_lopsided_pair_is_ordered_though_each_names_the_other() {
        let shelf = Shelf::default()
            .with(
                "Statistics",
                Some(book(40, &[("statistics", 40), ("probability", 12)])),
            )
            .with(
                "Probability",
                Some(book(40, &[("probability", 40), ("statistics", 2)])),
            );
        let outcome = shelf.order(&[]);
        assert_eq!(shelf.titles(&outcome), ["Probability", "Statistics"]);
    }

    #[test]
    fn a_word_its_owner_uses_no_more_than_the_rest_is_not_a_subject_to_build_on() {
        // "Papers" is where the first document is filed, and every document
        // says "paper" throughout.
        let mut shelf = Shelf::default()
            .with(
                "Graph Mining",
                Some(book(20, &[("graph mining", 20), ("paper", 20)])),
            )
            .with(
                "Topology",
                Some(book(20, &[("topology", 20), ("paper", 18)])),
            )
            .with(
                "Sampling",
                Some(book(20, &[("sampling", 20), ("paper", 19)])),
            );
        shelf.described[0] = Described::from_labels(&Labels {
            title: Some("Graph Mining".into()),
            file_stem: "Graph Mining".into(),
            folders: vec!["Papers".into()],
        });

        let outcome = shelf.order(&[]);

        assert!(outcome.documents[0]
            .subjects
            .iter()
            .any(|subject| subject.key == "paper"));
        assert!(outcome
            .documents
            .iter()
            .all(|document| document.placement == Placement::Standalone));
        assert!(!outcome
            .subjects
            .iter()
            .any(|(subject, _)| subject.key == "paper"));
    }

    #[test]
    fn a_word_most_of_a_large_library_uses_credits_no_document() {
        // Thirty documents, and all of them say "effect" in a few passages:
        // the one titled "Effect" is not what they build on.
        let mut shelf = Shelf::default().with("Effect", about("effect"));
        for index in 0..30 {
            let subject = format!("subject{}", (b'a' + index) as char);
            shelf = shelf.with(
                &subject,
                Some(book(40, &[(subject.as_str(), 40), ("effect", 3)])),
            );
        }
        let outcome = shelf.order(&[]);
        assert!(outcome
            .documents
            .iter()
            .all(|document| document.placement == Placement::Standalone));

        // The same word in a library too small to say which words are
        // ordinary is taken as the subject its title says it is.
        let small = Shelf::default().with("Effect", about("effect")).with(
            "Topology",
            Some(book(40, &[("topology", 40), ("effect", 3)])),
        );
        let outcome = small.order(&[]);
        assert_eq!(small.titles(&outcome), ["Effect", "Topology"]);
    }

    #[test]
    fn the_same_subject_is_ordered_by_level_then_volume() {
        let shelf = Shelf::default()
            .with("Advanced Algebra", about("algebra"))
            .with("Algebra Volume 2", about("algebra"))
            .with("Basic Algebra", about("algebra"))
            .with("Algebra Volume 1", about("algebra"));
        let outcome = shelf.order(&[]);
        assert_eq!(
            shelf.titles(&outcome),
            [
                "Basic Algebra",
                "Algebra Volume 1",
                "Algebra Volume 2",
                "Advanced Algebra"
            ]
        );
        assert!(outcome.documents[1]
            .after
            .iter()
            .any(|link| link.document == 3 && link.reasons == [Reason::Volume]));
    }

    #[test]
    fn a_narrower_subject_comes_after_the_one_it_is_a_case_of() {
        let shelf = Shelf::default()
            .with("Inferential Statistics", about("inferential statistics"))
            .with("Statistics", about("statistics"));
        let outcome = shelf.order(&[]);
        assert_eq!(
            shelf.titles(&outcome),
            ["Statistics", "Inferential Statistics"]
        );
        assert_eq!(outcome.documents[0].after[0].reasons, [Reason::Narrower]);
    }

    #[test]
    fn a_document_comes_after_one_it_cites() {
        let shelf = Shelf::default()
            .with("Sampling Methods", about("sampling methods"))
            .with("Graph Mining", about("graph mining"));
        // The first cites the second.
        let outcome = shelf.order(&[(0, 1)]);
        assert_eq!(shelf.titles(&outcome), ["Graph Mining", "Sampling Methods"]);
        assert_eq!(outcome.documents[0].after[0].reasons, [Reason::Cites]);
    }

    #[test]
    fn what_cannot_be_placed_is_listed_last_and_says_why() {
        let shelf = curriculum()
            .with("Scanned", None)
            .with("Untitled", Some(book(20, &[("nothing to see", 20)])))
            .with("Topology", about("topology"));

        let outcome = shelf.order(&[]);

        assert_eq!(
            shelf.titles(&outcome),
            [
                "Arithmetic",
                "Algebra",
                "Calculus",
                "Topology",
                "Untitled",
                "Scanned"
            ]
        );
        assert_eq!(shelf.placement(&outcome, "Topology"), Placement::Standalone);
        assert_eq!(shelf.placement(&outcome, "Untitled"), Placement::NoSubject);
        assert_eq!(shelf.placement(&outcome, "Scanned"), Placement::NoText);
    }

    #[test]
    fn a_document_without_a_subject_is_still_placed_by_the_subjects_it_names() {
        let shelf = Shelf::default()
            .with("Algebra", about("algebra"))
            .with("interactive-books", Some(book(20, &[("algebra", 4)])));
        let outcome = shelf.order(&[]);
        assert_eq!(shelf.titles(&outcome), ["Algebra", "interactive-books"]);
        assert_eq!(
            shelf.placement(&outcome, "interactive-books"),
            Placement::Specialised
        );
    }

    #[test]
    fn only_documents_in_scope_are_ordered() {
        let shelf = curriculum();
        // Arithmetic is in the library and out of the scope.
        let outcome = shelf.order_scope(&[], |index| index != 2);

        assert_eq!(shelf.titles(&outcome), ["Algebra", "Calculus"]);
        assert!(outcome.documents[0]
            .after
            .iter()
            .all(|link| link.document == 1));
    }

    #[test]
    fn the_scope_lists_its_subjects_by_how_many_documents_have_or_name_them() {
        let shelf = curriculum();
        let outcome = shelf.order(&[]);
        let listed: Vec<(&str, usize)> = outcome
            .subjects
            .iter()
            .map(|(subject, documents)| (subject.name.as_str(), *documents))
            .collect();
        assert_eq!(listed, [("arithmetic", 3), ("algebra", 2), ("calculus", 1)]);
    }

    #[test]
    fn a_theme_is_read_like_a_subject() {
        assert_eq!(
            theme_key("Random Variables").as_deref(),
            Some("random variable")
        );
        assert_eq!(theme_key("  "), None);
        assert_eq!(theme_key("one two three four five"), None);
    }
}
