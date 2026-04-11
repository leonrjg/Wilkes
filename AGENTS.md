# Rules
- When implementing a proposal, try to fulfill it completely in one go rather than splitting it in multiple steps requiring user prompts.
- You do what you offer: if you identify the addition or modification of a mechanism as necessary, and the user doesn't challenge it, you must either implement it in the current effort or explicitly say what you will implement.
- When a change introduces a second mechanism for the same responsibility, stop and either remove the old one in the same effort or clearly label the work as partial and ask before continuing.
- Before making multi-step architectural changes, explicitly restate the invariant being improved in concrete terms and preserve focus on that invariant throughout the work.
- In status updates and final summaries, distinguish clearly between “completed”, “partially migrated”, and “still duplicated”. Never present partial convergence as completion.

## Rust Guidelines
- Never index or slice strings by byte offset; always use character-aware method. Byte indexing (&s[..n]) is only safe when you can prove the offset is a char boundary, which is almost never true for arbitrary runtime strings.
