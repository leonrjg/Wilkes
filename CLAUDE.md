Read and obey my user-wide CLAUDE.md. Do not use sub-agents.

# Wilkes
- This is a GUI built on Tauri to search across multiple files, prioritizing PDFs.

## Global Instructions
- Do not add fallbacks or alternative implementations unless explicitly instructed.
- Do not silently suppress exceptions; always log them at least.
- Prefer to extend existing components rather than creating new ones.

## Cancellation pattern
Cancellable long-running operations use `tokio::select!` at the call site, not cooperative token checks inside the operation:

```rust
let result = tokio::select! {
    biased;
    _ = cancel_token.cancelled() => Err(anyhow::anyhow!("cancelled")),
    result = do_work(...) => result,
};
```

`do_work` runs to completion and has no knowledge of cancellation. The select drops the future at its next await point when cancel fires. Do not add `cancel.is_cancelled()` checks inside work functions.
