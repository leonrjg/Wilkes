use std::path::{Path, PathBuf};

use mupdf::pdf::PdfDocument;
use mupdf::{Buffer, Document, Font, Matrix, SimpleFontEncoding, Size, TextPageFlags};

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
        for (baseline, words) in BASELINES
            .iter()
            .zip(["one alpha", "two beta", "three gamma"])
        {
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
        graphics_only: false,
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
        sibling: false,
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
        fonts: vec![],
        pages: vec![page(
            1,
            vec![kept(1, 0, 0.0), kept(1, 2, 20.0)],
            vec![
                typed(80.0, "Typed in place of it"),
                typed(100.0, "and a second line"),
            ],
        )],
    };
    write_edited_pdf(&source, &export, &destination).unwrap();

    let edited = lines(&destination, 0);
    assert!(
        !has(&edited, "two beta"),
        "replaced text survived: {edited:?}"
    );
    assert!(has(&edited, "Typed in place of it"));
    assert!(has(&edited, "and a second line"));
    let moved = top_of(&edited, "P1 line three gamma") - top_of(&original, "P1 line three gamma");
    assert!((moved - 20.0).abs() < 0.5, "line three moved by {moved}");
    let stayed = top_of(&edited, "P1 line one alpha") - top_of(&original, "P1 line one alpha");
    assert!(stayed.abs() < 0.5, "line one moved by {stayed}");
    // Each kept line is in the file once, not once per copy of its page.
    assert_eq!(
        edited
            .iter()
            .filter(|(words, _)| words.contains("one alpha"))
            .count(),
        1
    );
}

#[test]
fn text_in_a_font_sent_with_the_edits_is_set_in_that_font() {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture(directory.path(), 1);
    let destination = directory.path().join("edited.pdf");
    // The document's own font, as the reader sends it: any font file will do.
    let id = "own:ABCDEF+Sent-Regular".to_string();
    let file = fonts::load(&system_font()).unwrap().data;
    let export = EditExport {
        fonts: vec![ExportFont {
            id: id.clone(),
            data: STANDARD.encode(&file),
        }],
        pages: vec![page(
            1,
            vec![kept(1, 0, 0.0), kept(1, 2, 0.0)],
            vec![TextChunk {
                x: LEFT,
                baseline: 80.0,
                text: "Set in a sent font".to_string(),
                font: id,
                size: 12.0,
                color: [0, 0, 0],
            }],
        )],
    };
    write_edited_pdf(&source, &export, &destination).unwrap();

    assert!(has(&lines(&destination, 0), "Set in a sent font"));
    let saved = std::fs::read(&destination).unwrap();
    assert!(
        saved
            .windows(b"Sent-Regular".len())
            .any(|window| window == b"Sent-Regular"),
        "the sent font is not what the text was embedded in"
    );
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
        graphics_only: false,
    };
    let export = EditExport {
        fonts: vec![],
        pages: vec![
            page(1, vec![kept(1, 0, 0.0), kept(1, 1, 0.0)], vec![]),
            page(
                2,
                vec![
                    carried,
                    kept(2, 0, 20.0),
                    kept(2, 1, 20.0),
                    kept(2, 2, 20.0),
                ],
                vec![],
            ),
        ],
    };
    write_edited_pdf(&source, &export, &destination).unwrap();

    let first = lines(&destination, 0);
    assert!(has(&first, "P1 line two beta"));
    assert!(
        !has(&first, "three gamma"),
        "the carried line stayed behind: {first:?}"
    );

    let second = lines(&destination, 1);
    let carried_top = top_of(&second, "P1 line three gamma");
    assert!(carried_top < top_of(&second, "P2 line one alpha"));
    let original = lines(&source, 1);
    let moved = top_of(&second, "P2 line three gamma") - top_of(&original, "P2 line three gamma");
    assert!(
        (moved - 20.0).abs() < 0.5,
        "page 2's lines moved by {moved}"
    );
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
        graphics_only: false,
    };
    let export = EditExport {
        fonts: vec![],
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
fn what_a_paragraph_stood_on_is_kept_without_its_words() {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture(directory.path(), 1);
    let destination = directory.path().join("edited.pdf");

    // Line two is drawn again as paper only -- twice, as a paragraph that grew
    // by a line would have it -- and line three follows below.
    let paper = |dy: f32| Strip {
        graphics_only: true,
        ..kept(1, 1, dy)
    };
    let export = EditExport {
        fonts: vec![],
        pages: vec![page(
            1,
            vec![kept(1, 0, 0.0), paper(0.0), paper(20.0), kept(1, 2, 20.0)],
            vec![],
        )],
    };
    write_edited_pdf(&source, &export, &destination).unwrap();

    let edited = lines(&destination, 0);
    assert!(
        !has(&edited, "two beta"),
        "the words came along: {edited:?}"
    );
    assert!(has(&edited, "P1 line one alpha"));
    assert!(has(&edited, "P1 line three gamma"));
}

/// The colour of one point of a page, in page space.
fn colour_at(path: &Path, page: i32, x: f32, y: f32) -> [u8; 3] {
    let doc = Document::open(path.to_str().unwrap()).unwrap();
    let pixmap = doc
        .load_page(page)
        .unwrap()
        .to_pixmap(
            &Matrix::IDENTITY,
            &mupdf::Colorspace::device_rgb(),
            false,
            false,
        )
        .unwrap();
    let at = y as usize * pixmap.stride() as usize + x as usize * usize::from(pixmap.n());
    let samples = pixmap.samples();
    [samples[at], samples[at + 1], samples[at + 2]]
}

/// The fixture with `before` drawn under page 1's text and `forms` among its
/// XObjects.
fn fixture_over(directory: &Path, before: &[u8], forms: &[(&str, &str)]) -> PathBuf {
    let plain = fixture(directory, 1);
    let mut doc = PdfDocument::open(plain.to_str().unwrap()).unwrap();
    let mut object = doc.find_page(0).unwrap();
    let mut content = before.to_vec();
    content.extend(
        object
            .get_dict("Contents")
            .unwrap()
            .unwrap()
            .read_stream()
            .unwrap(),
    );
    let mut contents = doc.add_object(&doc.new_dict().unwrap()).unwrap();
    contents
        .write_stream_buffer(&Buffer::from_bytes(&content).unwrap())
        .unwrap();
    object.dict_put("Contents", contents).unwrap();

    let mut resources = object.get_dict("Resources").unwrap().unwrap();
    let mut xobjects = doc.new_dict().unwrap();
    for (name, stream) in forms {
        let dict = doc
            .new_object_from_str(&format!(
                "<< /Type /XObject /Subtype /Form /BBox [0 0 {WIDTH} {HEIGHT}] \
                 /Resources << /Font {} >> >>",
                "<< /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>"
            ))
            .unwrap();
        let mut form = doc.add_object(&dict).unwrap();
        form.write_stream_buffer(&Buffer::from_bytes(stream.as_bytes()).unwrap())
            .unwrap();
        xobjects.dict_put(*name, form).unwrap();
    }
    resources.dict_put("XObject", xobjects).unwrap();

    let path = directory.join("over.pdf");
    doc.save(path.to_str().unwrap()).unwrap();
    path
}

/// An export that redraws line two of page 1 as paper only, twice.
fn line_two_as_paper() -> EditExport {
    let paper = |dy: f32| Strip {
        graphics_only: true,
        ..kept(1, 1, dy)
    };
    EditExport {
        fonts: vec![],
        pages: vec![page(
            1,
            vec![kept(1, 0, 0.0), paper(0.0), paper(20.0), kept(1, 2, 20.0)],
            vec![],
        )],
    }
}

#[test]
fn an_image_under_a_paragraph_is_still_there_without_its_words() {
    let directory = tempfile::tempdir().unwrap();
    // One red pixel, stretched over the whole of line two.
    let area = line_area(1);
    let mut image = format!(
        "q {} 0 0 {} {} {} cm BI /W 1 /H 1 /BPC 8 /CS /RGB ID ",
        area.width,
        area.height,
        area.x,
        HEIGHT - area.bottom()
    )
    .into_bytes();
    image.extend_from_slice(&[200, 0, 0]);
    image.extend_from_slice(b" EI Q\n");
    let source = fixture_over(directory.path(), &image, &[]);
    let destination = directory.path().join("edited.pdf");
    write_edited_pdf(&source, &line_two_as_paper(), &destination).unwrap();

    assert!(!has(&lines(&destination, 0), "two beta"));
    // Where the line was, and where the paragraph's added line is: both stand
    // on the image, away from where any letter was.
    for dy in [0.0, 20.0] {
        let [red, green, _] = colour_at(&destination, 0, area.right() - 4.0, area.y + 6.0 + dy);
        assert!(red > 150 && green < 60, "no image at +{dy}: {red} {green}");
    }
}

#[test]
fn words_drawn_by_a_form_are_taken_out_with_the_rest() {
    let directory = tempfile::tempdir().unwrap();
    // Line two's place also holds words that a form draws, with a rule.
    let area = line_area(1);
    let form = format!(
        "BT /F1 12 Tf {} {} Td (inside a form) Tj ET {} {} 60 1 re f",
        LEFT + 110.0,
        HEIGHT - BASELINES[1],
        LEFT,
        HEIGHT - area.bottom()
    );
    let source = fixture_over(directory.path(), b"/Fm1 Do\n", &[("Fm1", form.as_str())]);
    assert!(has(&lines(&source, 0), "inside a form"));
    let destination = directory.path().join("edited.pdf");
    write_edited_pdf(&source, &line_two_as_paper(), &destination).unwrap();

    let edited = lines(&destination, 0);
    assert!(
        !has(&edited, "inside a form"),
        "the form kept its words: {edited:?}"
    );
    // The rule the form draws is not text, and stays.
    let [red, ..] = colour_at(&destination, 0, LEFT + 30.0, area.bottom() - 0.5);
    assert!(red < 100, "the form's rule is gone");
}

#[test]
fn a_form_the_page_lists_and_does_not_draw_is_not_copied() {
    let directory = tempfile::tempdir().unwrap();
    // Some producers give every page one list of every page's content. A page
    // that draws one form of two must not take a copy of the other with it.
    let words = |text: &str| format!("BT /F1 12 Tf 80 400 Td ({text}) Tj ET");
    let source = fixture_over(
        directory.path(),
        b"/Fm1 Do\n",
        &[
            ("Fm1", words("drawn here").as_str()),
            ("Fm2", words("another page entirely").as_str()),
        ],
    );
    let destination = directory.path().join("edited.pdf");
    write_edited_pdf(&source, &line_two_as_paper(), &destination).unwrap();

    let saved = PdfDocument::open(destination.to_str().unwrap()).unwrap();
    // Forms are told from the page's own content by the `Td` only they use.
    let contains =
        |content: &[u8], text: &[u8]| content.windows(text.len()).any(|window| window == text);
    let forms: Vec<Vec<u8>> = (1..saved.count_objects().unwrap() as i32)
        .filter_map(|number| saved.new_indirect(number, 0).ok()?.read_stream().ok())
        .filter(|content| contains(content, b"80 400 Td"))
        .collect();
    // One copy was made without its words: of the form the page draws.
    let wordless = forms
        .iter()
        .filter(|content| !contains(content, b"Tj"))
        .count();
    assert_eq!(wordless, 1);
    assert!(forms
        .iter()
        .any(|content| contains(content, b"another page entirely")));
}

/// The fixture with a link over each of page 1's second and third lines.
fn fixture_with_links(directory: &Path) -> PathBuf {
    let plain = fixture(directory, 1);
    let mut doc = PdfDocument::open(plain.to_str().unwrap()).unwrap();
    let mut annotations = doc.new_array().unwrap();
    for line in [1, 2] {
        let area = line_area(line);
        // User space is y-up: the area's bottom edge is the lower y.
        let link = doc
            .new_object_from_str(&format!(
                "<< /Type /Annot /Subtype /Link /Rect [{} {} {} {}] /Border [0 0 0] \
                 /A << /S /URI /URI (https://example.org/{line}) >> >>",
                area.x,
                HEIGHT - area.bottom(),
                area.right(),
                HEIGHT - area.y
            ))
            .unwrap();
        annotations
            .array_push(doc.add_object(&link).unwrap())
            .unwrap();
    }
    doc.find_page(0)
        .unwrap()
        .dict_put("Annots", annotations)
        .unwrap();
    let path = directory.join("linked.pdf");
    doc.save(path.to_str().unwrap()).unwrap();
    path
}

/// Each link on a page: where it points, and the top of its area in page space.
fn links(path: &Path, page: i32) -> Vec<(String, f32)> {
    let doc = PdfDocument::open(path.to_str().unwrap()).unwrap();
    let object = doc.find_page(page).unwrap();
    let Some(annotations) = object.get_dict("Annots").unwrap() else {
        return Vec::new();
    };
    (0..annotations.len().unwrap())
        .map(|index| {
            let annotation = annotations.get_array(index as i32).unwrap().unwrap();
            let rect = annotation.get_dict("Rect").unwrap().unwrap();
            let upper = rect.get_array(3).unwrap().unwrap().as_float().unwrap();
            let uri = annotation
                .get_dict("A")
                .unwrap()
                .unwrap()
                .get_dict("URI")
                .unwrap()
                .unwrap();
            (uri.as_string().unwrap().to_string(), HEIGHT - upper)
        })
        .collect()
}

#[test]
fn a_link_follows_the_line_it_is_on_and_goes_with_text_that_was_replaced() {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture_with_links(directory.path());
    let destination = directory.path().join("edited.pdf");
    assert_eq!(links(&source, 0).len(), 2);

    // Line two is replaced; line three moves down by 20.
    let export = EditExport {
        fonts: vec![],
        pages: vec![page(1, vec![kept(1, 0, 0.0), kept(1, 2, 20.0)], vec![])],
    };
    write_edited_pdf(&source, &export, &destination).unwrap();

    let after = links(&destination, 0);
    assert_eq!(after.len(), 1, "{after:?}");
    assert_eq!(after[0].0, "https://example.org/2");
    assert!(
        (after[0].1 - (line_area(2).y + 20.0)).abs() < 0.01,
        "{after:?}"
    );
}

#[test]
fn a_link_goes_to_the_next_page_with_its_line() {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture_with_links(directory.path());
    let destination = directory.path().join("edited.pdf");
    let carried = Strip {
        source: 1,
        rect: line_area(2),
        x: LEFT,
        y: line_area(0).y,
        graphics_only: false,
    };
    let export = EditExport {
        fonts: vec![],
        pages: vec![
            page(1, vec![kept(1, 0, 0.0), kept(1, 1, 0.0)], vec![]),
            page(2, vec![carried], vec![]),
        ],
    };
    write_edited_pdf(&source, &export, &destination).unwrap();

    assert_eq!(
        links(&destination, 0)
            .iter()
            .map(|link| link.0.as_str())
            .collect::<Vec<_>>(),
        ["https://example.org/1"]
    );
    let carried = links(&destination, 1);
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0].0, "https://example.org/2");
    assert!((carried[0].1 - line_area(0).y).abs() < 0.01, "{carried:?}");
}

/// A two-page fixture with an outline entry and a named destination that
/// both point at the top of page 1's third line.
fn fixture_with_destinations(directory: &Path) -> PathBuf {
    let plain = fixture(directory, 2);
    let mut doc = PdfDocument::open(plain.to_str().unwrap()).unwrap();
    let page = doc.find_page(0).unwrap().as_indirect().unwrap();
    let top = HEIGHT - line_area(2).y;
    let entry = doc
        .add_object(
            &doc.new_object_from_str(&format!(
                "<< /Title (Line three) /Dest [{page} 0 R /XYZ {LEFT} {top} null] >>"
            ))
            .unwrap(),
        )
        .unwrap();
    let mut outlines = doc.new_dict().unwrap();
    outlines
        .dict_put("First", entry.try_clone().unwrap())
        .unwrap();
    outlines.dict_put("Last", entry).unwrap();
    let outlines = doc.add_object(&outlines).unwrap();
    let names = doc
        .new_object_from_str(&format!(
            "<< /Dests << /Names [(three) << /D [{page} 0 R /FitH {top}] >>] >> >>"
        ))
        .unwrap();
    let mut catalog = doc.catalog().unwrap();
    catalog.dict_put("Outlines", outlines).unwrap();
    catalog.dict_put("Names", names).unwrap();
    let path = directory.join("destinations.pdf");
    doc.save(path.to_str().unwrap()).unwrap();
    path
}

/// The page (0-based) and top an outline's first entry and the named
/// destination `three` point at, in user space.
fn destinations(path: &Path) -> [(i32, f32); 2] {
    let doc = PdfDocument::open(path.to_str().unwrap()).unwrap();
    let page_of = |reference: PdfObject| {
        let number = reference.as_indirect().unwrap();
        (0..doc.page_count().unwrap())
            .find(|index| doc.find_page(*index).unwrap().as_indirect().unwrap() == number)
            .unwrap()
    };
    let catalog = doc.catalog().unwrap();
    let get = |object: &PdfObject, key: &str| object.get_dict(key).unwrap().unwrap();
    let outline = get(&get(&get(&catalog, "Outlines"), "First"), "Dest");
    let named = get(
        &get(&get(&catalog, "Names"), "Dests")
            .get_dict("Names")
            .unwrap()
            .unwrap()
            .get_array(1)
            .unwrap()
            .unwrap(),
        "D",
    );
    let at = |array: &PdfObject, index: i32| array.get_array(index).unwrap().unwrap();
    [
        (
            page_of(at(&outline, 0)),
            at(&outline, 3).as_float().unwrap(),
        ),
        (page_of(at(&named, 0)), at(&named, 2).as_float().unwrap()),
    ]
}

#[test]
fn a_destination_follows_what_it_pointed_at_down_the_page() {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture_with_destinations(directory.path());
    let destination = directory.path().join("edited.pdf");
    let export = EditExport {
        fonts: vec![],
        pages: vec![page(
            1,
            vec![kept(1, 0, 0.0), kept(1, 1, 0.0), kept(1, 2, 20.0)],
            vec![],
        )],
    };
    write_edited_pdf(&source, &export, &destination).unwrap();

    let was = HEIGHT - line_area(2).y;
    assert_eq!(destinations(&source), [(0, was), (0, was)]);
    // Twenty lower on the page is twenty less in a space that counts upward.
    assert_eq!(
        destinations(&destination),
        [(0, was - 20.0), (0, was - 20.0)]
    );
}

#[test]
fn a_destination_follows_what_it_pointed_at_onto_the_next_page() {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture_with_destinations(directory.path());
    let destination = directory.path().join("edited.pdf");
    // Line three of page 1 is now the first line of page 2.
    let moved = Strip {
        y: line_area(0).y,
        ..kept(1, 2, 0.0)
    };
    let export = EditExport {
        fonts: vec![],
        pages: vec![
            page(1, vec![kept(1, 0, 0.0), kept(1, 1, 0.0)], vec![]),
            page(
                2,
                vec![moved, kept(2, 0, 20.0), kept(2, 1, 20.0), kept(2, 2, 20.0)],
                vec![],
            ),
        ],
    };
    write_edited_pdf(&source, &export, &destination).unwrap();

    let top = HEIGHT - line_area(0).y;
    assert_eq!(destinations(&destination), [(1, top), (1, top)]);
}

#[test]
fn a_page_the_edits_did_not_reach_is_left_as_it_was() {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture(directory.path(), 2);
    let destination = directory.path().join("edited.pdf");
    let export = EditExport {
        fonts: vec![],
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
        fonts: vec![],
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
        fonts: vec![],
        pages: vec![page(1, vec![kept(4, 0, 0.0)], vec![])],
    };
    assert!(write_edited_pdf(&source, &export, &destination).is_err());
    // Appending page 3 to a one-page document would leave a hole at page 2.
    let export = EditExport {
        fonts: vec![],
        pages: vec![page(3, vec![kept(1, 0, 0.0)], vec![])],
    };
    assert!(write_edited_pdf(&source, &export, &destination).is_err());
}
