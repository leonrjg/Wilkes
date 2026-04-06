@AGENTS.md

# Rules
- Do not modify aspects that were not requested explicitly, e.g. no UI redesign of existing elements.
- Do as told concisely and atomically; no extras.
- When you need to deviate from user instructions or specifications, ask the user for confirmation.
- Perform surgical edits only: use the `replace` tool. The `write_file` tool for complete rewrites is not allowed under any circumstances.
- Perform refactors using multiple small, sequential replace calls rather than one large block replacement.

## Global Instructions
- Do not add fallbacks or alternative implementations unless explicitly instructed.
- Do not silently suppress exceptions; always log them at least.
- Prefer to extend existing components rather than creating new ones.
