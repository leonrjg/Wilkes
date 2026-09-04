//! Reading one MIT OpenCourseWare course into the documents it is made of.
//!
//! A course is not one file, which is why it has no `pdf_url` and cannot be
//! acquired the way a textbook is. It is a few dozen PDFs — lecture notes,
//! problem sets, exams, video transcripts — together with a handful of the
//! course's own web pages holding the syllabus, the calendar and the reading
//! list. Those pages are the only place the *ordering* lives: fetched without
//! them, a course arrives as a bag of documents with nothing saying which is
//! lecture one, and for a reading-list course whose PDFs are named `4.pdf` and
//! `13.pdf` that is close to unusable. So this module reads both halves, and
//! [`course_document`] renders the pages into a single Markdown file that is
//! written alongside the documents it describes.
//!
//! # Why the manifest rather than the download bundle
//!
//! OCW also publishes each course as a zip. That zip is the whole web site: for
//! 12.425 it is 10 MB, of which 800 kB is the PDFs and the rest is CSS,
//! MathJax and webpack bundles. Every JSON file inside it is served
//! individually at the same paths, so walking the manifest costs about 15 kB
//! of metadata and then fetches exactly the documents that were asked for.
//! It also sidesteps [`crate::acquire::MAX_DOWNLOAD_BYTES`], which the larger
//! course zips exceed while no single PDF inside one comes close.
//!
//! # What is refused, and why nothing is guessed
//!
//! Only `application/pdf` is kept. Everything else is *named* in
//! [`CourseManifest::skipped`] with the reason — audiovisual, an unhandled
//! type, or metadata too incomplete to classify — rather than dropped. A
//! resource whose `file_type` is absent is refused rather than inferred from
//! its extension: OCW states the type for every document it holds, so an
//! absence is a change in the feed and must surface as one.

use std::collections::HashMap;

use serde::{Deserialize, Deserializer};
use tokio::sync::mpsc;

use crate::network::ProviderHttpClient;
use crate::types::{CatalogueCourseProgress, CatalogueCourseStage};

/// Every manifest path is site-absolute (`/courses/<slug>/...`), so the origin
/// is supplied here rather than taken from the caller's URL. A course URL that
/// pointed elsewhere could then not redirect the document fetches with it.
pub const OCW_ORIGIN: &str = "https://ocw.mit.edu";

/// Ceiling on manifest entries walked for one course. The largest observed
/// course (8.01SC) declares a little over four hundred; this is well clear of
/// that and still bounds a feed that started answering with a cycle.
const MAX_MANIFEST_ENTRIES: usize = 2_000;

/// How OCW prefixes a stored file: thirty-two hex characters and an underscore
/// ahead of the name a human gave it. Stripped for the name on disk, because
/// `fda8db6bf38fc0b1c8ee2694027886f8_MIT11_165F11_ses01.pdf` is the storage
/// key and `MIT11_165F11_ses01.pdf` is the document.
const HASH_PREFIX_LEN: usize = 32;

/// The vendor name OCW's transcripts carry as their title. Every one of them
/// is called this, so the title says nothing and the filename is a YouTube id;
/// [`read_course`] renames them after the lecture they transcribe.
const TRANSCRIPT_TITLE: &str = "3play";

// ── What a course turns out to be ────────────────────────────────────────────

/// One document of a course, ready to fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CourseFile {
    /// The name this will be written under, already sanitised and unique
    /// within the course.
    pub filename: String,
    /// Absolute URL of the bytes.
    pub url: String,
    /// The section the course puts it in — `Lecture Notes`, `Assignments`,
    /// `Exams`. Absent for about a third of documents, which is why the
    /// generated document lists those under a heading that says so rather
    /// than filing them somewhere plausible.
    pub section: Option<String>,
    /// The title the course gives it, which is often more use than the
    /// filename and occasionally the only readable thing about it.
    pub title: String,
    pub description: String,
    pub size_bytes: Option<u64>,
}

/// One of the course's own web pages: prose that exists in no PDF.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoursePage {
    pub title: String,
    /// The page's HTML, converted. Markdown rather than HTML because the only
    /// extractor Wilkes registers is the PDF one — everything else is read as
    /// plain text, and `extract::outline::markdown_outline` then gives the
    /// generated document a real table of contents where raw tags would give
    /// it a page of angle brackets.
    pub markdown: String,
}

/// A resource this module refused, and why. Returned rather than logged away:
/// a course that quietly yielded three files out of forty would look like a
/// course with three files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedResource {
    pub title: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CourseManifest {
    pub course_url: String,
    /// The course's path segment, used as the directory the documents land in.
    pub slug: String,
    pub title: String,
    pub description: String,
    pub pages: Vec<CoursePage>,
    pub files: Vec<CourseFile>,
    pub skipped: Vec<SkippedResource>,
}

// ── The wire ─────────────────────────────────────────────────────────────────

/// `file_size` arrives as a number for documents and as a decimal string for
/// videos. Both are the same fact, so both are read; anything else is absent
/// rather than an error, because the size is only ever used to describe a
/// download and never to decide one.
fn flexible_size<'de, D: Deserializer<'de>>(de: D) -> Result<Option<u64>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(de)?;
    Ok(match value {
        Some(serde_json::Value::Number(n)) => n.as_u64(),
        Some(serde_json::Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    })
}

#[derive(Deserialize, Default)]
struct RawEntry {
    #[serde(default)]
    title: Option<String>,
    /// A page's prose.
    #[serde(default)]
    content: Option<String>,
    /// A resource's blurb.
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    file_type: Option<String>,
    #[serde(default, deserialize_with = "flexible_size")]
    file_size: Option<u64>,
    #[serde(default)]
    parent_title: Option<String>,
    /// `OCWFile` for a resource, `CourseSection` for a page.
    #[serde(default)]
    ocw_type: Option<String>,
    /// `Document`, `Video`, `Image`. The reliable audiovisual discriminator:
    /// a video's `file_type` is the empty string, not a MIME type.
    #[serde(default)]
    resourcetype: Option<String>,
    #[serde(default)]
    video_metadata: Option<RawVideoMetadata>,
}

#[derive(Deserialize, Default)]
struct RawVideoMetadata {
    #[serde(default)]
    youtube_id: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawCourse {
    #[serde(default)]
    course_title: Option<String>,
    #[serde(default)]
    course_description: Option<String>,
}

// ── Reading a course ─────────────────────────────────────────────────────────

fn report(
    progress: Option<&mpsc::Sender<CatalogueCourseProgress>>,
    update: CatalogueCourseProgress,
) {
    if let Some(tx) = progress {
        // Lossy for the reason every other progress channel here is: a
        // consumer that stopped draining must not slow the walk down.
        let _ = tx.try_send(update);
    }
}

/// The course's path segment: `.../courses/<slug>/` gives `<slug>`.
fn slug_of(course_url: &str) -> Option<String> {
    let trimmed = course_url.trim_end_matches('/');
    let (_, tail) = trimmed.rsplit_once("/courses/")?;
    let slug = tail.split('/').next().unwrap_or_default();
    (!slug.is_empty()).then(|| slug.to_string())
}

/// Refuses a course URL that is not an OCW course.
///
/// The host is checked and not merely the shape: every document URL this
/// module goes on to fetch is built by joining [`OCW_ORIGIN`] to a path the
/// response supplied, so a course URL pointing somewhere else would be a
/// response from one host deciding what is fetched from another.
fn course_base(course_url: &str) -> anyhow::Result<String> {
    let parsed = url::Url::parse(course_url.trim())
        .map_err(|error| anyhow::anyhow!("Not a usable course URL: {error}"))?;
    anyhow::ensure!(
        parsed.scheme() == "https" && parsed.host_str() == Some("ocw.mit.edu"),
        "Not an MIT OpenCourseWare course URL: {course_url}"
    );
    anyhow::ensure!(
        parsed.path().contains("/courses/"),
        "MIT OpenCourseWare URL names no course: {course_url}"
    );
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

/// Reads one course's manifest into the documents and pages it names.
///
/// Sequential rather than concurrent: a course is a few dozen small requests,
/// the retry and timeout policy lives in [`ProviderHttpClient`] and is
/// per-request, and a walk that fanned out would report its progress in an
/// order that had nothing to do with what was happening.
pub async fn read_course(
    http: &ProviderHttpClient,
    course_url: &str,
    progress: Option<&mpsc::Sender<CatalogueCourseProgress>>,
) -> anyhow::Result<CourseManifest> {
    let base = course_base(course_url)?;
    read_course_at(http, OCW_ORIGIN, &base, progress).await
}

/// The walk itself, with the origin the manifest's site-absolute paths hang
/// off. Split from [`read_course`] so the host check happens exactly once, in
/// front, and the loop can be exercised against a test server without the
/// check being what the test has to work around.
async fn read_course_at(
    http: &ProviderHttpClient,
    origin: &str,
    base: &str,
    progress: Option<&mpsc::Sender<CatalogueCourseProgress>>,
) -> anyhow::Result<CourseManifest> {
    let slug = slug_of(base)
        .ok_or_else(|| anyhow::anyhow!("MIT OpenCourseWare URL names no course: {base}"))?;

    let course: RawCourse = http.get_json(format!("{base}/data.json"), &[]).await?;
    let map: HashMap<String, String> = http
        .get_json(format!("{base}/content_map.json"), &[])
        .await?;

    // Sorted so a course read twice produces the same document in the same
    // order; a HashMap's iteration order would make the generated file differ
    // between runs over identical input.
    let mut paths: Vec<&String> = map.values().collect();
    paths.sort();
    paths.truncate(MAX_MANIFEST_ENTRIES);
    let total = paths.len();

    let mut pages: Vec<CoursePage> = Vec::new();
    let mut entries: Vec<RawEntry> = Vec::new();
    let mut skipped: Vec<SkippedResource> = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        // Six of 10.34's 182 entries map to the empty string. Joined to the
        // origin that is a request for OCW's home page, which answers with
        // HTML and lands here as "not JSON" — a confusing way to learn that
        // the manifest named nothing. A value that is not a course path is
        // refused before anything is fetched.
        if !path.starts_with("/courses/") {
            skipped.push(SkippedResource {
                title: (*path).clone(),
                reason: "manifest entry names no course path".into(),
            });
            continue;
        }
        let url = format!("{origin}{path}");
        let entry: RawEntry = match http.get_json(url, &[]).await {
            Ok(entry) => entry,
            Err(error) => {
                // One unreadable entry out of forty must not lose the other
                // thirty-nine, but it is still reported: a course silently
                // three documents short is worse than one that says so.
                tracing::warn!(path = %path, "OCW manifest entry unreadable: {error}");
                skipped.push(SkippedResource {
                    title: (*path).clone(),
                    reason: format!("manifest entry unreadable: {error}"),
                });
                continue;
            }
        };
        report(
            progress,
            CatalogueCourseProgress {
                course_url: base.to_string(),
                stage: CatalogueCourseStage::Manifest,
                done: index + 1,
                total: Some(total),
                current: entry.title.clone(),
            },
        );
        let is_page = entry.ocw_type.as_deref() == Some("CourseSection")
            || (entry.ocw_type.is_none() && path.contains("/pages/"));
        if is_page {
            let markdown = html_to_markdown(entry.content.as_deref().unwrap_or_default());
            // A page with no prose is a navigation stub — a heading over a
            // list of links that are themselves resources. Keeping it would
            // put empty sections in the generated document.
            if !markdown.trim().is_empty() {
                pages.push(CoursePage {
                    title: entry.title.clone().unwrap_or_else(|| "Untitled".into()),
                    markdown,
                });
            }
            continue;
        }
        entries.push(entry);
    }

    // Built before classification because a transcript is named after the
    // lecture it transcribes, and that lecture is a different manifest entry.
    let lectures = video_titles(&entries);
    let mut files: Vec<CourseFile> = Vec::new();
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    for entry in &entries {
        match classify(entry, origin, &lectures, &mut used) {
            Ok(file) => files.push(file),
            Err(skip) => skipped.push(skip),
        }
    }

    files.sort_by(|a, b| {
        section_rank(a.section.as_deref())
            .cmp(&section_rank(b.section.as_deref()))
            .then_with(|| a.title.cmp(&b.title))
    });
    pages.sort_by_key(|page| page_rank(&page.title));

    Ok(CourseManifest {
        course_url: base.to_string(),
        slug,
        title: course.course_title.unwrap_or_default(),
        description: course.course_description.unwrap_or_default(),
        pages,
        files,
        skipped,
    })
}

/// YouTube id to lecture title, for every video the course declares.
fn video_titles(entries: &[RawEntry]) -> HashMap<String, String> {
    entries
        .iter()
        .filter_map(|entry| {
            let id = entry
                .video_metadata
                .as_ref()?
                .youtube_id
                .as_deref()
                .filter(|id| !id.trim().is_empty())?;
            let title = entry.title.as_deref().filter(|t| !t.trim().is_empty())?;
            Some((id.to_string(), title.to_string()))
        })
        .collect()
}

/// Decides what one resource is, and refuses everything that is not a PDF.
///
/// `used` carries the names already claimed in this course so two documents
/// that reduce to the same filename do not collide — `download_to_root`
/// refuses same-name-different-content, and a course containing both a
/// `lecture5.pdf` and a `Lecture5.pdf` would otherwise fail halfway through.
fn classify(
    entry: &RawEntry,
    origin: &str,
    lectures: &HashMap<String, String>,
    used: &mut std::collections::HashSet<String>,
) -> Result<CourseFile, SkippedResource> {
    let title = entry
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "Untitled".into());
    let skip = |reason: &str| SkippedResource {
        title: title.clone(),
        reason: reason.to_string(),
    };

    let resourcetype = entry.resourcetype.as_deref().unwrap_or_default();
    let youtube = entry
        .video_metadata
        .as_ref()
        .and_then(|meta| meta.youtube_id.as_deref())
        .unwrap_or_default();
    if matches!(resourcetype, "Video" | "Audio") || !youtube.trim().is_empty() {
        return Err(skip("audiovisual"));
    }

    let Some(file) = entry.file.as_deref().filter(|f| !f.trim().is_empty()) else {
        return Err(skip("no file"));
    };
    // Absent rather than defaulted: OCW states a type for every document it
    // holds, so a missing one means the feed changed shape and guessing from
    // the extension would hide that behind a plausible-looking download.
    let Some(file_type) = entry.file_type.as_deref().filter(|t| !t.trim().is_empty()) else {
        return Err(skip("no file_type stated"));
    };
    if file_type != "application/pdf" {
        return Err(skip(&format!("not a PDF ({file_type})")));
    }

    let stored = file.rsplit('/').next().unwrap_or(file);
    let is_transcript = title.to_ascii_lowercase().contains(TRANSCRIPT_TITLE);
    let base_name = if is_transcript {
        // `PKbah48l3AU.pdf` names nothing. The video with that id does.
        match transcript_id(stored).and_then(|id| lectures.get(id)) {
            Some(lecture) => format!("{lecture} (transcript)"),
            // Still kept, still named honestly: an unmatched transcript is a
            // real document, and calling it what it is beats inventing a
            // lecture number for it.
            // Without dropping the extension first this becomes
            // `42TkHA__6bk.pdf (transcript).pdf`.
            None => format!(
                "{} (transcript)",
                strip_hash(stored).strip_suffix(".pdf").unwrap_or(stored)
            ),
        }
    } else {
        strip_hash(stored).to_string()
    };

    Ok(CourseFile {
        filename: unique_filename(&base_name, used),
        url: format!("{origin}{file}"),
        section: entry.parent_title.clone().filter(|s| !s.trim().is_empty()),
        title,
        description: entry.description.clone().unwrap_or_default(),
        size_bytes: entry.file_size,
    })
}

/// The YouTube id in a transcript's stored name, which ends `_<id>.pdf`.
fn transcript_id(stored: &str) -> Option<&str> {
    stored
        .strip_suffix(".pdf")
        .and_then(|stem| stem.rsplit('_').next())
        .filter(|id| id.chars().count() == 11)
}

/// Drops OCW's storage prefix when there is one, and leaves the name alone
/// when there is not — the prefix is fixed-width hex, so its presence is a
/// fact about the name rather than a guess.
fn strip_hash(stored: &str) -> &str {
    let mut chars = stored.chars();
    let prefix: String = chars.by_ref().take(HASH_PREFIX_LEN).collect();
    if prefix.chars().count() == HASH_PREFIX_LEN
        && prefix.chars().all(|c| c.is_ascii_hexdigit())
        && chars.next() == Some('_')
    {
        // Character-aware: the split point is counted in chars, never bytes.
        return stored
            .char_indices()
            .nth(HASH_PREFIX_LEN + 1)
            .map(|(offset, _)| &stored[offset..])
            .unwrap_or(stored);
    }
    stored
}

/// A filename safe on every platform Wilkes runs on, and unique in this course.
fn unique_filename(name: &str, used: &mut std::collections::HashSet<String>) -> String {
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.chars().count() <= 5 => (stem, ext),
        _ => (name, "pdf"),
    };
    let mut cleaned: String = stem
        .chars()
        .map(|c| {
            // Parentheses are kept because this function itself appends
            // "(2)" to disambiguate: stripping them from the name it was
            // given while adding them to the name it returns would be one
            // character class treated two ways.
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ' ' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .collect();
    cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    // Counted in characters, so a title with accents is trimmed to 120 glyphs
    // rather than cut through the middle of one.
    let trimmed: String = cleaned.chars().take(120).collect();
    let stem = if trimmed.trim().is_empty() {
        "document".to_string()
    } else {
        trimmed.trim().to_string()
    };
    let mut candidate = format!("{stem}.{extension}");
    let mut counter = 2;
    while !used.insert(candidate.to_ascii_lowercase()) {
        candidate = format!("{stem} ({counter}).{extension}");
        counter += 1;
    }
    candidate
}

/// Teaching order, so the generated document reads the way a course does
/// rather than the way a hash map iterates. Anything unlisted sorts after the
/// named sections and then alphabetically.
fn section_rank(section: Option<&str>) -> (usize, String) {
    const ORDER: [&str; 9] = [
        "Syllabus",
        "Calendar",
        "Readings",
        "Lecture Notes",
        "Recitations",
        "Assignments",
        "Labs",
        "Projects",
        "Exams",
    ];
    match section {
        Some(name) => match ORDER.iter().position(|known| *known == name) {
            Some(index) => (index, String::new()),
            None => (ORDER.len(), name.to_string()),
        },
        None => (ORDER.len() + 1, String::new()),
    }
}

fn page_rank(title: &str) -> (usize, String) {
    const ORDER: [&str; 5] = [
        "Syllabus",
        "Calendar",
        "Readings",
        "Lecture Notes",
        "Assignments",
    ];
    match ORDER.iter().position(|known| *known == title) {
        Some(index) => (index, String::new()),
        None => (ORDER.len(), title.to_string()),
    }
}

// ── The generated course document ────────────────────────────────────────────

/// Renders a course's pages and contents as one Markdown document.
///
/// This is the file that makes the rest of the download a course. It carries
/// the syllabus, the calendar and the reading list — none of which OCW
/// publishes as a PDF in all but a handful of courses — and then an index of
/// every document fetched alongside it, under the section the course files it
/// in. What was refused is listed too, because a reader who can see that four
/// lecture videos were skipped knows why the sequence has gaps in it.
pub fn course_document(manifest: &CourseManifest) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", manifest.title.trim()));
    if !manifest.description.trim().is_empty() {
        out.push_str(manifest.description.trim());
        out.push_str("\n\n");
    }
    out.push_str(&format!("Source: <{}>  \n", manifest.course_url));
    out.push_str(
        "License: CC BY-NC-SA 4.0, MIT OpenCourseWare  \n\
         Assembled by Wilkes from the course manifest; the prose below is the \
         course's own web pages, which OCW does not publish as documents.\n\n",
    );

    for page in &manifest.pages {
        out.push_str(&format!("## {}\n\n", page.title.trim()));
        out.push_str(page.markdown.trim());
        out.push_str("\n\n");
    }

    out.push_str("## Documents in this course\n\n");
    if manifest.files.is_empty() {
        out.push_str("This course publishes no documents; everything it holds is above.\n\n");
    } else {
        let mut current: Option<&str> = None;
        let mut started = false;
        for file in &manifest.files {
            let section = file.section.as_deref();
            if current != section || !started {
                started = true;
                current = section;
                out.push_str(&format!(
                    "\n### {}\n\n",
                    section.unwrap_or("Filed under no section")
                ));
            }
            out.push_str(&format!("- [{}]({})", file.title.trim(), file.filename));
            if !file.description.trim().is_empty() {
                out.push_str(&format!(" — {}", clip_words(&file.description, 60)));
            }
            out.push('\n');
        }
        out.push('\n');
    }

    if !manifest.skipped.is_empty() {
        out.push_str("## Not fetched\n\n");
        let mut counts: Vec<(String, usize)> = Vec::new();
        for skip in &manifest.skipped {
            match counts.iter_mut().find(|(reason, _)| *reason == skip.reason) {
                Some((_, n)) => *n += 1,
                None => counts.push((skip.reason.clone(), 1)),
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (reason, count) in counts {
            out.push_str(&format!("- {count} × {reason}\n"));
        }
        out.push('\n');
    }
    out
}

fn clip_words(text: &str, limit: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= limit {
        return words.join(" ");
    }
    format!("{}…", words[..limit].join(" "))
}

// ── HTML to Markdown ─────────────────────────────────────────────────────────

/// Converts the HTML subset OCW's page content uses into Markdown.
///
/// Deliberately narrow and hand-written rather than a parser dependency: the
/// input is machine-generated by one publisher and uses a dozen tags. Tags it
/// does not know are *unwrapped* — their text is kept and the markup dropped —
/// which is the only choice that cannot lose prose. Nothing here interprets a
/// tag it has not been told about.
pub fn html_to_markdown(html: &str) -> String {
    let mut out = String::new();
    let mut chars = html.chars().peekable();
    let mut list_stack: Vec<Option<usize>> = Vec::new();
    let mut tables: Vec<TableState> = Vec::new();
    let mut hrefs: Vec<String> = Vec::new();
    let mut skip_depth = 0usize;

    while let Some(c) = chars.next() {
        if c != '<' {
            if skip_depth == 0 {
                // Inside a table, a newline in the source is just formatting —
                // OCW writes `<th>\n\nActivities\n</th>` — and letting it
                // through would end the row that cell is in.
                if !tables.is_empty() && c.is_whitespace() {
                    out.push(' ');
                } else {
                    out.push(c);
                }
            }
            continue;
        }
        let mut tag = String::new();
        for c in chars.by_ref() {
            if c == '>' {
                break;
            }
            tag.push(c);
        }
        let tag = tag.trim();
        let closing = tag.starts_with('/');
        let body = tag.trim_start_matches('/');
        let name: String = body
            .chars()
            .take_while(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();

        match name.as_str() {
            "script" | "style" => {
                if closing {
                    skip_depth = skip_depth.saturating_sub(1);
                } else if !tag.ends_with('/') {
                    skip_depth += 1;
                }
            }
            _ if skip_depth > 0 => {}
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                // One level deeper than the page's own heading, so a page's
                // internal structure nests under it instead of competing.
                let level = name
                    .chars()
                    .nth(1)
                    .and_then(|d| d.to_digit(10))
                    .unwrap_or(3)
                    + 1;
                out.push_str("\n\n");
                if !closing {
                    out.push_str(&"#".repeat(level.min(6) as usize));
                    out.push(' ');
                }
            }
            "table" => {
                if closing {
                    tables.pop();
                } else {
                    tables.push(TableState::default());
                }
                out.push_str("\n\n");
            }
            "tr" if !tables.is_empty() => {
                let state = tables.last_mut().expect("inside a table");
                if closing {
                    out.push('|');
                    // Markdown needs the delimiter row before it will render
                    // any of it as a table, and only the first row knows how
                    // many columns there are.
                    if state.row == 0 && state.cells > 0 {
                        out.push_str("\n|");
                        for _ in 0..state.cells {
                            out.push_str(" --- |");
                        }
                    }
                    state.row += 1;
                } else {
                    state.cells = 0;
                    out.push('\n');
                }
            }
            "td" | "th" if !tables.is_empty() => {
                if closing {
                    out.push(' ');
                } else {
                    tables.last_mut().expect("inside a table").cells += 1;
                    out.push_str("| ");
                }
            }
            // A block break inside a cell would end the row that cell is in,
            // and OCW wraps every cell's text in a paragraph.
            "p" | "div" | "section" | "blockquote" | "br" | "hr" if !tables.is_empty() => {
                out.push(' ')
            }
            "p" | "div" | "section" | "tr" | "blockquote" => out.push_str("\n\n"),
            "br" | "hr" => out.push_str("  \n"),
            "ul" => {
                if closing {
                    list_stack.pop();
                    out.push_str("\n\n");
                } else {
                    list_stack.push(None);
                    out.push('\n');
                }
            }
            "ol" => {
                if closing {
                    list_stack.pop();
                    out.push_str("\n\n");
                } else {
                    list_stack.push(Some(1));
                    out.push('\n');
                }
            }
            "li" if !closing => {
                let depth = list_stack.len().saturating_sub(1);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
                match list_stack.last_mut() {
                    Some(Some(n)) => {
                        out.push_str(&format!("{n}. "));
                        *n += 1;
                    }
                    _ => out.push_str("- "),
                }
            }
            "strong" | "b" => out.push_str("**"),
            "em" | "i" => out.push('*'),
            "code" => out.push('`'),
            "a" => {
                if closing {
                    match hrefs.pop() {
                        Some(href) if !href.is_empty() => out.push_str(&format!("]({href})")),
                        // An anchor with no destination is a bookmark target;
                        // its text is prose and the brackets would be noise.
                        _ => out.push_str(""),
                    }
                } else {
                    let href = attribute(body, "href").unwrap_or_default();
                    if href.is_empty() {
                        hrefs.push(String::new());
                    } else {
                        out.push('[');
                        hrefs.push(absolute(&href));
                    }
                }
            }
            "img" => {
                let alt = attribute(body, "alt").unwrap_or_default();
                if !alt.trim().is_empty() {
                    out.push_str(&format!("({})", alt.trim()));
                }
            }
            // Unknown tag: unwrapped, never interpreted.
            _ => {}
        }
    }
    tidy(&decode_entities(&out))
}

/// Where a table has got to, so the delimiter row lands after the first row
/// and nowhere else.
#[derive(Default)]
struct TableState {
    row: usize,
    cells: usize,
}

/// One attribute's value out of a tag body, single or double quoted.
fn attribute(body: &str, name: &str) -> Option<String> {
    let lowered = body.to_ascii_lowercase();
    let at = lowered.find(&format!("{name}="))?;
    // Slicing at a byte offset `find` returned on the *same* string shape is
    // safe; the chars are then taken from the original so the value keeps its
    // case and any non-ASCII it holds.
    let rest: String = body
        .chars()
        .skip(lowered[..at].chars().count() + name.len() + 1)
        .collect();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return Some(rest.chars().take_while(|c| !c.is_whitespace()).collect());
    }
    Some(rest.chars().skip(1).take_while(|c| *c != quote).collect())
}

/// Page links are site-absolute; a reader opening the generated document
/// outside a browser needs the origin on them.
fn absolute(href: &str) -> String {
    if href.starts_with('/') {
        format!("{OCW_ORIGIN}{href}")
    } else {
        href.to_string()
    }
}

fn decode_entities(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        let entity: String = chars
            .clone()
            .take_while(|c| *c != ';' && c.is_ascii_alphanumeric() || *c == '#')
            .collect();
        let named = match entity.as_str() {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            "ndash" | "#8211" => Some('–'),
            "mdash" | "#8212" => Some('—'),
            "lsquo" | "#8216" => Some('\u{2018}'),
            "rsquo" | "#8217" => Some('\u{2019}'),
            "ldquo" | "#8220" => Some('\u{201c}'),
            "rdquo" | "#8221" => Some('\u{201d}'),
            "hellip" | "#8230" => Some('…'),
            "times" => Some('×'),
            "deg" => Some('°'),
            _ => None,
        };
        match named {
            Some(decoded) => {
                for _ in 0..entity.chars().count() + 1 {
                    chars.next();
                }
                out.push(decoded);
            }
            // An entity nobody listed is left exactly as it was written. It is
            // visible to a reader as `&copy;`, which is wrong but honest;
            // swallowing it would delete text.
            None => out.push('&'),
        }
    }
    out
}

/// Collapses the whitespace the tag walk leaves behind, without touching the
/// blank line that separates two blocks.
fn tidy(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut blanks = 0usize;
    for line in text.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        // A trailing double space is Markdown's hard break and is emitted
        // deliberately by `<br>`; restore it after the collapse.
        let collapsed = if line.ends_with("  ") && !collapsed.is_empty() {
            format!("{collapsed}  ")
        } else {
            collapsed
        };
        if collapsed.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
            lines.push(String::new());
        } else {
            blanks = 0;
            lines.push(collapsed);
        }
    }
    lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(json: serde_json::Value) -> RawEntry {
        serde_json::from_value(json).expect("entry")
    }

    fn classified(json: serde_json::Value) -> Result<CourseFile, SkippedResource> {
        let mut used = std::collections::HashSet::new();
        classify(&entry(json), OCW_ORIGIN, &HashMap::new(), &mut used)
    }

    #[test]
    fn a_course_url_from_another_host_is_refused() {
        // Every document URL is built by joining OCW's origin to a path the
        // response supplied, so accepting a foreign course URL would let one
        // host decide what is fetched from another.
        assert!(course_base("https://example.invalid/courses/x").is_err());
        assert!(course_base("http://ocw.mit.edu/courses/x").is_err());
        assert!(course_base("https://ocw.mit.edu/about/").is_err());
        assert_eq!(
            course_base("https://ocw.mit.edu/courses/18-03-spring-2010/").expect("ocw"),
            "https://ocw.mit.edu/courses/18-03-spring-2010"
        );
    }

    #[test]
    fn a_video_is_refused_even_though_it_states_no_type() {
        // The whole reason resourcetype decides this: a video's file_type is
        // the empty string, so a filter reading only the MIME would have kept
        // a 115 MB MP4 as an untyped document.
        let skip = classified(serde_json::json!({
            "title": "Session 12: Constrained Optimization",
            "file": "/courses/c/vid.mp4",
            "file_type": "",
            "resourcetype": "Video",
            "video_metadata": { "youtube_id": "PKbah48l3AU" }
        }))
        .expect_err("a video must be refused");
        assert_eq!(skip.reason, "audiovisual");
    }

    #[test]
    fn a_resource_with_no_stated_type_is_refused_rather_than_guessed() {
        // The extension says PDF. That is exactly the inference this must not
        // make: OCW types every document it holds, so an absence is the feed
        // changing shape and has to surface instead of being papered over.
        let skip = classified(serde_json::json!({
            "title": "Lecture 5",
            "file": "/courses/c/abc_lecture5.pdf",
            "resourcetype": "Document"
        }))
        .expect_err("an untyped resource must be refused");
        assert_eq!(skip.reason, "no file_type stated");
    }

    #[test]
    fn a_spreadsheet_is_refused_and_names_its_type() {
        let skip = classified(serde_json::json!({
            "title": "AER_Data.xls",
            "file": "/courses/c/abc_AER_Data.xls",
            "file_type": "application/vnd.ms-excel",
            "resourcetype": "Document"
        }))
        .expect_err("a spreadsheet is not a document we keep");
        assert_eq!(skip.reason, "not a PDF (application/vnd.ms-excel)");
    }

    #[test]
    fn a_pdf_keeps_its_section_and_loses_the_storage_prefix() {
        let file = classified(serde_json::json!({
            "title": "Lecture 1 notes",
            "file": "/courses/c/fda8db6bf38fc0b1c8ee2694027886f8_MIT11_165F11_ses01.pdf",
            "file_type": "application/pdf",
            "file_size": 214871,
            "parent_title": "Lecture Notes",
            "resourcetype": "Document"
        }))
        .expect("a pdf is kept");
        assert_eq!(file.filename, "MIT11_165F11_ses01.pdf");
        assert_eq!(
            file.url,
            format!(
                "{OCW_ORIGIN}/courses/c/fda8db6bf38fc0b1c8ee2694027886f8_MIT11_165F11_ses01.pdf"
            )
        );
        assert_eq!(file.section.as_deref(), Some("Lecture Notes"));
        assert_eq!(file.size_bytes, Some(214871));
    }

    #[test]
    fn file_size_is_read_whether_it_arrives_as_a_number_or_a_string() {
        // OCW states it both ways in the same manifest: a number for
        // documents, a decimal string for videos.
        let numeric = entry(serde_json::json!({ "file_size": 11104 }));
        let textual = entry(serde_json::json!({ "file_size": "115440479" }));
        assert_eq!(numeric.file_size, Some(11104));
        assert_eq!(textual.file_size, Some(115_440_479));
    }

    #[test]
    fn a_transcript_is_named_after_the_lecture_it_transcribes() {
        // Every transcript is titled "3play pdf file" and stored under a
        // YouTube id, so both the title and the filename say nothing. The
        // video carrying that id is the only thing that does.
        let lectures = HashMap::from([(
            "PKbah48l3AU".to_string(),
            "Session 12: Constrained Optimization".to_string(),
        )]);
        let mut used = std::collections::HashSet::new();
        let file = classify(
            &entry(serde_json::json!({
                "title": "3play pdf file",
                "file": "/courses/c/002f527b28ed600f79fff62fcdbc29b1_PKbah48l3AU.pdf",
                "file_type": "application/pdf",
                "resourcetype": "Document"
            })),
            OCW_ORIGIN,
            &lectures,
            &mut used,
        )
        .expect("a transcript is a document");
        assert_eq!(
            file.filename,
            "Session 12_ Constrained Optimization (transcript).pdf"
        );
    }

    #[test]
    fn an_unmatched_transcript_does_not_gain_a_second_extension() {
        // Its YouTube id names no video in this course — which happens when
        // the video's own manifest entry was one of the unreadable ones — so
        // the stored name is all there is to go on.
        let mut used = std::collections::HashSet::new();
        let file = classify(
            &entry(serde_json::json!({
                "title": "3play pdf file",
                "file": "/courses/c/002f527b28ed600f79fff62fcdbc29b1_42TkHA__6bk.pdf",
                "file_type": "application/pdf",
                "resourcetype": "Document"
            })),
            OCW_ORIGIN,
            &HashMap::new(),
            &mut used,
        )
        .expect("an unmatched transcript is still a document");
        assert_eq!(file.filename, "42TkHA__6bk (transcript).pdf");
    }

    #[test]
    fn two_documents_that_reduce_to_one_name_do_not_collide() {
        // download_to_root refuses same-name-different-content, so a course
        // holding both `lecture5.pdf` and `Lecture5.pdf` would fail partway
        // through rather than at the start.
        let mut used = std::collections::HashSet::new();
        assert_eq!(unique_filename("lecture5.pdf", &mut used), "lecture5.pdf");
        assert_eq!(
            unique_filename("Lecture5.pdf", &mut used),
            "Lecture5 (2).pdf"
        );
    }

    #[test]
    fn a_name_is_trimmed_by_characters_and_never_through_a_glyph() {
        let mut used = std::collections::HashSet::new();
        let long = format!("{}.pdf", "Ü".repeat(400));
        let name = unique_filename(&long, &mut used);
        assert_eq!(name.chars().filter(|c| *c == 'Ü').count(), 120);
        assert!(name.ends_with(".pdf"));
    }

    #[test]
    fn a_stored_name_without_the_hash_prefix_is_left_alone() {
        assert_eq!(strip_hash("lecture5.pdf"), "lecture5.pdf");
        assert_eq!(
            strip_hash("fda8db6bf38fc0b1c8ee2694027886f8_ses01.pdf"),
            "ses01.pdf"
        );
        // Thirty-two characters, but not hex: not a prefix, so not stripped.
        assert_eq!(
            strip_hash("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz_ses01.pdf"),
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz_ses01.pdf"
        );
    }

    #[test]
    fn page_html_becomes_markdown_a_reader_and_an_outline_can_both_use() {
        let markdown = html_to_markdown(
            "<h3 id=\"prereq\">Prerequisites</h3><p>Physics I (8.01) &amp; \
             <em>Calculus</em></p><ul><li>First</li><li>Second</li></ul>\
             <p>See the <a href=\"/courses/x/\">course</a>.</p>",
        );
        assert!(markdown.starts_with("#### Prerequisites"), "{markdown}");
        assert!(
            markdown.contains("Physics I (8.01) & *Calculus*"),
            "{markdown}"
        );
        assert!(markdown.contains("- First\n- Second"), "{markdown}");
        assert!(
            markdown.contains("[course](https://ocw.mit.edu/courses/x/)"),
            "{markdown}"
        );
    }

    #[test]
    fn a_table_survives_as_a_table_rather_than_one_cell_per_line() {
        // OCW writes its grading criteria and calendars as tables and wraps
        // every cell's text in a paragraph. Treating <tr> as a block break put
        // each cell on a line of its own with a bare pipe above it, which is
        // neither a table nor prose.
        // The markup OCW actually publishes: a <table> opened inside a <p>,
        // stray </p> and <p> interleaved between the rows, and a blank line in
        // every cell. A converter that only handled well-formed tables passed
        // its test and still produced one cell per line on the real thing.
        let markdown = html_to_markdown(
            "<p><table>\n\n<thead>\n\n<tr>\n\n<th>\n\nActivities\n</th>\n\n\
             <th>\n\npercentages\n</th>\n</p>\n</tr>\n\n<p></thead>\n\n<tr>\n\n\
             <td>\n\nProblem Sets\n</td>\n\n<td>\n\n30%\n</td>\n</p>\n<p></tr>\n\n\
             </table>",
        );
        assert!(
            markdown.contains("| Activities | percentages |"),
            "{markdown}"
        );
        assert!(markdown.contains("| --- | --- |"), "{markdown}");
        assert!(markdown.contains("| Problem Sets | 30% |"), "{markdown}");
        // Exactly one delimiter row, under the header and nowhere else.
        assert_eq!(markdown.matches("| --- |").count(), 1, "{markdown}");
    }

    #[test]
    fn an_unknown_tag_keeps_its_text_and_an_unknown_entity_keeps_its_own() {
        // Unwrapping is the only choice that cannot lose prose, and an entity
        // nobody listed is left visible rather than deleted.
        assert_eq!(
            html_to_markdown("<span class=\"x\">kept</span> &copy; 2011"),
            "kept &copy; 2011"
        );
        assert_eq!(html_to_markdown("<script>var a = 1;</script>gone"), "gone");
    }

    #[test]
    fn the_generated_document_carries_the_pages_and_indexes_the_documents() {
        let manifest = CourseManifest {
            course_url: "https://ocw.mit.edu/courses/c".into(),
            slug: "c".into(),
            title: "Water Quality Control".into(),
            description: "A course about water.".into(),
            pages: vec![
                CoursePage {
                    title: "Syllabus".into(),
                    markdown: "Meets twice weekly.".into(),
                },
                CoursePage {
                    title: "Readings".into(),
                    markdown: "Chapter 1.".into(),
                },
            ],
            files: vec![
                CourseFile {
                    filename: "chapter1lecture.pdf".into(),
                    url: "https://ocw.mit.edu/courses/c/chapter1lecture.pdf".into(),
                    section: Some("Lecture Notes".into()),
                    title: "Chapter 1".into(),
                    description: "Introduction.".into(),
                    size_bytes: Some(10),
                },
                CourseFile {
                    filename: "loose.pdf".into(),
                    url: "https://ocw.mit.edu/courses/c/loose.pdf".into(),
                    section: None,
                    title: "Loose sheet".into(),
                    description: String::new(),
                    size_bytes: None,
                },
            ],
            skipped: vec![
                SkippedResource {
                    title: "Lecture 1 video".into(),
                    reason: "audiovisual".into(),
                },
                SkippedResource {
                    title: "Lecture 2 video".into(),
                    reason: "audiovisual".into(),
                },
            ],
        };
        let document = course_document(&manifest);
        assert!(document.starts_with("# Water Quality Control"));
        // The pages are the whole point: this prose is in no PDF.
        assert!(document.contains("## Syllabus\n\nMeets twice weekly."));
        assert!(document.contains("## Readings\n\nChapter 1."));
        // Each document is linked by the name it was actually written under,
        // so the index works from inside the course directory.
        assert!(document.contains("### Lecture Notes"));
        assert!(document.contains("- [Chapter 1](chapter1lecture.pdf) — Introduction."));
        // A third of documents carry no section, and saying so beats filing
        // them somewhere plausible.
        assert!(document.contains("### Filed under no section"));
        // What was refused is stated, so a gap in the sequence has a reason.
        assert!(document.contains("- 2 × audiovisual"), "{document}");
    }

    #[tokio::test]
    async fn a_course_walk_separates_pages_from_documents_and_reports_as_it_goes() {
        let mut server = mockito::Server::new_async().await;
        let base = format!("{}/courses/1-77-water-quality-spring-2006", server.url());
        let path = "/courses/1-77-water-quality-spring-2006";

        let _course = server
            .mock("GET", format!("{path}/data.json").as_str())
            .with_status(200)
            .with_body(r#"{"course_title":"Water Quality","course_description":"About water."}"#)
            .create_async()
            .await;
        let _map = server
            .mock("GET", format!("{path}/content_map.json").as_str())
            .with_status(200)
            .with_body(format!(
                r#"{{"a":"{path}/pages/syllabus/data.json",
                     "b":"{path}/resources/lecture1/data.json",
                     "c":"{path}/resources/lecture1-video/data.json"}}"#
            ))
            .create_async()
            .await;
        let _page = server
            .mock("GET", format!("{path}/pages/syllabus/data.json").as_str())
            .with_status(200)
            .with_body(
                r#"{"title":"Syllabus","ocw_type":"CourseSection",
                    "content":"<p>Meets on Tuesdays.</p>"}"#,
            )
            .create_async()
            .await;
        let _pdf = server
            .mock(
                "GET",
                format!("{path}/resources/lecture1/data.json").as_str(),
            )
            .with_status(200)
            .with_body(format!(
                r#"{{"title":"Lecture 1","ocw_type":"OCWFile","resourcetype":"Document",
                     "file_type":"application/pdf","file_size":1234,
                     "parent_title":"Lecture Notes",
                     "file":"{path}/fda8db6bf38fc0b1c8ee2694027886f8_chapter1lecture.pdf"}}"#
            ))
            .create_async()
            .await;
        let _video = server
            .mock(
                "GET",
                format!("{path}/resources/lecture1-video/data.json").as_str(),
            )
            .with_status(200)
            .with_body(
                r#"{"title":"Lecture 1 video","ocw_type":"OCWFile","resourcetype":"Video",
                    "file_type":"","video_metadata":{"youtube_id":"abcdefghijk"}}"#,
            )
            .create_async()
            .await;

        let (tx, mut rx) = mpsc::channel(64);
        let http = ProviderHttpClient::new("test");
        let manifest = read_course_at(&http, &server.url(), &base, Some(&tx))
            .await
            .expect("walk");

        assert_eq!(manifest.slug, "1-77-water-quality-spring-2006");
        assert_eq!(manifest.title, "Water Quality");
        assert_eq!(manifest.pages.len(), 1);
        assert_eq!(manifest.pages[0].markdown, "Meets on Tuesdays.");
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].filename, "chapter1lecture.pdf");
        assert_eq!(
            manifest.skipped,
            vec![SkippedResource {
                title: "Lecture 1 video".into(),
                reason: "audiovisual".into()
            }]
        );

        // Reported during the walk, not after it: a caller must not have to
        // wait for the last entry to learn the first one landed.
        let first = rx.try_recv().expect("a manifest report");
        assert_eq!(first.stage, CatalogueCourseStage::Manifest);
        assert_eq!(first.total, Some(3));
    }

    #[tokio::test]
    async fn a_manifest_entry_naming_no_path_is_refused_before_anything_is_fetched() {
        let mut server = mockito::Server::new_async().await;
        let base = format!("{}/courses/c", server.url());
        let _course = server
            .mock("GET", "/courses/c/data.json")
            .with_status(200)
            .with_body(r#"{"course_title":"C"}"#)
            .create_async()
            .await;
        let _map = server
            .mock("GET", "/courses/c/content_map.json")
            .with_status(200)
            .with_body(r#"{"a":"","b":"/courses/c/resources/here/data.json"}"#)
            .create_async()
            .await;
        let _here = server
            .mock("GET", "/courses/c/resources/here/data.json")
            .with_status(200)
            .with_body(
                r#"{"title":"Here","ocw_type":"OCWFile","resourcetype":"Document",
                    "file_type":"application/pdf",
                    "file":"/courses/c/fda8db6bf38fc0b1c8ee2694027886f8_here.pdf"}"#,
            )
            .create_async()
            .await;
        // Anything asked of the origin's root would be this mock, and it must
        // never be reached: an empty manifest value is not a request.
        let root = server.mock("GET", "/").expect(0).create_async().await;

        let http = ProviderHttpClient::new("test");
        let manifest = read_course_at(&http, &server.url(), &base, None)
            .await
            .expect("walk");
        root.assert_async().await;
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.skipped.len(), 1);
        assert_eq!(
            manifest.skipped[0].reason,
            "manifest entry names no course path"
        );
    }

    #[tokio::test]
    async fn one_unreadable_entry_costs_that_entry_and_says_so() {
        let mut server = mockito::Server::new_async().await;
        let base = format!("{}/courses/c", server.url());
        let path = "/courses/c";
        let _course = server
            .mock("GET", format!("{path}/data.json").as_str())
            .with_status(200)
            .with_body(r#"{"course_title":"C"}"#)
            .create_async()
            .await;
        let _map = server
            .mock("GET", format!("{path}/content_map.json").as_str())
            .with_status(200)
            .with_body(format!(
                r#"{{"a":"{path}/resources/gone/data.json",
                     "b":"{path}/resources/here/data.json"}}"#
            ))
            .create_async()
            .await;
        let _gone = server
            .mock("GET", format!("{path}/resources/gone/data.json").as_str())
            .with_status(404)
            .create_async()
            .await;
        let _here = server
            .mock("GET", format!("{path}/resources/here/data.json").as_str())
            .with_status(200)
            .with_body(format!(
                r#"{{"title":"Here","ocw_type":"OCWFile","resourcetype":"Document",
                     "file_type":"application/pdf","file":"{path}/fda8db6bf38fc0b1c8ee2694027886f8_here.pdf"}}"#
            ))
            .create_async()
            .await;

        let http = ProviderHttpClient::new("test");
        let manifest = read_course_at(&http, &server.url(), &base, None)
            .await
            .expect("a course survives one bad entry");
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.skipped.len(), 1);
        assert!(
            manifest.skipped[0].reason.contains("unreadable"),
            "{:?}",
            manifest.skipped[0]
        );
    }
}
