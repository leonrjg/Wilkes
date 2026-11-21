//! What a document's title can be trusted to say.
//!
//! Two questions, both about the title a file carries in its own metadata:
//! whether it is a title at all, or the name of a file the document was once
//! made from; and, when the document names no DOI, which work a literature
//! provider's search answer names under that same title.

use std::path::Path;

use super::doi::normalize_doi;
use crate::types::LiteratureSearchResult;

/// Whether `title` is a file's name or path rather than a title.
///
/// Typesetting and word-processing tools write the name of their source file
/// into the PDF's title field (`www10_sarwar.dvi`, `D:\proofs\paper.dvi`), and
/// a document is often titled after its own file. Neither names the work. Only
/// those two are recognised: a title is otherwise taken as written.
pub fn is_file_name(title: &str, path: &Path) -> bool {
    let title = title.trim();
    names_this_file(title, path) || last_path_segment_is_a_file_name(title)
}

fn names_this_file(title: &str, path: &Path) -> bool {
    let title = title.to_lowercase();
    [path.file_name(), path.file_stem()]
        .into_iter()
        .flatten()
        .any(|name| name.to_string_lossy().to_lowercase() == title)
}

/// The text after the last `/` or `\` is one word with a file extension: a
/// stem, a dot, then one to five ASCII letters or digits beginning with a
/// letter. A title with a slash in it (`TCP/IP`, `Input/Output in C`) ends in
/// words, not in a file name.
fn last_path_segment_is_a_file_name(title: &str) -> bool {
    let segment = title.rsplit(['/', '\\']).next().unwrap_or(title);
    if segment.chars().any(char::is_whitespace) {
        return false;
    }
    let Some((stem, extension)) = segment.rsplit_once('.') else {
        return false;
    };
    let extension_chars = extension.chars().count();
    !stem.is_empty()
        && (1..=5).contains(&extension_chars)
        && extension
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
        && extension.chars().all(|ch| ch.is_ascii_alphanumeric())
}

/// What a provider's search answer says about the work a title names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TitleMatch {
    /// Exactly one DOI is held by results bearing this title.
    Found(String),
    /// Several DOIs are held by results bearing this title, and nothing here
    /// tells them apart.
    Ambiguous(Vec<String>),
    /// No result bearing this title holds a DOI.
    NotFound,
}

/// The DOI of the one work in `results` whose title is `title`.
///
/// A result matches when its title is the same words in the same order, case
/// and punctuation aside, and, when both the document and the result name
/// authors, when they share a name. Several matching works with different DOIs
/// are refused rather than chosen between: a title alone cannot say which of
/// them the document is.
pub fn match_title(
    title: &str,
    author: Option<&str>,
    results: &[LiteratureSearchResult],
) -> TitleMatch {
    let wanted = normalized_title(title);
    if wanted.is_empty() {
        return TitleMatch::NotFound;
    }
    let author_words = author.map(name_words).filter(|words| !words.is_empty());
    let mut dois: Vec<String> = Vec::new();
    for result in results {
        let Some(result_title) = result.title.as_deref() else {
            continue;
        };
        if normalized_title(result_title) != wanted {
            continue;
        }
        if let (Some(author_words), Some(result_authors)) =
            (author_words.as_ref(), result.authors.as_deref())
        {
            let result_words = name_words(result_authors);
            if !result_words.is_empty() && !result_words.iter().any(|w| author_words.contains(w)) {
                continue;
            }
        }
        let Some(doi) = result.doi.as_deref().and_then(normalize_doi) else {
            continue;
        };
        if !dois.contains(&doi) {
            dois.push(doi);
        }
    }
    match dois.len() {
        0 => TitleMatch::NotFound,
        1 => TitleMatch::Found(dois.remove(0)),
        _ => TitleMatch::Ambiguous(dois),
    }
}

/// Lowercased words of letters and digits, joined by single spaces.
fn normalized_title(title: &str) -> String {
    title
        .to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The words of an author list long enough to be a name rather than an
/// initial or a joining word.
fn name_words(authors: &str) -> Vec<String> {
    authors
        .to_lowercase()
        .split(|ch: char| !ch.is_alphabetic())
        .filter(|word| word.chars().count() >= 3 && *word != "and")
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LiteratureAcquisition;

    fn result(title: &str, doi: Option<&str>, authors: Option<&str>) -> LiteratureSearchResult {
        LiteratureSearchResult {
            id: title.to_string(),
            doi: doi.map(str::to_string),
            title: Some(title.to_string()),
            year: None,
            publication_date: None,
            venue: None,
            citation_count: None,
            is_open_access: false,
            pdf_url: None,
            landing_page_url: None,
            image_url: None,
            open_access_status: None,
            license: None,
            authors: authors.map(str::to_string),
            publisher: None,
            language: None,
            file_format: None,
            file_size: None,
            summary: None,
            subject: None,
            kind: None,
            pages: None,
            recall_score: None,
            acquisition: LiteratureAcquisition::default(),
        }
    }

    #[test]
    fn a_source_file_name_written_as_the_title_is_not_a_title() {
        let path = Path::new("/library/sarwar-2001.pdf");
        assert!(is_file_name("www10_sarwar.dvi", path));
        assert!(is_file_name("radovanovic10a.dvi", path));
        assert!(is_file_name("  TII00006.dvi ", path));
    }

    #[test]
    fn a_path_written_as_the_title_is_not_a_title() {
        let path = Path::new("/library/benini.pdf");
        assert!(is_file_name(
            r"D:\AbhayRaj\14-3-12\tcsvt-benini-2211935\tcsvt-benini-2211935-proof.dvi",
            path
        ));
        assert!(is_file_name("/home/me/drafts/paper.tex", path));
    }

    #[test]
    fn the_documents_own_file_name_is_not_a_title() {
        let path = Path::new("/library/boyd-2020-the-narrative-arc.pdf");
        assert!(is_file_name("boyd-2020-the-narrative-arc", path));
        assert!(is_file_name("Boyd-2020-The-Narrative-Arc.PDF", path));
    }

    #[test]
    fn titles_are_taken_as_written() {
        let path = Path::new("/library/paper.pdf");
        for title in [
            "The narrative arc: Revealing core narrative structures through text analysis",
            "Microsoft Word - Similarity.docx",
            "untitled",
            "CSUR5305-106",
            "TCP/IP Illustrated",
            "Input/Output in C",
            "Web 2.0 and You",
            "Scaling to v1.0",
            "U.S. Patent Law",
            "arXiv:2101.00001",
        ] {
            assert!(!is_file_name(title, path), "{title:?} was refused");
        }
    }

    #[test]
    fn the_one_result_with_the_same_title_names_the_doi() {
        let results = [
            result("Story Embeddings", Some("10.1/other"), None),
            result(
                "Where Have I Heard This Story Before? Identifying Narrative Similarity in Movie Remakes",
                Some("https://doi.org/10.18653/v1/N18-2106"),
                Some("Snigdha Chaturvedi, Shashank Srivastava, Dan Roth"),
            ),
        ];
        assert_eq!(
            match_title(
                "Where have I Heard this Story Before?: Identifying Narrative Similarity in Movie Remakes",
                Some("Snigdha Chaturvedi ; Shashank Srivastava ; Dan Roth"),
                &results,
            ),
            TitleMatch::Found("10.18653/v1/n18-2106".into())
        );
    }

    #[test]
    fn a_similar_title_is_not_the_same_title() {
        let results = [result(
            "Specialized document embeddings for aspect-based similarity of research papers",
            Some("10.1145/3529372.3530912"),
            None,
        )];
        assert_eq!(
            match_title(
                "Aspect-based Document Similarity for Research Papers",
                None,
                &results
            ),
            TitleMatch::NotFound
        );
    }

    #[test]
    fn two_works_under_one_title_are_not_chosen_between() {
        let title = "The narrative arc: Revealing core narrative structures through text analysis";
        let results = [
            result(title, Some("10.1126/sciadv.aba2196"), None),
            result(title, Some("10.29025/2079-6021-2022-4-17-34"), None),
        ];
        assert_eq!(
            match_title(title, None, &results),
            TitleMatch::Ambiguous(vec![
                "10.1126/sciadv.aba2196".into(),
                "10.29025/2079-6021-2022-4-17-34".into()
            ])
        );
    }

    #[test]
    fn the_same_work_twice_is_one_match() {
        let results = [
            result("Hubs in Space", Some("10.1/hubs"), None),
            result("Hubs in space.", Some("https://doi.org/10.1/HUBS"), None),
        ];
        assert_eq!(
            match_title("Hubs in Space", None, &results),
            TitleMatch::Found("10.1/hubs".into())
        );
    }

    #[test]
    fn authors_who_share_no_name_are_another_work() {
        let results = [result(
            "Evaluation",
            Some("10.1/evaluation"),
            Some("Jane Doe, John Roe"),
        )];
        assert_eq!(
            match_title("Evaluation", Some("Ryan L. Boyd"), &results),
            TitleMatch::NotFound
        );
        assert_eq!(
            match_title("Evaluation", Some("J. Doe"), &results),
            TitleMatch::Found("10.1/evaluation".into())
        );
    }

    #[test]
    fn a_result_without_a_doi_names_nothing() {
        let results = [result("SubRosa", None, None)];
        assert_eq!(match_title("SubRosa", None, &results), TitleMatch::NotFound);
    }
}
