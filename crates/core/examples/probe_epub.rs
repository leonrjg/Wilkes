//! Ask what MuPDF actually gives us for a reflowable book.
//!
//!     cargo run --release --example probe_epub -- <file>...
//!
//! Answers, per file, the questions that decide whether EPUB support can be
//! an enabling rather than an implementation:
//!
//!   1. Does it open at all, and does it declare itself reflowable?
//!   2. What does `layout()` pin, and does page count depend on it?
//!   3. Is the extracted text prose or noise?
//!   4. Does the nav document survive as an outline, and do links survive?
//!   5. Can it be rendered to a surrogate PDF, at what size and cost?
//!   6. Does the surrogate agree with the original about where words are?
//!
//! (6) is the one that matters most: the index stores page-and-bbox locators
//! taken from the laid-out original, and the reader would draw them over the
//! surrogate. If the two disagree about geometry, every highlight in every
//! book is wrong and nothing else here is worth building.
//!
//! Drives one backend directly in this process, which is what an example is
//! for — see the boundary clause in `AGENTS.md`. Not precedent for `src/`.

use std::time::Instant;

use mupdf::{Document, MetadataName, TextPageFlags};

/// The layout this probe pins. Real support would put these three numbers in
/// the extraction recipe, because every page number and bbox in the index
/// descends from them and a silent change would move every locator.
const LAYOUT_WIDTH: f32 = 450.0;
const LAYOUT_HEIGHT: f32 = 600.0;
const LAYOUT_EM: f32 = 11.0;

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        eprintln!("usage: probe_epub <file>...");
        std::process::exit(2);
    }
    for path in &files {
        println!("\n═══ {path}");
        if let Err(error) = probe(path) {
            println!("  FAILED: {error}");
        }
    }
}

fn probe(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    palmdb_report(path);
    wilkes_path(path);
    let opened = Instant::now();
    let mut doc = Document::open(path)?;
    println!("  opened in {:?}", opened.elapsed());
    println!(
        "  is_pdf={}  reflowable={}",
        doc.is_pdf(),
        doc.is_reflowable()?
    );

    for (label, name) in [
        ("title", MetadataName::Title),
        ("author", MetadataName::Author),
        ("format", MetadataName::Format),
    ] {
        match doc.metadata(name) {
            Ok(value) if !value.is_empty() => println!("  {label}: {value}"),
            Ok(_) => println!("  {label}: <empty>"),
            Err(error) => println!("  {label}: <error: {error}>"),
        }
    }

    // Page count before any explicit layout: whatever mupdf's defaults are.
    // If this differs from the count after `layout()`, then relying on the
    // default would mean a mupdf version bump silently repaginating the book.
    let before = doc.page_count()?;
    let laid_out = Instant::now();
    doc.layout(LAYOUT_WIDTH, LAYOUT_HEIGHT, LAYOUT_EM)?;
    let after = doc.page_count()?;
    println!(
        "  pages: {before} with mupdf defaults, {after} at {LAYOUT_WIDTH}x{LAYOUT_HEIGHT} em {LAYOUT_EM} (laid out in {:?})",
        laid_out.elapsed()
    );

    // Pages to actually visit. `PROBE_SAMPLE=n` spreads n probes across the
    // document instead of walking it, which is the difference between a
    // question answered in a second and a 238 MB scan.
    let sample: Option<i32> = std::env::var("PROBE_SAMPLE")
        .ok()
        .and_then(|value| value.parse().ok());
    let visited: Vec<i32> = match sample {
        Some(n) if n > 0 && n < after => (0..n).map(|i| i * (after / n)).collect(),
        _ => (0..after).collect(),
    };
    println!("  visiting {} of {after} pages", visited.len());

    // ── Text ────────────────────────────────────────────────────────────────
    let mut total_chars = 0usize;
    let mut sample: Option<String> = None;
    let text_started = Instant::now();
    for &index in &visited {
        let page = doc.load_page(index)?;
        let text_page = page.to_text_page(TextPageFlags::empty())?;
        let mut page_text = String::new();
        for block in text_page.blocks() {
            for line in block.lines() {
                for character in line.chars() {
                    if let Some(c) = character.char() {
                        page_text.push(c);
                    }
                }
                page_text.push('\n');
            }
        }
        total_chars += page_text.chars().count();
        // First page with real prose on it, for eyeballing.
        if sample.is_none() && page_text.chars().filter(|c| c.is_alphabetic()).count() > 400 {
            sample = Some(page_text);
        }
    }
    println!(
        "  text: {total_chars} chars over {} pages ({:.0} chars/page) in {:?}",
        visited.len(),
        total_chars as f64 / visited.len().max(1) as f64,
        text_started.elapsed()
    );
    match &sample {
        Some(text) => {
            let excerpt: String = text.chars().take(300).collect();
            println!("  sample:\n    {}", excerpt.replace('\n', "\n    "));
        }
        None => println!("  sample: NO PAGE HELD 400+ LETTERS — text extraction is suspect"),
    }

    // ── Outline (the EPUB nav document) ─────────────────────────────────────
    let outlines = doc.outlines()?;
    let (entries, depth) = count_outline(&outlines, 1);
    println!("  outline: {entries} entries, depth {depth}");
    for entry in outlines.iter().take(5) {
        println!(
            "    - {:?}  uri={:?}  dest_page={:?}",
            entry.title,
            entry.uri,
            entry.dest.as_ref().map(|d| d.loc.page_number)
        );
    }

    // ── Links ───────────────────────────────────────────────────────────────
    let mut link_count = 0usize;
    let mut pages_with_links = 0usize;
    let mut first_link: Option<String> = None;
    for &index in &visited {
        let page = doc.load_page(index)?;
        let mut on_this_page = 0usize;
        for link in page.links()? {
            on_this_page += 1;
            if first_link.is_none() {
                first_link = Some(format!("p{index} -> {}", link.uri));
            }
        }
        if on_this_page > 0 {
            pages_with_links += 1;
        }
        link_count += on_this_page;
    }
    println!(
        "  links: {link_count} across {pages_with_links} visited pages; first: {first_link:?}"
    );
    if sample.is_some() {
        println!("  (sampled: stopping before the surrogate)");
        return Ok(());
    }

    // ── Surrogate PDF ───────────────────────────────────────────────────────
    // Bounded, because the whole-book conversion produced 365 MB for 198 pages
    // and 9.5 GB for 442 — the question is whether that is the conversion or
    // the write options, and a subset separates them.
    let span: i32 = std::env::var("PROBE_PAGES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(24);
    let last = (span - 1).min(after - 1);
    // `convert_to_pdf` puts the whole document on every page for a reflowable
    // source, so the surrogate is written the way `mutool convert` writes one:
    // a page at a time, replaying each laid-out page onto the writer's device.
    let out = std::env::temp_dir().join("wilkes-probe-surrogate.pdf");
    let convert_started = Instant::now();
    {
        let mut writer = mupdf::DocumentWriter::new(
            out.to_str().unwrap(),
            "pdf",
            "compress,compress-fonts,compress-images,garbage=4,sanitize",
        )?;
        for index in 0..=last {
            let page = doc.load_page(index)?;
            let bounds = page.bounds()?;
            let device = writer.begin_page(bounds)?;
            page.run(&device, &mupdf::Matrix::IDENTITY)?;
            writer.end_page(device)?;
        }
    }
    let size = std::fs::metadata(&out)?.len();
    println!(
        "  surrogate: pages 0..={last}, {size} bytes ({:.1} KB/page), written in {:?}",
        size as f64 / 1024.0 / (last + 1) as f64,
        convert_started.elapsed()
    );

    // ── Geometry coherence ──────────────────────────────────────────────────
    // Open the surrogate as a document in its own right and ask both copies
    // the same question: where is this word? Same page, same rectangle, or the
    // highlights the index stores cannot be drawn over the surrogate.
    let surrogate = Document::open(out.to_str().unwrap())?;
    println!(
        "  surrogate pages: {}, outline entries: {}",
        surrogate.page_count()?,
        count_outline(&surrogate.outlines()?, 1).0
    );

    // The decisive comparison, over every page rather than a sample: the index
    // stores page-and-bbox locators taken from the original, and the reader
    // draws them over the surrogate. Both must agree about page bounds and
    // about the text on each page, or every highlight in every book is wrong.
    let mut same_text = 0usize;
    let mut same_bounds = 0usize;
    let mut first_text_mismatch: Option<i32> = None;
    for index in 0..=last {
        let original = page_text(&doc, index)?;
        let copy = page_text(&surrogate, index)?;
        if original.trim() == copy.trim() {
            same_text += 1;
        } else if first_text_mismatch.is_none() {
            first_text_mismatch = Some(index);
            let a: Vec<char> = original.trim().chars().collect();
            let b: Vec<char> = copy.trim().chars().collect();
            let at = a
                .iter()
                .zip(b.iter())
                .position(|(x, y)| x != y)
                .unwrap_or(a.len().min(b.len()));
            println!(
                "      p{index}: lengths {} vs {}, first difference at char {at}",
                a.len(),
                b.len()
            );
            println!(
                "        original:  {:?}",
                a.iter()
                    .skip(at.saturating_sub(30))
                    .take(60)
                    .collect::<String>()
            );
            println!(
                "        surrogate: {:?}",
                b.iter()
                    .skip(at.saturating_sub(30))
                    .take(60)
                    .collect::<String>()
            );
        }
        let ob = doc.load_page(index)?.bounds()?;
        let cb = surrogate.load_page(index)?.bounds()?;
        if (ob.x1 - ob.x0 - (cb.x1 - cb.x0)).abs() < 0.01
            && (ob.y1 - ob.y0 - (cb.y1 - cb.y0)).abs() < 0.01
        {
            same_bounds += 1;
        }
    }
    let pages = (last + 1) as usize;
    println!("  text layer:  {same_text}/{pages} pages identical (first mismatch: {first_text_mismatch:?})");
    println!("  page bounds: {same_bounds}/{pages} pages identical");

    Ok(())
}

fn count_outline(items: &[mupdf::Outline], depth: usize) -> (usize, usize) {
    let mut total = items.len();
    let mut deepest = depth;
    for item in items {
        let (children, child_depth) = count_outline(&item.down, depth + 1);
        total += children;
        deepest = deepest.max(child_depth);
    }
    (total, deepest)
}

/// A long, distinctive word to search for. Short words appear everywhere and
/// would make the comparison meaningless.
fn longest_word(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_alphabetic())
        .filter(|word| word.chars().count() >= 8)
        .max_by_key(|word| word.chars().count())
        .map(|word| word.to_string())
}

/// The text of one page, as the extractor would read it.
fn page_text(doc: &Document, index: i32) -> Result<String, Box<dyn std::error::Error>> {
    let page = doc.load_page(index)?;
    let text_page = page.to_text_page(TextPageFlags::empty())?;
    let mut out = String::new();
    for block in text_page.blocks() {
        for line in block.lines() {
            for character in line.chars() {
                if let Some(c) = character.char() {
                    out.push(c);
                }
            }
            out.push('\n');
        }
    }
    Ok(out)
}

/// What a PalmDB container declares about itself, read before MuPDF touches it.
///
/// The reason this matters: `mobi_read_data` in MuPDF dispatches only
/// `COMPRESSION_NONE` and PalmDOC, and sends everything else — HUFF/CDIC,
/// which is compression type 17480 — through the PalmDOC decompressor anyway.
/// That returns plausible-looking bytes rather than an error, so a real
/// implementation has to refuse here rather than trust what comes back.
fn palmdb_report(path: &str) {
    use std::io::Read;

    // A bounded read: the answer lives in the first few hundred bytes, and
    // slurping the file would mean reading hundreds of megabytes to look at
    // forty of them.
    let Ok(mut file) = std::fs::File::open(path) else {
        return;
    };
    let mut bytes = vec![0u8; 1 << 16];
    let Ok(read) = file.read(&mut bytes) else {
        return;
    };
    bytes.truncate(read);
    if bytes.len() < 88 {
        return;
    }
    let kind = String::from_utf8_lossy(&bytes[60..68]).to_string();
    if kind != "BOOKMOBI" && kind != "TEXtREAd" {
        return;
    }
    let records = u16::from_be_bytes([bytes[76], bytes[77]]);
    let record0 = u32::from_be_bytes([bytes[78], bytes[79], bytes[80], bytes[81]]) as usize;
    if record0 + 40 > bytes.len() {
        println!("  palmdb: type={kind} records={records} (record 0 beyond the probed window)");
        return;
    }
    let compression = u16::from_be_bytes([bytes[record0], bytes[record0 + 1]]);
    // The MOBI header follows the 16-byte PalmDOC header. Its file-version
    // field is the one reliable KF8 discriminator: 6 for the MOBI6 records
    // MuPDF can read, 8 for KF8, which it cannot and does not say so.
    let version = if &bytes[record0 + 16..record0 + 20] == b"MOBI" {
        Some(u32::from_be_bytes([
            bytes[record0 + 36],
            bytes[record0 + 37],
            bytes[record0 + 38],
            bytes[record0 + 39],
        ]))
    } else {
        None
    };
    let named = match compression {
        1 => "none",
        2 => "PalmDOC",
        17480 => "HUFF/CDIC — decoded as PalmDOC, returns garbage",
        _ => "unknown",
    };
    let boundary = bytes.windows(8).any(|w| w == b"BOUNDARY");
    let verdict = match (version, compression) {
        (_, 17480) | (_, 0) => "REFUSE: unsupported compression",
        (Some(v), _) if v >= 8 => "REFUSE: KF8 — MuPDF reads MOBI6 records and finds none",
        (Some(_), 1 | 2) => "accept",
        _ => "REFUSE: unrecognised MOBI header",
    };
    println!(
        "  palmdb: type={kind} records={records} compression={compression} ({named}) file_version={version:?} kf8_boundary={boundary} -> {verdict}"
    );
}

/// The same file, read the way the application reads it: through the registry,
/// the recipe and the admission rules, rather than by calling MuPDF directly.
/// This is what says the wiring is real and not just the engine's capability.
fn wilkes_path(path: &str) {
    use wilkes_core::extract::ContentExtractor;

    // Extraction reads the whole document — for a 1,255-page scanned book that
    // is minutes, and the sampled mode exists precisely to avoid it.
    let sampled = std::env::var("PROBE_SAMPLE").is_ok();

    let path = std::path::Path::new(path);
    let registry = wilkes_core::extract::native_text_registry();
    let admitted = wilkes_core::types::FileType::detect(
        path,
        &wilkes_core::types::Settings::default().supported_extensions,
    );
    let recipe = wilkes_core::embed::ExtractionRecipe::for_path(path, &registry, 600, 128)
        .selected_extractor;
    print!("  wilkes: file_type={admitted:?} recipe={recipe}");
    if sampled {
        println!(" (sampled: extraction skipped)");
        return;
    }
    match registry.find(path, None) {
        None => println!(" NO EXTRACTOR"),
        Some(extractor) => match extractor.extract(path) {
            Ok(content) => {
                let outline = extractor.outline(path).map(|o| o.entries.len());
                println!(
                    " extracted={} chars, pages={:?}, mime={:?}, outline={:?}",
                    content.text.chars().count(),
                    content.metadata.page_count,
                    content.metadata.mime,
                    outline
                );
                println!("  images discovered: {}", content.images.len());
                // The surrogate as the application builds it, cache and all.
                // Skipped for a PDF, which the application never asks for and
                // `surrogate` refuses: its pages are its own.
                if wilkes_core::extract::document::format::PagedFormat::for_path(path)
                    == Some(wilkes_core::extract::document::format::PagedFormat::Pdf)
                {
                    return;
                }
                let started = std::time::Instant::now();
                match wilkes_core::extract::document::surrogate::surrogate(path) {
                    Ok(rendered) => {
                        let warm = std::time::Instant::now();
                        let _ = wilkes_core::extract::document::surrogate::surrogate(path);
                        println!(
                            "  surrogate(wilkes): {} bytes, {} outline entries, {} pages with links, cold {:?}, warm {:?}",
                            rendered.bytes.len(),
                            rendered.outline.len(),
                            rendered.links.len(),
                            started.elapsed(),
                            warm.elapsed()
                        );
                    }
                    Err(error) => println!("  surrogate(wilkes): FAILED: {error}"),
                }
            }
            Err(error) => println!(" REFUSED: {error}"),
        },
    }
}
