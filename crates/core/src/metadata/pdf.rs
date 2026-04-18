use std::path::Path;

use chrono::{Datelike, FixedOffset, TimeZone};
use mupdf::{Document, MetadataName, TextPageFlags};
use regex::Regex;

use crate::types::DocumentMetadata;

use super::doi::find_doi;
use super::FileMetadataExtractor;

pub struct PdfMetadataExtractor;

impl FileMetadataExtractor for PdfMetadataExtractor {
    fn can_handle(&self, path: &Path, _mime: Option<&str>) -> bool {
        path.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
    }

    fn extract_metadata(&self, path: &Path) -> anyhow::Result<DocumentMetadata> {
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 path"))?;

        let doc = Document::open(path_str)?;

        Ok(DocumentMetadata {
            title: read_non_empty_metadata(&doc, MetadataName::Title),
            author: read_non_empty_metadata(&doc, MetadataName::Author),
            doi: extract_pdf_doi(&doc),
            created_at: read_non_empty_metadata(&doc, MetadataName::CreationDate)
                .and_then(|value| normalize_pdf_creation_date(&value)),
        })
    }
}

fn read_non_empty_metadata(doc: &Document, name: MetadataName) -> Option<String> {
    doc.metadata(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn extract_pdf_doi(doc: &Document) -> Option<String> {
    find_embedded_doi(doc).or_else(|| find_first_page_doi(doc))
}

fn normalize_pdf_creation_date(value: &str) -> Option<String> {
    let captures = Regex::new(
        r"^D:(\d{4})(\d{2})(\d{2})(\d{2})(\d{2})(\d{2})([+-])(\d{2})'(\d{2})'$",
    )
    .ok()?
    .captures(value)?;

    let year = captures.get(1)?.as_str().parse::<i32>().ok()?;
    let month = captures.get(2)?.as_str().parse::<u32>().ok()?;
    let day = captures.get(3)?.as_str().parse::<u32>().ok()?;
    let hour = captures.get(4)?.as_str().parse::<u32>().ok()?;
    let minute = captures.get(5)?.as_str().parse::<u32>().ok()?;
    let second = captures.get(6)?.as_str().parse::<u32>().ok()?;
    let tz_sign = captures.get(7)?.as_str();
    let tz_hours = captures.get(8)?.as_str().parse::<i32>().ok()?;
    let tz_minutes = captures.get(9)?.as_str().parse::<i32>().ok()?;
    let offset_seconds = tz_hours
        .checked_mul(3600)?
        .checked_add(tz_minutes.checked_mul(60)?)?;
    let signed_offset = if tz_sign == "-" {
        -offset_seconds
    } else {
        offset_seconds
    };
    let offset = FixedOffset::east_opt(signed_offset)?;
    let parsed = offset
        .with_ymd_and_hms(year, month, day, hour, minute, second)
        .single()?;

    Some(format!("{:04}-{:02}", parsed.year(), parsed.month()))
}

fn find_embedded_doi(doc: &Document) -> Option<String> {
    [
        MetadataName::Keywords,
        MetadataName::Subject,
        MetadataName::Title,
        MetadataName::Author,
        MetadataName::Creator,
        MetadataName::Producer,
    ]
    .into_iter()
    .filter_map(|name| read_non_empty_metadata(doc, name))
    .find_map(|value| find_doi(&value))
}

fn find_first_page_doi(doc: &Document) -> Option<String> {
    let page = doc.load_page(0).ok()?;
    let text_page = page.to_text_page(TextPageFlags::empty()).ok()?;
    let text = text_page.to_text().ok()?;
    find_doi(&text)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_pdf_metadata_extractor_reads_title_and_author() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("metadata.pdf");
        let pdf_base64 = "JVBERi0xLjQKMSAwIG9iago8PCAvVHlwZSAvQ2F0YWxvZyAvUGFnZXMgMiAwIFIgPj4KZW5kb2JqCjIgMCBvYmoKPDwgL1R5cGUgL1BhZ2VzIC9LaWRzIFszIDAgUl0gL0NvdW50IDEgPj4KZW5kb2JqCjMgMCBvYmoKPDwgL1R5cGUgL1BhZ2UgL1BhcmVudCAyIDAgUiAvTWVkaWFCb3ggWzAgMCAzMDAgMTQ0XSAvQ29udGVudHMgNCAwIFIgL1Jlc291cmNlcyA8PCAvRm9udCA8PCAvRjEgNiAwIFIgPj4gPj4gPj4KZW5kb2JqCjQgMCBvYmoKPDwgL0xlbmd0aCA0NSA+PgpzdHJlYW0KQlQKL0YxIDE4IFRmCjUwIDgwIFRkCihIZWxsbyBNZXRhZGF0YSkgVGoKRVQKZW5kc3RyZWFtCmVuZG9iago1IDAgb2JqCjw8IC9UaXRsZSAoVGVzdCBUaXRsZSkgL0F1dGhvciAoVGVzdCBBdXRob3IpID4+CmVuZG9iago2IDAgb2JqCjw8IC9UeXBlIC9Gb250IC9TdWJ0eXBlIC9UeXBlMSAvQmFzZUZvbnQgL0hlbHZldGljYSA+PgplbmRvYmoKeHJlZgowIDcKMDAwMDAwMDAwMCA2NTUzNSBmIAowMDAwMDAwMDA5IDAwMDAwIG4gCjAwMDAwMDAwNTggMDAwMDAgbiAKMDAwMDAwMDExNSAwMDAwMCBuIAowMDAwMDAwMjQxIDAwMDAwIG4gCjAwMDAwMDAzMzUgMDAwMDAgbiAKMDAwMDAwMDM5OCAwMDAwMCBuIAp0cmFpbGVyCjw8IC9TaXplIDcgL1Jvb3QgMSAwIFIgL0luZm8gNSAwIFIgPj4Kc3RhcnR4cmVmCjQ2OAolJUVPRgo=";
        fs::write(&path, STANDARD.decode(pdf_base64).unwrap()).unwrap();

        let metadata = PdfMetadataExtractor.extract_metadata(&path).unwrap();

        assert_eq!(metadata.title.as_deref(), Some("Test Title"));
        assert_eq!(metadata.author.as_deref(), Some("Test Author"));
        assert_eq!(metadata.doi, None);
        assert_eq!(metadata.created_at, None);
    }

    #[test]
    fn test_pdf_metadata_extractor_reads_embedded_doi() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("metadata-doi.pdf");
        let pdf_base64 = "JVBERi0xLjQKMSAwIG9iago8PCAvVHlwZSAvQ2F0YWxvZyAvUGFnZXMgMiAwIFIgPj4KZW5kb2JqCjIgMCBvYmoKPDwgL1R5cGUgL1BhZ2VzIC9LaWRzIFszIDAgUl0gL0NvdW50IDEgPj4KZW5kb2JqCjMgMCBvYmoKPDwgL1R5cGUgL1BhZ2UgL1BhcmVudCAyIDAgUiAvTWVkaWFCb3ggWzAgMCAzMDAgMTQ0XSAvQ29udGVudHMgNCAwIFIgL1Jlc291cmNlcyA8PCAvRm9udCA8PCAvRjEgNiAwIFIgPj4gPj4gPj4KZW5kb2JqCjQgMCBvYmoKPDwgL0xlbmd0aCA0NSA+PgpzdHJlYW0KQlQKL0YxIDE4IFRmCjUwIDgwIFRkCihIZWxsbyBNZXRhZGF0YSkgVGoKRVQKZW5kc3RyZWFtCmVuZG9iago1IDAgb2JqCjw8IC9UaXRsZSAoVGVzdCBUaXRsZSkgL0F1dGhvciAoVGVzdCBBdXRob3IpIC9LZXl3b3JkcyAoZG9pOjEwLjEwMDAveHl6MTIzKSA+PgplbmRvYmoKNiAwIG9iago8PCAvVHlwZSAvRm9udCAvU3VidHlwZSAvVHlwZTEgL0Jhc2VGb250IC9IZWx2ZXRpY2EgPj4KZW5kb2JqCnhyZWYKMCA3CjAwMDAwMDAwMDAgNjU1MzUgZiAKMDAwMDAwMDAwOSAwMDAwMCBuIAowMDAwMDAwMDU4IDAwMDAwIG4gCjAwMDAwMDAxMTUgMDAwMDAgbiAKMDAwMDAwMDI0MSAwMDAwMCBuIAowMDAwMDAwMzM1IDAwMDAwIG4gCjAwMDAwMDA0MjggMDAwMDAgbiAKdHJhaWxlcgo8PCAvU2l6ZSA3IC9Sb290IDEgMCBSIC9JbmZvIDUgMCBSID4+CnN0YXJ0eHJlZgo0OTgKJSVFT0YK";
        fs::write(&path, STANDARD.decode(pdf_base64).unwrap()).unwrap();

        let metadata = PdfMetadataExtractor.extract_metadata(&path).unwrap();
        assert_eq!(metadata.doi.as_deref(), Some("10.1000/xyz123"));
        assert_eq!(metadata.created_at, None);
    }

    #[test]
    fn test_pdf_metadata_extractor_falls_back_to_first_page_doi() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("page-doi.pdf");
        let pdf_base64 = "JVBERi0xLjQKMSAwIG9iago8PCAvVHlwZSAvQ2F0YWxvZyAvUGFnZXMgMiAwIFIgPj4KZW5kb2JqCjIgMCBvYmoKPDwgL1R5cGUgL1BhZ2VzIC9LaWRzIFszIDAgUl0gL0NvdW50IDEgPj4KZW5kb2JqCjMgMCBvYmoKPDwgL1R5cGUgL1BhZ2UgL1BhcmVudCAyIDAgUiAvTWVkaWFCb3ggWzAgMCAzMDAgMTQ0XSAvQ29udGVudHMgNCAwIFIgL1Jlc291cmNlcyA8PCAvRm9udCA8PCAvRjEgNSAwIFIgPj4gPj4gPj4KZW5kb2JqCjQgMCBvYmoKPDwgL0xlbmd0aCA1MCA+PgpzdHJlYW0KQlQKL0YxIDEyIFRmCjUwIDgwIFRkCihET0k6IDEwLjEwMDAveHl6MTIzKSBUagpFVAplbmRzdHJlYW0KZW5kb2JqCjUgMCBvYmoKPDwgL1R5cGUgL0ZvbnQgL1N1YnR5cGUgL1R5cGUxIC9CYXNlRm9udCAvSGVsdmV0aWNhID4+CmVuZG9iagp4cmVmCjAgNgowMDAwMDAwMDAwIDY1NTM1IGYgCjAwMDAwMDAwMDkgMDAwMDAgbiAKMDAwMDAwMDA1OCAwMDAwMCBuIAowMDAwMDAwMTE1IDAwMDAwIG4gCjAwMDAwMDAyNDEgMDAwMDAgbiAKMDAwMDAwMDM0MSAwMDAwMCBuIAp0cmFpbGVyCjw8IC9TaXplIDYgL1Jvb3QgMSAwIFIgPj4Kc3RhcnR4cmVmCjQxMQolJUVPRgo=";
        fs::write(&path, STANDARD.decode(pdf_base64).unwrap()).unwrap();

        let metadata = PdfMetadataExtractor.extract_metadata(&path).unwrap();
        assert_eq!(metadata.doi.as_deref(), Some("10.1000/xyz123"));
        assert_eq!(metadata.created_at, None);
    }

    #[test]
    fn test_pdf_metadata_extractor_normalizes_creation_date() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("creation-date.pdf");
        let pdf_base64 = "JVBERi0xLjQKMSAwIG9iago8PCAvVHlwZSAvQ2F0YWxvZyAvUGFnZXMgMiAwIFIgPj4KZW5kb2JqCjIgMCBvYmoKPDwgL1R5cGUgL1BhZ2VzIC9LaWRzIFszIDAgUl0gL0NvdW50IDEgPj4KZW5kb2JqCjMgMCBvYmoKPDwgL1R5cGUgL1BhZ2UgL1BhcmVudCAyIDAgUiAvTWVkaWFCb3ggWzAgMCAzMDAgMTQ0XSAvQ29udGVudHMgNCAwIFIgL1Jlc291cmNlcyA8PCAvRm9udCA8PCAvRjEgNiAwIFIgPj4gPj4gPj4KZW5kb2JqCjQgMCBvYmoKPDwgL0xlbmd0aCA0NSA+PgpzdHJlYW0KQlQKL0YxIDE4IFRmCjUwIDgwIFRkCihIZWxsbyBNZXRhZGF0YSkgVGoKRVQKZW5kc3RyZWFtCmVuZG9iago1IDAgb2JqCjw8IC9UaXRsZSAoVGVzdCBUaXRsZSkgL0NyZWF0aW9uRGF0ZSAoRDoyMDI1MDQwNzE2Mjg1Ny0wMycwMCcpID4+CmVuZG9iago2IDAgb2JqCjw8IC9UeXBlIC9Gb250IC9TdWJ0eXBlIC9UeXBlMSAvQmFzZUZvbnQgL0hlbHZldGljYSA+PgplbmRvYmoKeHJlZgowIDcKMDAwMDAwMDAwMCA2NTUzNSBmIAowMDAwMDAwMDA5IDAwMDAwIG4gCjAwMDAwMDAwNTggMDAwMDAgbiAKMDAwMDAwMDExNSAwMDAwMCBuIAowMDAwMDAwMjQxIDAwMDAwIG4gCjAwMDAwMDAzMzUgMDAwMDAgbiAKMDAwMDAwMDQxMiAwMDAwMCBuIAp0cmFpbGVyCjw8IC9TaXplIDcgL1Jvb3QgMSAwIFIgL0luZm8gNSAwIFIgPj4Kc3RhcnR4cmVmCjQ4MgolJUVPRgo=";
        fs::write(&path, STANDARD.decode(pdf_base64).unwrap()).unwrap();

        let metadata = PdfMetadataExtractor.extract_metadata(&path).unwrap();
        assert_eq!(metadata.created_at.as_deref(), Some("2025-04"));
    }

    #[test]
    fn test_pdf_metadata_extractor_ignores_unsupported_creation_date_shape() {
        assert_eq!(
            normalize_pdf_creation_date("Mon Apr  7 16:28:57 2025 -03"),
            None
        );
        assert_eq!(
            normalize_pdf_creation_date("D:20250407162857-03'00'"),
            Some("2025-04".into())
        );
    }

    #[test]
    fn test_pdf_metadata_extractor_rejects_non_pdf_extension() {
        assert!(!PdfMetadataExtractor.can_handle(Path::new("notes.txt"), None));
    }
}
