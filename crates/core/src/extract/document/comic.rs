//! CBZ directory metadata is cached; image bytes and decoded pages are not.
//! Page names follow MuPDF's ASCII case-insensitive natural ordering so indexed
//! page numbers and coordinates remain valid. Entries never reach the filesystem.
use anyhow::{ensure, Context};
use std::{
    cmp::Ordering,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Instant, SystemTime},
};

const MAX_PAGE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ARCHIVES: usize = 8;
const IMAGE_EXTENSIONS: &[&str] = &[
    "bmp", "gif", "hdp", "j2k", "jb2", "jbig2", "jp2", "jpeg", "jpg", "jpx", "jxr", "pam", "pbm",
    "pgm", "pkm", "png", "pnm", "ppm", "tif", "tiff", "wdp",
];

#[derive(Clone, PartialEq, Eq)]
struct Identity {
    path: PathBuf,
    modified: SystemTime,
    len: u64,
}
struct Archive {
    identity: Identity,
    entries: Vec<(String, usize)>,
}
static ARCHIVES: Mutex<Vec<Arc<Archive>>> = Mutex::new(Vec::new());

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut a = a.bytes().peekable();
    let mut b = b.bytes().peekable();
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                // Compare digit runs without integer overflow, including very long names.
                let x: Vec<_> = std::iter::from_fn(|| a.next_if(u8::is_ascii_digit))
                    .skip_while(|v| *v == b'0')
                    .collect();
                let y: Vec<_> = std::iter::from_fn(|| b.next_if(u8::is_ascii_digit))
                    .skip_while(|v| *v == b'0')
                    .collect();
                let order = x.len().cmp(&y.len()).then_with(|| x.cmp(&y));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                a.next();
                b.next();
                // MuPDF compares C chars, whose signedness is target-dependent.
                let upper = |v: u8| v.to_ascii_uppercase() as std::ffi::c_char as i32;
                let order = upper(x).cmp(&upper(y));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
        }
    }
}

fn archive(path: &Path) -> anyhow::Result<Arc<Archive>> {
    ensure!(
        super::format::PagedFormat::pages_on_demand(path),
        "not a CBZ archive: {}",
        path.display()
    );
    let metadata = std::fs::metadata(path)?;
    let identity = Identity {
        path: path.to_path_buf(),
        modified: metadata.modified()?,
        len: metadata.len(),
    };
    // Initializers share this short directory read. No page decode or extraction
    // happens under the cache lock, and concurrent readers use independent files.
    let mut cache = ARCHIVES.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(index) = cache.iter().position(|v| v.identity == identity) {
        let found = cache.remove(index);
        cache.push(found.clone());
        return Ok(found);
    }
    let mut zip = zip::ZipArchive::new(File::open(path)?)?;
    let mut entries = Vec::new();
    for index in 0..zip.len() {
        let entry = zip.by_index_raw(index)?;
        let name = entry.name();
        if !entry.is_dir()
            && Path::new(name)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| IMAGE_EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e)))
        {
            entries.push((name.to_owned(), index));
        }
    }
    entries.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    ensure!(!entries.is_empty(), "comic contains no supported images");
    let found = Arc::new(Archive { identity, entries });
    cache.retain(|v| v.identity.path != path);
    cache.push(found.clone());
    if cache.len() > MAX_ARCHIVES {
        cache.remove(0);
    }
    Ok(found)
}

pub fn page_count(path: &Path) -> anyhow::Result<u32> {
    Ok(archive(path)?.entries.len().try_into()?)
}

/// WCP1, width/height in MuPDF points (little endian f32), image kind (u32),
/// then original JPEG/PNG bytes. Other MuPDF image formats are explicitly
/// normalized to PNG, since webviews cannot decode the full MuPDF format set.
/// The binary framing keeps megabytes of page data out of JSON on both shells.
pub fn page(path: &Path, number: u32) -> anyhow::Result<Vec<u8>> {
    let archive = archive(path)?;
    ensure!(number > 0, "pages are 1-based");
    let (name, index) = archive
        .entries
        .get(number as usize - 1)
        .context("comic page out of range")?;
    let mut zip = zip::ZipArchive::new(File::open(path)?)?;
    let mut entry = zip.by_index(*index)?;
    ensure!(entry.name() == name, "comic changed while opening page");
    ensure!(entry.size() <= MAX_PAGE_BYTES, "comic page exceeds 64 MiB");
    let started = Instant::now();
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        ensure!(
            started.elapsed().as_secs_f32() < 2.0,
            "comic page read exceeded two seconds"
        );
        let read = entry.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        ensure!(
            bytes.len() + read <= MAX_PAGE_BYTES as usize,
            "comic page exceeds 64 MiB"
        );
        bytes.extend_from_slice(&chunk[..read]);
    }
    let doc = mupdf::Document::from_bytes(&bytes, name)?;
    let loaded = doc.load_page(0)?;
    let bounds = loaded.bounds()?;
    let width = bounds.x1 - bounds.x0;
    let height = bounds.y1 - bounds.y0;
    ensure!(
        width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0,
        "invalid comic page dimensions"
    );
    let kind: u32 = if bytes.starts_with(&[0xff, 0xd8]) {
        1
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        2
    } else {
        let scale = (2048.0 / width.max(height)).min(1.0);
        let pixmap = loaded.to_pixmap(
            &mupdf::Matrix::new_scale(scale, scale),
            &mupdf::Colorspace::device_rgb(),
            false,
            false,
        )?;
        bytes.clear();
        pixmap.write_to(&mut bytes, mupdf::ImageFormat::PNG)?;
        2
    };
    let mut packet = Vec::with_capacity(16 + bytes.len());
    packet.extend_from_slice(b"WCP1");
    packet.extend_from_slice(&width.to_le_bytes());
    packet.extend_from_slice(&height.to_le_bytes());
    packet.extend_from_slice(&kind.to_le_bytes());
    packet.extend_from_slice(&bytes);
    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn comic() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.cbz");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        for (index, name) in ["頁10.png", "頁2.png", "notes.txt", "PAGE1.PNG"]
            .iter()
            .enumerate()
        {
            let image = image::RgbImage::new(40 + index as u32, 60 + index as u32);
            let mut png = std::io::Cursor::new(Vec::new());
            image.write_to(&mut png, image::ImageFormat::Png).unwrap();
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(png.get_ref()).unwrap();
        }
        zip.finish().unwrap();
        (dir, path)
    }
    #[test]
    fn directory_and_original_page_agree_with_mupdf() {
        let (_dir, path) = comic();
        assert_eq!(page_count(&path).unwrap(), 3);
        let a = archive(&path).unwrap();
        assert!(
            a.entries.iter().position(|e| e.0 == "頁2.png")
                < a.entries.iter().position(|e| e.0 == "頁10.png")
        );
        let doc = mupdf::Document::open(path.to_str().unwrap()).unwrap();
        for number in 1..=3 {
            let packet = page(&path, number).unwrap();
            assert_eq!(&packet[..4], b"WCP1");
            let bounds = doc.load_page(number as i32 - 1).unwrap().bounds().unwrap();
            assert_eq!(
                f32::from_le_bytes(packet[4..8].try_into().unwrap()),
                bounds.x1
            );
            assert_eq!(
                f32::from_le_bytes(packet[8..12].try_into().unwrap()),
                bounds.y1
            );
            assert!(packet[16..].starts_with(b"\x89PNG"));
        }
        assert!(page(&path, 0).is_err());
        assert!(page(&path, 4).is_err());
    }
    #[test]
    fn concurrent_opens_share_directory_and_replacement_invalidates_it() {
        let (_dir, path) = comic();
        let first = archive(&path).unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| assert!(Arc::ptr_eq(&first, &archive(&path).unwrap())));
            }
        });
        std::fs::write(&path, b"invalid replacement").unwrap();
        assert!(page_count(&path).is_err());
    }
}
