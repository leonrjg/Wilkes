use wilkes_core::types::{ByteRange, MatchRef, PreviewData, SourceOrigin};

/// Load preview data for a match.
pub async fn preview(match_ref: MatchRef) -> anyhow::Result<PreviewData> {
    match &match_ref.origin {
        SourceOrigin::TextFile { .. } => preview_text(&match_ref).await,
        SourceOrigin::PdfPage { page, bbox } => {
            preview_pdf(&match_ref, *page, bbox.clone()).await
        }
    }
}

async fn preview_text(match_ref: &MatchRef) -> anyhow::Result<PreviewData> {
    let content = tokio::fs::read_to_string(&match_ref.path).await?;
    let language = detect_language(&match_ref.path);

    let (highlight_line, highlight_range) = if let Some(range) = &match_ref.text_range {
        // Compute line number from byte offset
        let line = content[..range.start.min(content.len())].lines().count() as u32;
        // Adjust for potential missing trailing newline on last line or empty file
        let highlight_line = if line == 0 { 1 } else { line as u32 };

        // Convert byte range to UTF-16 code unit range for the frontend (JS/CodeMirror)
        let utf16_start = content[..range.start.min(content.len())].encode_utf16().count();
        let utf16_len = content[range.start.min(content.len())..range.end.min(content.len())]
            .encode_utf16()
            .count();
        let highlight_range = ByteRange {
            start: utf16_start,
            end: utf16_start + utf16_len,
        };

        (highlight_line, highlight_range)
    } else {
        let line = match &match_ref.origin {
            SourceOrigin::TextFile { line, .. } => *line,
            _ => 1,
        };
        (line, line_range(&content, line))
    };

    Ok(PreviewData::Text {
        content,
        language,
        highlight_line,
        highlight_range,
    })
}

async fn preview_pdf(
    _match_ref: &MatchRef,
    page: u32,
    highlight_bbox: Option<wilkes_core::types::BoundingBox>,
) -> anyhow::Result<PreviewData> {
    // The frontend loads the file directly via the asset protocol (convertFileSrc).
    // No byte transfer over IPC.
    Ok(PreviewData::Pdf { page, highlight_bbox })
}

/// Detect a language hint for CodeMirror syntax highlighting.
pub fn detect_language(path: &std::path::Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    let lang = match ext.to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "py" => "python",
        "js" | "jsx" => "javascript",
        "ts" | "tsx" => "typescript",
        "json" | "jsonl" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" | "scss" | "sass" => "css",
        "xml" => "xml",
        "sql" => "sql",
        "sh" | "bash" | "zsh" => "shell",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "rb" => "ruby",
        "swift" => "swift",
        "kt" => "kotlin",
        "cs" => "csharp",
        _ => return None,
    };
    Some(lang.into())
}

/// Return the byte range (in the whole file string) of the given 1-based line.
fn line_range(content: &str, line: u32) -> ByteRange {
    let target = line.saturating_sub(1) as usize;
    let mut offset = 0usize;
    for (i, l) in content.lines().enumerate() {
        if i == target {
            return ByteRange {
                start: offset,
                end: offset + l.len(),
            };
        }
        offset += l.len() + 1; // +1 for '\n'
    }
    ByteRange { start: 0, end: 0 }
}
