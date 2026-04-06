## Rust Guidelines
- Never index or slice strings by byte offset; always use character-aware method. Byte indexing (&s[..n]) is only safe when you can prove the offset is a char boundary, which is almost never true for arbitrary runtime strings.
