use std::path::{Path, PathBuf};

use mupdf::pdf::PdfDocument;
use mupdf::{Buffer, Document, Font, SimpleFontEncoding, Size, TextPageFlags};

use super::fonts::{self, FontRequest, GenericFamily};
use super::*;

const WIDTH: f32 = 400.0;
const HEIGHT: f32 = 300.0;
const LEFT: f32 = 50.0;
/// Baselines of the three lines every fixture page has, in page space.
const BASELINES: [f32; 3] = [60.0, 80.0, 100.0];

/// A document of `pages` pages, each with three lines that name themselves:
/// `P1 line one alpha`, `P1 line two beta`, `P1 line three gamma`.
fn fixture(directory: &Path, pages: u32) -> PathBuf {
    let mut doc = PdfDocument::new();
    let font = doc
        .add_simple_font(&Font::new("Helvetica").unwrap(), SimpleFontEncoding::Latin)
        .unwrap();
    for page in 1..=pages {
        let mut object = doc.new_page(Size::new(WIDTH, HEIGHT)).unwrap().object();
        let mut fonts = doc.new_dict().unwrap();
        fonts.dict_put("F1", font.try_clone().unwrap()).unwrap();
        let mut resources = doc.new_dict().unwrap();
        resources.dict_put("Font", fonts).unwrap();
        object.dict_put("Resources", resources).unwrap();

        let mut stream = String::new();
        for (baseline, words) in BASELINES.iter().zip(["one alpha", "two beta", "three gamma"]) {
            stream.push_str(&format!(
                "BT /F1 12 Tf {LEFT} {} Td (P{page} line {words}) Tj ET\n",
                HEIGHT - baseline
            ));
        }
        let mut contents = doc.add_object(&doc.new_dict().unwrap()).unwrap();
        contents
            .write_stream_buffer(&Buffer::from_bytes(stream.as_bytes()).unwrap())
            .unwrap();
        object.dict_put("Contents", contents).unwrap();
    }
    let path = directory.join("source.pdf");
    doc.save(path.to_str().unwrap()).unwrap();
    path
}

/// The ink of a fixture line, as the reader would have measured it: from a
/// little above the capitals to a little below the baseline.
fn line_area(line: usize) -> Area {
    Area {
        x: LEFT,
        y: BASELINES[line] - 9.0,
        width: 200.0,
        height: 12.0,
    }
}

fn kept(source: u32, line: usize, dy: f32) -> Strip {
    let rect = line_area(line);
    Strip {
        source,
        rect,
        x: rect.x,
        y: rect.y + dy,
    }
}

/// Every line of text on a page, with the top of its box.
fn lines(path: &Path, page: i32) -> Vec<(String, f32)> {
    let doc = Document::open(path.to_str().unwrap()).unwrap();
    let text = doc
        .load_page(page)
        .unwrap()
        .to_text_page(TextPageFlags::empty())
        .unwrap();
    let mut lines = Vec::new();
    for block in text.blocks() {
        for line in block.lines() {
            let words: String = line.chars().filter_map(|c| c.char()).collect();
            lines.push((words.trim().to_string(), line.bounds().y0));
        }
    }
    lines
}

fn top_of(lines: &[(String, f32)], text: &str) -> f32 {
    lines
        .iter()
        .find(|(words, _)| words == text)
        .unwrap_or_else(|| panic!("no line {text:?} among {lines:?}"))
        .1
}

fn has(lines: &[(String, f32)], text: &str) -> bool {
    lines.iter().any(|(words, _)| words.contains(text))
}

fn page(number: u32, strips: Vec<Strip>, text: Vec<TextChunk>) -> ExportPage {
    ExportPage {
        page: number,
        width: WIDTH,
        height: HEIGHT,
        strips,
        text,
    }
}

fn system_font() -> String {
    fonts::resolve(&FontRequest {
        name: String::new(),
        bold: false,
        italic: false,
        generic: GenericFamily::SansSerif,
    })
    .expect("a system has a sans-serif font")
    .id
}

#[test]
fn text_that_was_replaced_is_gone_and_what_followed_it_moved() {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture(directory.path(), 1);
    let destination = directory.path().join("edited.pdf");
    let original = lines(&source, 0);

    // Line two was rewritten as two lines, so line three is one line lower.
    let typed = |baseline: f32, text: &str| TextChunk {
        x: LEFT,
        baseline,
        text: text.to_string(),
        font: system_font(),
        size: 12.0,
        color: [0, 0, 0],
    };
    let export = EditExport {
        pages: vec![page(
            1,
            vec![kept(1, 0, 0.0), kept(1, 2, 20.0)],
            vec![typed(80.0, "Typed in place of it"), typed(100.0, "and a second line")],
        )],
    };
    write_edited_pdf(&source, &export, &destination).unwrap();

    let edited = lines(&destination, 0);
    assert!(!has(&edited, "two beta"), "replaced text survived: {edited:?}");
    assert!(has(&edited, "Typed in place of it"));
    assert!(has(&edited, "and a second line"));
    let moved = top_of(&edited, "P1 line three gamma") - top_of(&original, "P1 line three gamma");
    assert!((moved - 20.0).abs() < 0.5, "line three moved by {moved}");
    let stayed = top_of(&edited, "P1 line one alpha") - top_of(&original, "P1 line one alpha");
    assert!(stayed.abs() < 0.5, "line one moved by {stayed}");
    // Each kept line is in the file once, not once per copy of its page.
    assert_eq!(edited.iter().filter(|(words, _)| words.contains("one alpha")).count(), 1);
}

#[test]
fn a_line_pushed_off_a_page_arrives_on_the_next_and_leaves_the_first() {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture(directory.path(), 2);
    let destination = directory.path().join("edited.pdf");

    // Page 1 keeps two lines. Its third opens page 2, whose own lines each
    // move down a line to make room.
    let first_line_top = line_area(0).y;
    let carried = Strip {
        source: 1,
        rect: line_area(2),
        x: LEFT,
        y: first_line_top,
    };
    let export = EditExport {
        pages: vec![
            page(1, vec![kept(1, 0, 0.0), kept(1, 1, 0.0)], vec![]),
            page(
                2,
                vec![carried, kept(2, 0, 20.0), kept(2, 1, 20.0), kept(2, 2, 20.0)],
                vec![],
            ),
        ],
    };
    write_edited_pdf(&source, &export, &destination).unwrap();

    let first = lines(&destination, 0);
    assert!(has(&first, "P1 line two beta"));
    assert!(!has(&first, "three gamma"), "the carried line stayed behind: {first:?}");

    let second = lines(&destination, 1);
    let carried_top = top_of(&second, "P1 line three gamma");
    assert!(carried_top < top_of(&second, "P2 line one alpha"));
    let original = lines(&source, 1);
    let moved = top_of(&second, "P2 line three gamma") - top_of(&original, "P2 line three gamma");
    assert!((moved - 20.0).abs() < 0.5, "page 2's lines moved by {moved}");
}

#[test]
fn content_past_the_last_page_gets_a_page_of_its_own() {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture(directory.path(), 1);
    let destination = directory.path().join("edited.pdf");

    let carried = Strip {
        source: 1,
        rect: line_area(2),
        x: LEFT,
        y: line_area(0).y,
    };
    let export = EditExport {
        pages: vec![
            page(1, vec![kept(1, 0, 0.0), kept(1, 1, 0.0)], vec![]),
            page(2, vec![carried], vec![]),
        ],
    };
    write_edited_pdf(&source, &export, &destination).unwrap();

    let doc = Document::open(destination.to_str().unwrap()).unwrap();
    assert_eq!(doc.page_count().unwrap(), 2);
    let appended = lines(&destination, 1);
    assert_eq!(appended.len(), 1);
    assert!(has(&appended, "P1 line three gamma"));
}

#[test]
fn a_page_the_edits_did_not_reach_is_left_as_it_was() {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture(directory.path(), 2);
    let destination = directory.path().join("edited.pdf");
    let export = EditExport {
        pages: vec![page(1, vec![kept(1, 0, 0.0)], vec![])],
    };
    write_edited_pdf(&source, &export, &destination).unwrap();
    assert_eq!(lines(&destination, 1), lines(&source, 1));
}

#[test]
fn the_source_is_never_the_destination() {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture(directory.path(), 1);
    let before = std::fs::read(&source).unwrap();
    let export = EditExport {
        pages: vec![page(1, vec![kept(1, 0, 0.0)], vec![])],
    };
    let error = write_edited_pdf(&source, &export, &source).unwrap_err();
    assert!(error.to_string().contains("refusing"), "{error}");
    assert_eq!(std::fs::read(&source).unwrap(), before);
}

#[test]
fn an_export_that_names_a_page_the_document_lacks_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture(directory.path(), 1);
    let destination = directory.path().join("edited.pdf");
    let export = EditExport {
        pages: vec![page(1, vec![kept(4, 0, 0.0)], vec![])],
    };
    assert!(write_edited_pdf(&source, &export, &destination).is_err());
    // Appending page 3 to a one-page document would leave a hole at page 2.
    let export = EditExport {
        pages: vec![page(3, vec![kept(1, 0, 0.0)], vec![])],
    };
    assert!(write_edited_pdf(&source, &export, &destination).is_err());
}
