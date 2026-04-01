use wilkes_core::types::{ByteRange, MatchRef, PreviewData, SourceOrigin};

/// Load preview data for a match.
pub async fn preview(match_ref: MatchRef) -> anyhow::Result<PreviewData> {
    match &match_ref.origin {
        SourceOrigin::TextFile { line, .. } => preview_text(&match_ref, *line).await,
        SourceOrigin::PdfPage { page, bbox } => {
            preview_pdf(&match_ref, *page, bbox.clone()).await
        }
    }
}

async fn preview_text(match_ref: &MatchRef, highlight_line: u32) -> anyhow::Result<PreviewData> {
    let content = tokio::fs::read_to_string(&match_ref.path).await?;
    let language = detect_language(&match_ref.path);

    // Compute the character range of the matched line so the frontend can
    // highlight it. For Phase 1 we highlight the whole line; Phase 2 adds
    // column-level precision via MatchRef.
    let highlight_range = line_range(&content, highlight_line);

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
