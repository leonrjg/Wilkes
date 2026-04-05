import sys
from pathlib import Path
from typing import Optional, List, Tuple, Any

SUPPORTED_EXTENSIONS = {
    ".txt", ".md", ".markdown", ".rst", ".py", ".js", ".ts", ".jsx", ".tsx",
    ".java", ".c", ".cpp", ".h", ".cs", ".go", ".rs", ".rb", ".swift", ".kt",
    ".html", ".css", ".json", ".yaml", ".yml", ".toml", ".xml", ".sh",
    ".pdf",
}

def extract_text(path: Path) -> Optional[str]:
    suffix = path.suffix.lower()
    if suffix == ".pdf":
        try:
            import fitz  # PyMuPDF
            doc = fitz.open(path)
            text = ""
            for page in doc:
                text += page.get_text()
            return text
        except Exception as e:
            sys.stderr.write(f"Error extracting PDF: {e}\n")
            return None
    else:
        try:
            with open(path, "r", encoding="utf-8", errors="ignore") as f:
                return f.read()
        except Exception as e:
            sys.stderr.write(f"Error reading file: {e}\n")
            return None

def extract_chunks(files: List[Path], splitter: Any) -> List[Tuple[Path, int, str, int, int, int]]:
    # Collect all chunks before embedding so the engine receives the full corpus
    # in one call and can batch optimally across all files.
    # Each entry: (path, chunk_idx, chunk_text, byte_start, byte_end, line_num)
    all_chunks = []
    for path in files:
        try:
            text = extract_text(path)
            if not text:
                continue
            for idx, (char_offset, chunk_text) in enumerate(splitter.chunk_indices(text)):
                byte_start = len(text[:char_offset].encode('utf-8'))
                byte_end = byte_start + len(chunk_text.encode('utf-8'))
                line_num = text[:char_offset].count('\n') + 1
                all_chunks.append((path, idx, chunk_text, byte_start, byte_end, line_num))
        except Exception as e:
            sys.stderr.write(f"Error extracting {path}: {e}\n")
    return all_chunks
