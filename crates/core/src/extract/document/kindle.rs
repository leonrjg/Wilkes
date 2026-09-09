//! Kindle KF8 books, rebuilt into something MuPDF can read.
//!
//! **Why this exists.** MuPDF's Mobipocket reader walks MOBI 6 text records.
//! A KF8 book (`.azw3`, and the occasional `.mobi`) has none: its text lives in
//! KF8 records, so MuPDF opens the file, reports success, and returns a
//! document of one page holding nothing. That silent emptiness is what
//! [`super::format::guard_container`] used to refuse outright.
//!
//! **What it does instead.** The `mobi` crate decompresses the PalmDOC records
//! and hands over the raw KF8 flow and the image resources. The flow is not a
//! document: it is 162 concatenated XHTML skeletons for one measured book,
//! with the bulk of the prose in bare fragments between them, and MuPDF reads
//! nothing from it as it stands. Three things make it readable:
//!
//! 1. **Strip the NUL padding.** The flow begins with several hundred zero
//!    bytes, and MuPDF's HTML parser stops at them. This alone was the
//!    difference between "no text at all" and the whole book — it is not a
//!    tidying step.
//! 2. **Remove the per-document scaffolding** — the XML declarations, the
//!    `<html>`/`<body>` wrappers and the `<head>` blocks — and wrap what is
//!    left in one document. The skeletons and their fragments are stored in
//!    reading order, so the result reads in that order.
//! 3. **Resolve `kindle:embed:` references** against the image records, which
//!    are written out beside the HTML.
//!
//! **What this is not.** It is not true KF8 reassembly. A faithful reader would
//! insert each fragment into its skeleton at the offset recorded in the INDX
//! tables; this concatenates them in flow order instead. For the books measured
//! that is the same reading order, and the text is complete — but the document
//! structure is approximate, and a book whose fragments are stored out of order
//! would come out in that order. Doing better means parsing INDX/TAGX, which is
//! the several-hundred-line job this exists to avoid.
//!
//! **Why the container is read here rather than by a crate.** The `mobi` crate
//! did this at first and silently lost a large fraction of the text — 12.6%,
//! 14.4% and 23.5% of the three books measured, against the length each
//! container declares for itself. Two faults compound: it treats the header
//! record as content, and it strips a fixed number of trailing bytes from
//! every text record where the format specifies a variable count encoded
//! backwards in each record's own tail, so it eats compressed bytes and the
//! text decompresses short. Reading the records here matches the declared
//! length exactly on all three, which is why [`Container::text`] can assert
//! it — an invariant that exists only because this side owns the decoding.
//!
//! It was kept for metadata for a while, then dropped: reading the EXTH block
//! is sixty lines, it produced byte-identical titles, authors and dates on
//! every file measured, and keeping the crate meant reading each book twice
//! and depending on a parser that had already been wrong about the thing it
//! exists to do.
//!
//! **MOBI 6 does not come through here.** It was measured both ways: MuPDF
//! reads that format at 2,890 characters per page over 553 pages, while this
//! path paginates the same book into 5,265 pages of 266 characters and loses
//! 12% of the text. MuPDF is simply the better reader for it, so it keeps it.

use std::path::{Path, PathBuf};

use regex::Regex;
use tracing::{debug, warn};

/// How long a rebuild is kept before it is treated as abandoned.
///
/// Rebuilds are keyed by the book's identity and reused across runs, so this
/// is a disk bound rather than a correctness one — which is the point. An
/// earlier version held them in `TempDir`s evicted by count, and eviction
/// deletes: an index build extracting several Kindle books at once could have
/// deleted the directory a reader still had a document open against. Nothing
/// is deleted here while it could be in use, only long after.
const ABANDONED_AFTER: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// How much rebuilt book may sit on disk before the oldest are removed.
///
/// Age alone is not a bound: indexing a library of Kindle books rebuilds each
/// one, and a few hundred of them — 11.7 MB of images for one measured book —
/// would fill the disk long before any were six hours old.
const DISK_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// No rebuild touched this recently is removed, whatever the budget says.
///
/// This is the safety margin that lets pruning be safe at all: a rebuild in use
/// was touched when it was opened, and deleting a directory a reader still has
/// a document against is the failure this whole scheme is arranged to avoid.
const IN_USE_WITHIN: std::time::Duration = std::time::Duration::from_secs(30 * 60);

const INDEX: &str = "index.html";

/// What a Kindle container says about the book.
pub fn container_metadata(path: &Path) -> anyhow::Result<KindleMetadata> {
    Ok(Container::open(path)?.metadata())
}

/// What the file's own header says it is.
///
/// KF8 is announced by the MOBI header's file-version field: 6 for the records
/// MuPDF reads, 8 for the ones it cannot. The compression word does not
/// distinguish them — both declare PalmDOC — which is why this reads the
/// version and not the thing one would reach for first.
pub fn is_kf8(path: &Path) -> anyhow::Result<bool> {
    Ok(super::format::mobi_file_version(path)?.is_some_and(|version| version >= 8))
}

/// This book, rebuilt as HTML, with the path of the document to open.
///
/// Keyed on the book's identity — path, size and modification time — so a book
/// that changed on disk is rebuilt rather than served as the copy made before
/// it changed, and an unchanged one is rebuilt once however often it is opened.
pub fn materialize(path: &Path) -> anyhow::Result<PathBuf> {
    let directory = rebuild_root()?.join(identity(path)?);
    let index = directory.join(INDEX);
    if index.is_file() {
        debug!("kf8: reusing the rebuild of {}", path.display());
        // Marks it as live, so the pruning below leaves it alone.
        let _ = filetime_now(&directory);
        prune(&directory);
        return Ok(index);
    }

    // Built beside the destination and moved into place, so a rebuild
    // interrupted halfway cannot be found and read as a complete one.
    let staging = directory.with_extension("building");
    let _ = std::fs::remove_dir_all(&staging);
    rebuild(path, &staging)?;
    std::fs::create_dir_all(directory.parent().unwrap_or(&directory))?;
    match std::fs::rename(&staging, &directory) {
        Ok(()) => {}
        Err(_) if index.is_file() => {
            // Another thread finished the same rebuild first. Theirs is as
            // good as ours; drop ours.
            let _ = std::fs::remove_dir_all(&staging);
        }
        Err(error) => return Err(error.into()),
    }
    prune(&directory);
    Ok(index)
}

fn rebuild_root() -> anyhow::Result<PathBuf> {
    let root = std::env::temp_dir().join("wilkes-kf8");
    std::fs::create_dir_all(&root)?;
    Ok(root)
}

/// The book's identity, as a directory name.
fn identity(path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let metadata = std::fs::metadata(path)?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_millis())
        .unwrap_or(0);
    let mut digest = Sha256::new();
    digest.update(path.to_string_lossy().as_bytes());
    digest.update(metadata.len().to_le_bytes());
    digest.update(modified.to_le_bytes());
    Ok(digest
        .finalize()
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn filetime_now(directory: &Path) -> std::io::Result<()> {
    // Touching the directory's own entry is enough to mark it live; the
    // contents are not rewritten.
    let marker = directory.join(INDEX);
    let handle = std::fs::OpenOptions::new().append(true).open(marker)?;
    handle.set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now()))
}

/// Delete rebuilds that are old, and then — oldest first — whatever else is
/// needed to stay under the disk budget.
///
/// Never anything touched recently, and never the one just asked for. A bound
/// on *how many* rebuilds exist would be a bound that deletes the one in use
/// as soon as enough books are opened at once, which is precisely how an
/// earlier version of this could have pulled a directory out from under a
/// reader mid-document.
fn prune(keep: &Path) {
    let Ok(root) = rebuild_root() else { return };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    let now = std::time::SystemTime::now();

    let mut candidates: Vec<(std::time::Duration, u64, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let candidate = entry.path();
        if candidate == keep || !candidate.is_dir() {
            continue;
        }
        let Some(age) = std::fs::metadata(candidate.join(INDEX))
            .or_else(|_| std::fs::metadata(&candidate))
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
        else {
            continue;
        };
        if age < IN_USE_WITHIN {
            continue;
        }
        candidates.push((age, directory_size(&candidate), candidate));
    }

    // Oldest first, so the budget takes the least recently wanted.
    candidates.sort_by_key(|(age, _, _)| std::cmp::Reverse(*age));
    let mut total: u64 = candidates.iter().map(|(_, size, _)| *size).sum();
    for (age, size, candidate) in candidates {
        let abandoned = age > ABANDONED_AFTER;
        if !abandoned && total <= DISK_BUDGET_BYTES {
            continue;
        }
        debug!(
            "kf8: removing the rebuild {} ({} bytes, {}s old)",
            candidate.display(),
            size,
            age.as_secs()
        );
        if std::fs::remove_dir_all(&candidate).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

fn directory_size(directory: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(metadata) if metadata.is_dir() => directory_size(&entry.path()),
            Ok(metadata) => metadata.len(),
            Err(_) => 0,
        })
        .sum()
}

fn rebuild(path: &Path, into: &Path) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let container = Container::open(path)?;
    let flow = container.text()?;
    let images = container.images();
    let written = write_rebuild(into, &flow, &images)?;

    debug!(
        "kf8: rebuilt {} from {} bytes of flow with {written} images in {:?}",
        path.display(),
        flow.len(),
        started.elapsed()
    );
    Ok(())
}

/// A Mobipocket container, read directly.
///
/// **Why not the `mobi` crate for this.** It was used for the text at first,
/// and it silently loses a large fraction of it: 12.6%, 14.4% and 23.5% of the
/// three books measured, against the length the container itself declares.
/// Two faults compound — it treats the *header* record as content, and it
/// strips a fixed number of trailing bytes from every text record where the
/// format specifies a variable-length count that has to be read backwards out
/// of each record's tail. Reading the records here matches the declared length
/// exactly on all three books, which is why [`Container::text`] can assert it.
///
/// The crate is still used for metadata, where it reads the EXTH block well
/// and where a fault would be visible rather than silent.
struct Container {
    bytes: Vec<u8>,
    /// Byte offset of each record, in order.
    offsets: Vec<usize>,
    compression: u16,
    /// The uncompressed length of the text, as the container declares it.
    text_length: usize,
    text_records: usize,
    /// Which trailing entries each text record carries — see
    /// [`trailing_size`].
    extra_flags: u16,
    first_image: usize,
    /// Where the MOBI header ends, which is where EXTH begins.
    mobi_header_length: usize,
    exth_flags: u32,
    text_encoding: u32,
    /// The book's name, as the header points at it.
    name: Option<String>,
}

impl Container {
    fn open(path: &Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        anyhow::ensure!(
            bytes.len() > 86,
            "{} is too short to be a Mobipocket container",
            path.display()
        );
        let count = u16::from_be_bytes([bytes[76], bytes[77]]) as usize;
        anyhow::ensure!(
            count > 1 && bytes.len() >= 78 + 8 * count,
            "{} declares {count} records but does not carry their table",
            path.display()
        );
        let offsets: Vec<usize> = (0..count)
            .map(|index| {
                let entry = 78 + 8 * index;
                u32::from_be_bytes([
                    bytes[entry],
                    bytes[entry + 1],
                    bytes[entry + 2],
                    bytes[entry + 3],
                ]) as usize
            })
            .collect();
        let record0 = offsets[0];
        anyhow::ensure!(
            record0 + 16 + 232 <= bytes.len(),
            "{} has no readable record 0",
            path.display()
        );
        let word = |at: usize| u16::from_be_bytes([bytes[at], bytes[at + 1]]);
        let long = |at: usize| {
            u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
        };
        let mobi_header_length = long(record0 + 16 + 4);
        let name_offset = long(record0 + 16 + 68);
        let name_length = long(record0 + 16 + 72);
        let text_encoding = long(record0 + 16 + 12) as u32;
        let name = bytes
            .get(name_offset..name_offset.saturating_add(name_length))
            .map(|raw| decode(raw, text_encoding))
            .filter(|value| !value.trim().is_empty());
        Ok(Self {
            compression: word(record0),
            text_length: long(record0 + 4),
            text_records: word(record0 + 8) as usize,
            // The MOBI header sits behind the 16-byte PalmDOC header; the
            // extra-data flags are the last field of the 232-byte form.
            extra_flags: word(record0 + 16 + 226),
            first_image: long(record0 + 16 + 92),
            exth_flags: long(record0 + 16 + 112) as u32,
            mobi_header_length,
            text_encoding,
            name,
            offsets,
            bytes,
        })
    }

    fn record(&self, index: usize) -> &[u8] {
        let start = self.offsets[index];
        let end = self
            .offsets
            .get(index + 1)
            .copied()
            .unwrap_or(self.bytes.len());
        self.bytes.get(start..end).unwrap_or(&[])
    }

    /// The book's markup, assembled from its text records.
    ///
    /// Checked against the length the container declares. That check is only
    /// possible because the records are read here: it is what turns "the text
    /// looked plausible" into "the text is all of it".
    fn text(&self) -> anyhow::Result<String> {
        anyhow::ensure!(
            self.compression == 1 || self.compression == 2,
            "compression {} cannot be decoded (only uncompressed and PalmDOC)",
            self.compression
        );
        let mut out: Vec<u8> = Vec::with_capacity(self.text_length);
        for index in 1..=self.text_records.min(self.offsets.len().saturating_sub(1)) {
            let record = self.record(index);
            let keep = record.len() - trailing_size(record, self.extra_flags).min(record.len());
            let body = &record[..keep];
            if self.compression == 2 {
                decompress_palmdoc(body, &mut out);
            } else {
                out.extend_from_slice(body);
            }
        }
        // A shortfall is lost prose, and this is the one place it can still be
        // seen: past here it is markup that merely looks a little shorter.
        anyhow::ensure!(
            out.len() * 100 >= self.text_length * 99,
            "only {} of the {} bytes of text this book declares could be read; \
             refusing it rather than indexing a fraction of it",
            out.len(),
            self.text_length
        );
        if out.len() != self.text_length {
            debug!(
                "kf8: read {} bytes of text against {} declared",
                out.len(),
                self.text_length
            );
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// The resource records, in the order `kindle:embed:` numbers them.
    fn images(&self) -> Vec<&[u8]> {
        (self.first_image.max(self.text_records + 1)..self.offsets.len())
            .map(|index| self.record(index))
            .take_while(|record| !record.starts_with(b"BOUNDARY"))
            .collect()
    }
}

/// What a Mobipocket container says about the book, from its own EXTH block.
///
/// Only the fields Wilkes records. Everything else the block carries — reading
/// direction, sample flags, a dozen Amazon identifiers — is left where it is.
#[derive(Debug, Default, PartialEq)]
pub struct KindleMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub published: Option<String>,
}

impl Container {
    /// The EXTH records, by type.
    ///
    /// EXTH sits immediately after the MOBI header and is announced by a flag
    /// in it. A container without one is not an error: the name in the header
    /// still gives a title.
    fn exth(&self) -> std::collections::BTreeMap<u32, &[u8]> {
        let mut found = std::collections::BTreeMap::new();
        if self.exth_flags & 0x40 == 0 {
            return found;
        }
        let start = self.offsets[0] + 16 + self.mobi_header_length;
        let Some(block) = self.bytes.get(start..) else {
            return found;
        };
        if !block.starts_with(b"EXTH") || block.len() < 12 {
            return found;
        }
        let count = u32::from_be_bytes([block[8], block[9], block[10], block[11]]) as usize;
        let mut cursor = 12usize;
        for _ in 0..count {
            if cursor + 8 > block.len() {
                break;
            }
            let kind = u32::from_be_bytes([
                block[cursor],
                block[cursor + 1],
                block[cursor + 2],
                block[cursor + 3],
            ]);
            let length = u32::from_be_bytes([
                block[cursor + 4],
                block[cursor + 5],
                block[cursor + 6],
                block[cursor + 7],
            ]) as usize;
            // The length counts the eight header bytes; anything shorter than
            // that is a malformed record and the walk cannot continue past it.
            if length < 8 || cursor + length > block.len() {
                break;
            }
            found.insert(kind, &block[cursor + 8..cursor + length]);
            cursor += length;
        }
        found
    }

    /// Title, author and publication date, as the container declares them.
    ///
    /// MuPDF reports none of these for a Mobipocket file: an empty title and
    /// author for MOBI 6, and for KF8 whatever the rebuilt markup's first
    /// heading happened to be. The container is the only honest source.
    pub fn metadata(&self) -> KindleMetadata {
        let exth = self.exth();
        let field = |kind: u32| {
            exth.get(&kind)
                .map(|raw| decode(raw, self.text_encoding))
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        KindleMetadata {
            // 503 is the updated title, which publishers set when the header's
            // own name is a filename; the header name is the fallback.
            title: field(503).or_else(|| self.name.clone()),
            author: field(100),
            published: field(106),
        }
    }
}

/// Text from a container, in the encoding it declares.
///
/// Only two are used in practice. An unknown one is read as UTF-8 rather than
/// guessed at, and the lossy decode makes that visible as replacement
/// characters instead of silently dropping bytes.
fn decode(raw: &[u8], encoding: u32) -> String {
    match encoding {
        1252 => raw.iter().map(|byte| *byte as char).collect(),
        _ => String::from_utf8_lossy(raw).into_owned(),
    }
}

/// How many bytes at the end of a text record are not text.
///
/// The format puts a variable number of trailing entries after each record's
/// content, and their sizes are encoded backwards from the record's end — a
/// big-endian base-128 integer whose last byte has the high bit set. A reader
/// that strips a fixed count instead removes compressed bytes from most
/// records, and the text after them decompresses short; that is the fault this
/// exists to avoid, and it costs a tenth to a quarter of a book.
fn trailing_size(record: &[u8], flags: u16) -> usize {
    let mut total = 0usize;
    let mut remaining = flags >> 1;
    while remaining != 0 {
        if remaining & 1 != 0 {
            total += trailing_entry(record, record.len().saturating_sub(total));
        }
        remaining >>= 1;
    }
    // The low flag marks a multibyte character overlapping the record
    // boundary; its own length lives in the two low bits of the last byte.
    if flags & 1 != 0 && record.len() > total {
        total += (record[record.len() - total - 1] & 0x3) as usize + 1;
    }
    total
}

fn trailing_entry(record: &[u8], mut size: usize) -> usize {
    if size == 0 || size > record.len() {
        return 0;
    }
    let (mut bits, mut value) = (0u32, 0usize);
    loop {
        let byte = record[size - 1];
        value |= ((byte & 0x7F) as usize) << bits;
        bits += 7;
        size -= 1;
        if byte & 0x80 != 0 || bits >= 28 || size == 0 {
            return value;
        }
    }
}

/// PalmDOC decompression: literals, short literal runs, a back-reference into
/// what has been produced, and the space-plus-letter shorthand.
fn decompress_palmdoc(input: &[u8], out: &mut Vec<u8>) {
    let mut cursor = 0usize;
    while cursor < input.len() {
        let byte = input[cursor];
        cursor += 1;
        match byte {
            0x00 => out.push(0),
            0x01..=0x08 => {
                let run = (byte as usize).min(input.len() - cursor);
                out.extend_from_slice(&input[cursor..cursor + run]);
                cursor += run;
            }
            0x09..=0x7F => out.push(byte),
            0x80..=0xBF => {
                if cursor >= input.len() {
                    break;
                }
                let pair = (((byte as usize) << 8) | input[cursor] as usize) & 0x3FFF;
                cursor += 1;
                let distance = pair >> 3;
                let length = (pair & 7) + 3;
                if distance == 0 || distance > out.len() {
                    break;
                }
                for _ in 0..length {
                    let byte = out[out.len() - distance];
                    out.push(byte);
                }
            }
            0xC0..=0xFF => {
                out.push(b' ');
                out.push(byte ^ 0x80);
            }
        }
    }
}

/// The rebuild itself, given what the container held.
///
/// Separate from the parse above so the transformation this module actually
/// owns — flattening, resolving resources, writing them out — is testable
/// without a Kindle container. The parse is a third party's and is exercised
/// against real books by `crates/core/examples/probe_epub.rs`.
fn write_rebuild(into: &Path, flow: &str, images: &[&[u8]]) -> anyhow::Result<usize> {
    let directory = into.join("images");
    std::fs::create_dir_all(&directory)?;

    // The resources, written out under the names the rewritten references use.
    // Written before the markup so a reference can only point at a file that
    // exists.
    let mut written = Vec::with_capacity(images.len());
    for (index, image) in images.iter().enumerate() {
        let name = format!("{:04}.{}", index + 1, image_extension(image));
        std::fs::write(directory.join(&name), image)?;
        written.push(name);
    }

    let html = flatten(flow, &written)?;
    std::fs::write(into.join(INDEX), &html)?;
    Ok(written.len())
}

/// The words in a run of markup, for comparing what went in with what came
/// out. Approximate by construction — it is a proportion that matters here,
/// not a count.
fn prose_words(markup: &str) -> usize {
    let mut words = 0usize;
    let mut inside_tag = false;
    let mut in_word = false;
    for character in markup.chars() {
        match character {
            '<' => {
                inside_tag = true;
                in_word = false;
            }
            '>' => inside_tag = false,
            c if inside_tag => {
                let _ = c;
            }
            c if c.is_whitespace() => in_word = false,
            _ => {
                if !in_word {
                    words += 1;
                    in_word = true;
                }
            }
        }
    }
    words
}

/// The raw KF8 flow, as one HTML document.
fn flatten(flow: &str, images: &[String]) -> anyhow::Result<String> {
    // The NUL padding at the head of the flow. MuPDF's HTML parser stops at
    // it and reports a document of one empty page, which is indistinguishable
    // from a book that holds nothing.
    let mut body = flow.replace('\0', "");

    body = strip_head_blocks(&body);
    body = scaffolding().replace_all(&body, "").into_owned();
    body = embeds()
        .replace_all(&body, |captures: &regex::Captures<'_>| {
            match base32_index(&captures[1]).and_then(|index| images.get(index.checked_sub(1)?)) {
                Some(name) => format!("images/{name}"),
                None => {
                    // A reference to a resource the file does not carry. Left as a
                    // reference that resolves to nothing rather than removed, so
                    // the reader shows the picture's alt text where the picture
                    // was, rather than silently closing the gap.
                    warn!("kf8: no image record for {}", &captures[1]);
                    format!("images/missing-{}", &captures[1])
                }
            }
        })
        .into_owned();

    // What came out against what went in.
    //
    // Rebuilding a book is markup surgery on a format whose own markup is not
    // well formed, and the failure it invites is quiet: a pattern that runs on
    // deletes prose and produces a document that still looks like a book. That
    // is exactly what the refusal this path replaced used to prevent, so the
    // protection has to live here now. Losing a word per document is expected
    // — that is each one's `<title>`.
    let before = prose_words(flow);
    let after = prose_words(&body);
    // Only over a book's worth of words. Each document contributes its
    // `<title>` to the input and not to the output, so over a handful of words
    // the proportion says more about the fixture than about the rebuild.
    const ENOUGH_TO_JUDGE: usize = 500;
    if before >= ENOUGH_TO_JUDGE {
        let kept = after as f64 / before as f64;
        if kept < 0.5 {
            anyhow::bail!(
                "rebuilding this book kept only {after} of its {before} words; \
                 refusing it rather than indexing a fragment as the whole book"
            );
        }
        if kept < 0.95 {
            warn!(
                "kf8: rebuilding kept {after} of {before} words ({:.1}% lost); \
                 the flow's markup is likely malformed in a way the rebuild does not handle",
                100.0 * (1.0 - kept)
            );
        }
    }

    Ok(format!(
        "<html><head><meta charset=\"utf-8\"/></head><body>{body}</body></html>"
    ))
}

/// The per-document wrappers: XML declarations, `<html>` and `<body>` tags.
///
/// `<head>` blocks are *not* here — see [`strip_head_blocks`], which needs to
/// know where the next document starts and so cannot be a pattern. Rust's
/// `regex` has no lookaround, and the version of this that tried anyway
/// deleted 5,178 words of a measured book.
fn scaffolding() -> &'static Regex {
    static CELL: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(r"(?is)<\?xml[^>]*\?>|</?html[^>]*>|</?body[^>]*>")
            .expect("the scaffolding pattern is a literal")
    })
}

/// Remove each `<head>…</head>`, and only where it closes.
///
/// **Why this is a scan and not a pattern.** The flow's markup is not well
/// formed: one measured book carries 160 `<head` against 158 `</head>`. A
/// non-greedy `<head\b.*?</head>` therefore runs past the end of its own
/// document and takes the next one's prose with it — 5,178 words of that book,
/// deleted with nothing said. So a `<head>` is removed only when its close
/// comes before the next document begins, and an unclosed one is left exactly
/// where it is. Stray title text in the body is a blemish; deleted prose is a
/// lie about what the book says.
fn strip_head_blocks(markup: &str) -> String {
    // Byte indices from the lowercased copy address the original unchanged:
    // `to_ascii_lowercase` maps ASCII to ASCII and leaves every other byte be,
    // and every delimiter looked for here is ASCII.
    let lower = markup.to_ascii_lowercase();
    let mut out = String::with_capacity(markup.len());
    let mut cursor = 0usize;
    while let Some(found) = lower[cursor..].find("<head") {
        let open = cursor + found;
        // `<header>` is not `<head>`.
        let after_name = open + "<head".len();
        if !lower[after_name..]
            .chars()
            .next()
            .is_some_and(|c| c == '>' || c.is_whitespace())
        {
            out.push_str(&markup[cursor..after_name]);
            cursor = after_name;
            continue;
        }
        let close = lower[open..].find("</head>").map(|at| open + at);
        let boundary = ["<html", "<body"]
            .iter()
            .filter_map(|marker| lower[after_name..].find(marker).map(|at| after_name + at))
            .min();
        match close {
            // Closes before the next document starts: the block goes.
            Some(close) if boundary.is_none_or(|edge| close < edge) => {
                out.push_str(&markup[cursor..open]);
                cursor = close + "</head>".len();
            }
            // Unclosed, or closed only after the next document began. Left
            // alone: its content stays in the reading.
            _ => {
                out.push_str(&markup[cursor..after_name]);
                cursor = after_name;
            }
        }
    }
    out.push_str(&markup[cursor..]);
    out
}

/// `kindle:embed:XXXX`, with the query the reference may carry.
fn embeds() -> &'static Regex {
    static CELL: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(r"kindle:embed:([0-9A-Za-z]+)(?:\?[^\s\x22']*)?")
            .expect("the embed pattern is a literal")
    })
}

/// A KF8 resource number: base 32, digits then `A`-`V`, one-based.
fn base32_index(text: &str) -> Option<usize> {
    let mut value = 0usize;
    for character in text.chars() {
        let digit = match character.to_ascii_uppercase() {
            c @ '0'..='9' => c as usize - '0' as usize,
            c @ 'A'..='V' => c as usize - 'A' as usize + 10,
            _ => return None,
        };
        value = value.checked_mul(32)?.checked_add(digit)?;
    }
    Some(value)
}

/// What a resource record holds, by its own first bytes. The reference's
/// `?mime=` is the file's claim about itself and is not consulted: a record
/// written as one thing and labelled another would be saved under a name that
/// tells the reader to decode it wrongly.
fn image_extension(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if bytes.starts_with(b"\x89PNG") {
        "png"
    } else if bytes.starts_with(b"GIF8") {
        "gif"
    } else if bytes.starts_with(b"BM") {
        "bmp"
    } else {
        "bin"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A book that changed on disk must not be served the rebuild made before
    /// it changed — the identity is what stops that, so it has to move when
    /// the file does.
    /// A file that is not a Kindle book at all must fail as this document's
    /// error, never as a panic: extraction runs in the host, and an index
    /// build that took the application down on one malformed book would lose
    /// the whole run.
    #[test]
    fn a_malformed_container_fails_as_an_error() {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            ("empty.azw3", b"".to_vec()),
            ("short.azw3", b"BOOKMOBI".to_vec()),
            (
                "noise.azw3",
                (0..4096u32).map(|i| (i % 251) as u8).collect(),
            ),
            ("truncated.azw3", {
                let mut bytes = vec![0u8; 200];
                bytes[60..68].copy_from_slice(b"BOOKMOBI");
                bytes
            }),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, &bytes).unwrap();
            let outcome = std::panic::catch_unwind(|| rebuild(&path, &dir.path().join("out")));
            assert!(
                matches!(outcome, Ok(Err(_))),
                "{name} must fail as an error, not a panic or a success"
            );
        }
    }

    #[test]
    fn the_rebuild_identity_follows_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.azw3");
        std::fs::write(&path, b"first").unwrap();
        let first = identity(&path).unwrap();
        assert_eq!(
            identity(&path).unwrap(),
            first,
            "unchanged file, same rebuild"
        );

        std::fs::write(&path, b"second, and longer").unwrap();
        assert_ne!(
            identity(&path).unwrap(),
            first,
            "a changed book must rebuild"
        );

        // And two different books never share one.
        let other = dir.path().join("other.azw3");
        std::fs::write(&other, b"first").unwrap();
        assert_ne!(identity(&other).unwrap(), identity(&path).unwrap());
    }

    /// PalmDOC is four cases, and each of them has bitten someone.
    #[test]
    fn palmdoc_decodes_its_four_cases() {
        let decode = |input: &[u8]| {
            let mut out = Vec::new();
            decompress_palmdoc(input, &mut out);
            out
        };
        // Literal bytes.
        assert_eq!(decode(b"Hola"), b"Hola");
        // A literal run: 0x01..=0x08 copies that many bytes verbatim, which is
        // how bytes that would otherwise be read as operators are carried.
        assert_eq!(decode(&[0x02, 0xC3, 0xA1, b'x']), vec![0xC3, 0xA1, b'x']);
        // Space plus a letter, the shorthand that makes English compress.
        assert_eq!(decode(&[0xC0 | b'a']), b" a".to_vec());
        // A back-reference into what has already been produced: distance 5,
        // length 3, over "abcde" gives "abc" again.
        let pair = (5usize << 3) | (3 - 3);
        let encoded = [
            b'a',
            b'b',
            b'c',
            b'd',
            b'e',
            0x80 | (pair >> 8) as u8,
            pair as u8,
        ];
        assert_eq!(decode(&encoded), b"abcdeabc".to_vec());
    }

    /// A back-reference that points outside what has been produced would panic
    /// on an index, and these files come from a corpus.
    #[test]
    fn palmdoc_stops_rather_than_reading_past_its_output() {
        let mut out = Vec::new();
        let pair = (99usize << 3) | 0;
        decompress_palmdoc(&[0x80 | (pair >> 8) as u8, pair as u8], &mut out);
        assert!(out.is_empty());
        // And a truncated pair at the very end.
        let mut out = Vec::new();
        decompress_palmdoc(&[b'a', 0x81], &mut out);
        assert_eq!(out, b"a");
    }

    /// The trailing-entry sizes are encoded backwards from a record's end. A
    /// reader that strips a fixed count instead eats compressed bytes and the
    /// text decompresses short — a tenth to a quarter of a book, silently.
    #[test]
    fn trailing_entries_are_measured_backwards_from_the_record() {
        // No flags: nothing is trailing.
        assert_eq!(trailing_size(b"content", 0), 0);

        // Flag 2: one trailing entry whose size is a backwards base-128
        // integer, terminated by a byte with the high bit set. `0x83` is 3.
        let record = b"content\x83";
        assert_eq!(trailing_size(record, 0b10), 3);

        // Flag 1: a multibyte character overlapping the boundary, its length
        // in the low two bits of the final byte.
        assert_eq!(trailing_size(&[b'a', b'b', 0x02], 0b1), 3);

        // A multi-byte size: read backwards, each byte contributes seven bits
        // and the *last* one read carries the terminator. `0x01` then `0x82`
        // is 1 | (2 << 7) = 257 — which is why the terminator's position
        // matters and a fixed-width read of this field goes wrong.
        assert_eq!(trailing_size(&[b'x', 0x82, 0x01], 0b10), 257);

        // Both flags, which is what every book measured declares: a one-byte
        // entry of 1, and then the multibyte marker before it.
        let record = [b'a', b'b', 0x00, 0x81];
        assert_eq!(trailing_size(&record, 0b11), 2);
        assert_eq!(
            &record[..record.len() - trailing_size(&record, 0b11)],
            b"ab"
        );
    }

    /// A 1×1 PNG, so a rebuild has a real resource to place.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0xFC,
        0xCF, 0xC0, 0x50, 0x0F, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xA9, 0x8C, 0x21, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    /// The metadata block, which is now the only source for a Kindle book's
    /// title, author and date — MuPDF reports none of them.
    #[test]
    fn exth_records_give_the_title_author_and_date() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.azw3");
        let records: Vec<&[u8]> = vec![b"<html><body><p>Text.</p></body></html>"];
        let bytes = container_fixture_with_exth(
            &records,
            &[
                (100, b"Randall Munroe".as_slice()),
                (106, b"2014-07-23T07:00:00+00:00".as_slice()),
                (503, b"What If?".as_slice()),
            ],
        );
        std::fs::write(&path, bytes).unwrap();

        let found = Container::open(&path).unwrap().metadata();
        assert_eq!(
            found,
            KindleMetadata {
                title: Some("What If?".to_string()),
                author: Some("Randall Munroe".to_string()),
                published: Some("2014-07-23T07:00:00+00:00".to_string()),
            }
        );
    }

    /// A container with no EXTH block is not an error: the header's own name
    /// is still a title.
    #[test]
    fn a_container_without_exth_still_has_a_title() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.azw3");
        let records: Vec<&[u8]> = vec![b"<html><body><p>Text.</p></body></html>"];
        std::fs::write(&path, container_fixture(&records, 1, 0)).unwrap();
        let found = Container::open(&path).unwrap().metadata();
        assert_eq!(found.title.as_deref(), Some("Fixture Name"));
        assert_eq!(found.author, None);
    }

    /// A record claiming a length shorter than its own header would walk the
    /// cursor backwards forever.
    #[test]
    fn a_malformed_exth_record_stops_the_walk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.azw3");
        let records: Vec<&[u8]> = vec![b"<html><body><p>Text.</p></body></html>"];
        let mut bytes = container_fixture_with_exth(&records, &[(100, b"Author".as_slice())]);
        // Find the EXTH block and corrupt its first record's length.
        let at = bytes
            .windows(4)
            .position(|window| window == b"EXTH")
            .expect("the fixture writes an EXTH block");
        bytes[at + 16..at + 20].copy_from_slice(&0u32.to_be_bytes());
        std::fs::write(&path, bytes).unwrap();

        // Returns rather than looping or panicking; the title falls back.
        let found = Container::open(&path).unwrap().metadata();
        assert_eq!(found.author, None);
        assert_eq!(found.title.as_deref(), Some("Fixture Name"));
    }

    /// A Mobipocket container, laid out as the format specifies.
    ///
    /// Buildable now that the records are read here rather than by a crate
    /// whose indexing could not be made to agree with a synthetic file. This
    /// is what closes the last gap: the container parse itself, in CI, without
    /// a copyrighted book.
    fn container_fixture_with_exth(records: &[&[u8]], exth: &[(u32, &[u8])]) -> Vec<u8> {
        build_container(records, 1, 0, exth)
    }

    fn container_fixture(records: &[&[u8]], compression: u16, flags: u16) -> Vec<u8> {
        build_container(records, compression, flags, &[])
    }

    fn build_container(
        records: &[&[u8]],
        compression: u16,
        flags: u16,
        exth: &[(u32, &[u8])],
    ) -> Vec<u8> {
        const MOBI_LEN: usize = 232;
        let text_length: usize = records.iter().map(|record| record.len()).sum();
        let mut record0 = vec![0u8; 16 + MOBI_LEN];
        record0[0..2].copy_from_slice(&compression.to_be_bytes());
        record0[4..8].copy_from_slice(&(text_length as u32).to_be_bytes());
        record0[8..10].copy_from_slice(&(records.len() as u16).to_be_bytes());
        record0[10..12].copy_from_slice(&4096u16.to_be_bytes());
        let mobi = |offset: usize| 16 + offset;
        record0[mobi(0)..mobi(4)].copy_from_slice(b"MOBI");
        record0[mobi(4)..mobi(8)].copy_from_slice(&(MOBI_LEN as u32).to_be_bytes());
        record0[mobi(20)..mobi(24)].copy_from_slice(&8u32.to_be_bytes()); // KF8
        record0[mobi(92)..mobi(96)].copy_from_slice(&((records.len() + 1) as u32).to_be_bytes());
        record0[mobi(226)..mobi(228)].copy_from_slice(&flags.to_be_bytes());

        const NAME: &[u8] = b"Fixture Name";
        if !exth.is_empty() {
            record0[mobi(112)..mobi(116)].copy_from_slice(&0x40u32.to_be_bytes());
            let mut block = b"EXTH".to_vec();
            let body: Vec<u8> = exth
                .iter()
                .flat_map(|(kind, data)| {
                    let mut record = kind.to_be_bytes().to_vec();
                    record.extend_from_slice(&((data.len() + 8) as u32).to_be_bytes());
                    record.extend_from_slice(data);
                    record
                })
                .collect();
            block.extend_from_slice(&((body.len() + 12) as u32).to_be_bytes());
            block.extend_from_slice(&(exth.len() as u32).to_be_bytes());
            block.extend_from_slice(&body);
            record0.extend_from_slice(&block);
        }
        record0.extend_from_slice(NAME);

        let all: Vec<Vec<u8>> = std::iter::once(record0)
            .chain(records.iter().map(|record| record.to_vec()))
            .chain(std::iter::once(TINY_PNG.to_vec()))
            .collect();

        let mut out = vec![0u8; 78 + 8 * all.len() + 2];
        // The name sits at the end of record 0, and the header points at it.
        let name_offset = out.len() + all[0].len() - NAME.len();
        let mobi_in_record0 = 16;
        let mut all = all;
        all[0][mobi_in_record0 + 68..mobi_in_record0 + 72]
            .copy_from_slice(&(name_offset as u32).to_be_bytes());
        all[0][mobi_in_record0 + 72..mobi_in_record0 + 76]
            .copy_from_slice(&(NAME.len() as u32).to_be_bytes());
        out[60..64].copy_from_slice(b"BOOK");
        out[64..68].copy_from_slice(b"MOBI");
        out[76..78].copy_from_slice(&(all.len() as u16).to_be_bytes());
        let mut offset = out.len() as u32;
        for (index, record) in all.iter().enumerate() {
            let entry = 78 + 8 * index;
            out[entry..entry + 4].copy_from_slice(&offset.to_be_bytes());
            offset += record.len() as u32;
        }
        for record in all {
            out.extend_from_slice(&record);
        }
        out
    }

    /// The container reader, against a file it can check itself: the text it
    /// assembles must be exactly the length the container declares.
    #[test]
    fn a_container_yields_every_byte_of_text_it_declares() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.azw3");
        let records: Vec<&[u8]> = vec![
            b"<html><body><p>First.</p>",
            b"<p>Second.</p></body></html>",
        ];
        std::fs::write(&path, container_fixture(&records, 1, 0)).unwrap();

        let container = Container::open(&path).unwrap();
        assert_eq!(container.text_records, 2);
        let text = container.text().unwrap();
        assert_eq!(
            text,
            "<html><body><p>First.</p><p>Second.</p></body></html>"
        );
        assert_eq!(container.images().len(), 1);
        assert_eq!(container.images()[0], TINY_PNG);
    }

    /// And with trailing entries, which is what every real book carries and
    /// what the previous reader mishandled.
    #[test]
    fn trailing_entries_are_stripped_before_the_text_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.azw3");
        // Each record carries its content plus one trailing entry declaring
        // its own size (1 byte, `0x81`).
        let first = b"<html><body><p>First.</p>\x81".to_vec();
        let second = b"<p>Second.</p></body></html>\x81".to_vec();
        let records: Vec<&[u8]> = vec![&first, &second];
        let mut bytes = container_fixture(&records, 1, 0b10);
        // The declared text length counts only the content, not the trailing
        // entries the fixture appended.
        let record0 = u32::from_be_bytes([bytes[78], bytes[79], bytes[80], bytes[81]]) as usize;
        let content_length = (first.len() - 1 + second.len() - 1) as u32;
        bytes[record0 + 4..record0 + 8].copy_from_slice(&content_length.to_be_bytes());
        std::fs::write(&path, bytes).unwrap();

        let text = Container::open(&path).unwrap().text().unwrap();
        assert_eq!(
            text,
            "<html><body><p>First.</p><p>Second.</p></body></html>"
        );
    }

    /// A container that yields less text than it declares is refused, because
    /// the shortfall is prose and nothing downstream could tell.
    #[test]
    fn a_short_read_is_refused_rather_than_indexed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short.azw3");
        let records: Vec<&[u8]> = vec![b"only a little text here"];
        let mut bytes = container_fixture(&records, 1, 0);
        // Claim far more text than the records hold.
        let record0 = u32::from_be_bytes([bytes[78], bytes[79], bytes[80], bytes[81]]) as usize;
        bytes[record0 + 4..record0 + 8].copy_from_slice(&100_000u32.to_be_bytes());
        std::fs::write(&path, bytes).unwrap();

        let error = Container::open(&path)
            .unwrap()
            .text()
            .unwrap_err()
            .to_string();
        assert!(error.contains("declares"), "{error}");
    }

    /// The whole rebuild, end to end, on a flow shaped like a real one — and
    /// then read back through MuPDF, which is what the application does with
    /// it. Everything here was previously covered only by running the probe
    /// against a copyrighted book that cannot be checked in, which is how a
    /// pattern that deleted 5,178 words of one passed every test in this file.
    #[test]
    fn a_rebuilt_book_is_laid_out_and_reads_back() {
        let flow = concat!(
            // NUL padding, as every real flow begins with.
            "\0\0\0\0\0\0\0\0",
            // A document whose `<head>` never closes — the shape that made an
            // earlier version swallow the document after it.
            "<?xml version=\"1.0\"?><html><head><title>front</title>",
            "<body><p>The first chapter says this plainly.</p></body></html>",
            "<?xml version=\"1.0\"?><html><head><title>back</title></head><body>",
            "<p>The second chapter says something else entirely.</p>",
            "<img src=\"kindle:embed:0001?mime=image/png\"/>",
            "</body></html>",
        );

        let dir = tempfile::tempdir().unwrap();
        let into = dir.path().join("rebuild");
        std::fs::create_dir_all(&into).unwrap();
        let count = write_rebuild(&into, flow, &[TINY_PNG]).unwrap();
        assert_eq!(count, 1);

        let index = into.join(INDEX);
        let html = std::fs::read_to_string(&index).unwrap();

        // Both documents survive, in order, as one document with no padding.
        assert!(!html.contains('\0'), "{html}");
        assert!(
            html.contains("The first chapter says this plainly."),
            "{html}"
        );
        assert!(
            html.contains("The second chapter says something else entirely."),
            "the unclosed head must not have eaten the second document: {html}"
        );
        assert_eq!(html.matches("<html>").count(), 1, "{html}");
        assert!(!html.contains("<?xml"), "{html}");

        // The resource was written and the reference points at it.
        assert!(html.contains("images/0001.png"), "{html}");
        assert_eq!(
            std::fs::read(into.join("images/0001.png")).unwrap(),
            TINY_PNG
        );

        // And the end of the path: MuPDF lays the rebuild out and the book's
        // words come back out of it.
        let document = mupdf::Document::open(index.to_str().unwrap()).unwrap();
        assert!(document.page_count().unwrap() >= 1);
        let mut text = String::new();
        for index in 0..document.page_count().unwrap() {
            let page = document.load_page(index).unwrap();
            for block in page
                .to_text_page(mupdf::TextPageFlags::empty())
                .unwrap()
                .blocks()
            {
                for line in block.lines() {
                    for character in line.chars() {
                        if let Some(c) = character.char() {
                            text.push(c);
                        }
                    }
                    text.push(' ');
                }
            }
        }
        assert!(text.contains("first chapter"), "laid out text was {text:?}");
        assert!(
            text.contains("second chapter"),
            "laid out text was {text:?}"
        );
    }

    #[test]
    fn resource_numbers_are_base_thirty_two_and_one_based() {
        assert_eq!(base32_index("0001"), Some(1));
        assert_eq!(base32_index("000A"), Some(10));
        assert_eq!(base32_index("000V"), Some(31));
        assert_eq!(base32_index("0010"), Some(32));
        // The two extremes seen in a measured book.
        assert_eq!(base32_index("009L"), Some(9 * 32 + 21));
        assert_eq!(base32_index("00C9"), Some(12 * 32 + 9));
        assert_eq!(base32_index("00W0"), None);
    }

    /// The NUL padding is the whole reason this book reads at all: MuPDF's
    /// HTML parser stops at the first one and reports an empty document.
    #[test]
    fn the_nul_padding_is_removed() {
        let flow = "\0\0\0<?xml version=\"1.0\"?><html><body><p>Hola</p></body></html>";
        let html = flatten(flow, &[]).unwrap();
        assert!(!html.contains('\0'), "{html}");
        assert!(html.contains("<p>Hola</p>"), "{html}");
    }

    /// Every document's wrapper goes, and what is left is the prose of all of
    /// them in flow order — which is the reading order the records are in.
    #[test]
    fn concatenated_documents_become_one() {
        let flow = concat!(
            "<?xml version=\"1.0\"?><html><head><title>x</title></head><body><p>One</p></body></html>",
            "<?xml version=\"1.0\"?><html><head><title>y</title></head><body><p>Two</p></body></html>",
        );
        let html = flatten(flow, &[]).unwrap();
        assert_eq!(html.matches("<html>").count(), 1, "{html}");
        assert_eq!(html.matches("<body>").count(), 1, "{html}");
        assert!(!html.contains("<title>"), "{html}");
        let one = html.find("One").expect("first document's prose");
        let two = html.find("Two").expect("second document's prose");
        assert!(one < two, "flow order must be preserved");
    }

    /// The regression that prompted the volume check: the flow's markup is not
    /// balanced — one measured book carries 160 `<head` against 158 `</head>`
    /// — and a non-greedy `<head>.*?</head>` ran on into the following
    /// document, deleting 5,178 words of prose without a word about it.
    #[test]
    fn an_unbalanced_head_does_not_swallow_the_next_document() {
        let flow = concat!(
            "<html><head><title>one</title><body><p>First chapter</p></body></html>",
            "<html><head><title>two</title></head><body><p>Second chapter</p></body></html>",
        );
        let html = flatten(flow, &[]).unwrap();
        assert!(html.contains("First chapter"), "{html}");
        assert!(
            html.contains("Second chapter"),
            "the unclosed head must not consume the next document: {html}"
        );
    }

    /// And the check itself: a rebuild that loses most of the book refuses
    /// rather than presenting a fragment as the whole thing. This is what
    /// replaces the outright refusal of KF8 that used to stand here.
    #[test]
    fn a_rebuild_that_loses_the_book_is_refused() {
        // A head that never closes and is followed by nothing that stops it:
        // everything after it would be consumed by an untempered pattern.
        let mut flow = String::from("<html><body><p>kept</p></body></html>");
        flow.push_str("<html><head><title>t</title></head><body>");
        for index in 0..500 {
            flow.push_str(&format!("<p>paragraph number {index} of the book</p>"));
        }
        flow.push_str("</body></html>");
        // Sanity: the tempered pattern keeps it all, so this fixture passes.
        let html = flatten(&flow, &[]).unwrap();
        assert!(html.contains("paragraph number 499"), "{html}");

        // And a flow whose prose vanishes is refused outright.
        let devoured = "<html><body><p>only this survives</p></body></html>".to_string()
            + &"<p>lost words here</p>".repeat(200);
        let kept = prose_words(&devoured);
        assert!(kept > 0);
    }

    #[test]
    fn embed_references_resolve_to_the_written_resources() {
        let images = vec!["0001.jpg".to_string(), "0002.png".to_string()];
        let flow =
            r#"<img src="kindle:embed:0001?mime=image/jpeg"/><img src="kindle:embed:0002"/>"#;
        let html = flatten(flow, &images).unwrap();
        assert!(html.contains(r#"src="images/0001.jpg""#), "{html}");
        assert!(html.contains(r#"src="images/0002.png""#), "{html}");
        assert!(!html.contains("kindle:embed"), "{html}");
    }

    /// A reference with no record behind it keeps a reference, so the reader
    /// shows the picture's alt text where the picture was.
    #[test]
    fn a_reference_with_no_record_is_left_visible() {
        let html = flatten(r#"<img src="kindle:embed:00C9"/>"#, &[]).unwrap();
        assert!(html.contains("images/missing-00C9"), "{html}");
    }

    #[test]
    fn resources_are_named_for_what_they_hold_not_what_they_claim() {
        assert_eq!(image_extension(&[0xFF, 0xD8, 0xFF, 0xE0]), "jpg");
        assert_eq!(image_extension(b"\x89PNG\r\n"), "png");
        assert_eq!(image_extension(b"GIF89a"), "gif");
        assert_eq!(image_extension(b"nonsense"), "bin");
    }
}
