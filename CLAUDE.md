@AGENTS.md

Use Gemini MCP as a sub-agent for codebase exploration or any use case in which you need sub-agents - your own subagents are forbidden, do not use them.
Offload any tasks whose intermediate states you don't need to Gemini. Tools such as grep do not need to be offloaded.
- Specify model "gemini-2.5-flash-lite" or it will crash. 
You remain responsible for doing the final analysis of root causes and cost-benefit; Gemini only provides information - give it quick queries.

# Wilkes
- This is a GUI built on Tauri to search across multiple files, prioritizing PDFs.

## Global Instructions
- Do not add fallbacks or alternative implementations unless explicitly instructed.
- Do not silently suppress exceptions; always log them at least.
- Prefer to extend existing components rather than creating new ones.
