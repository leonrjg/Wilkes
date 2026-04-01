Use Gemini MCP as a sub-agent for codebase exploration or any use case in which you need sub-agents - your own subagents are forbidden, do not use them.
Offload any tasks whose intermediate states you don't need to Gemini, specifying model = "gemini-3-flash-preview".
You remain responsible for doing the final analysis of root causes and cost-benefit; Gemini only provides information - give it quick queries.

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
