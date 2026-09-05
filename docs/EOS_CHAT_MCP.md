# MCP access for EOS chat

The shared Wilkes HTTP API mounts `/mcp` using the same MCP implementation as
Wilkes chat and the external MCP server. EOS uses its configured Wilkes base URL;
there is no additional port, independent library, or copied MCP tool catalogue.
Both headless and desktop HTTP servers supply the workspace catalogue. Test-only
routers without a catalogue omit this route.

The route rejects browser Origins and retains rmcp's default localhost Host
validation. It has the same trusted-local-service deployment assumptions as the
HTTP API; Host validation is not authentication for a publicly exposed service.
The separately configured external MCP server retains its existing bearer-token
and Host policy.

Existing document/search/literature/download capabilities are unchanged.
`read_library` adds paged reads of bookmarks, tags and recent search history.
`edit_library` describes a typed union of actions for bookmarks, tags, document
tagging, smart collections, file rename and metadata refresh. They delegate to
`AppContext` and its existing research store. Smart collections use the existing
CEL schema; bookmark locations use 1-based page/line numbers.

All edits refuse managed read-only workspaces. Paths are canonicalized and
checked against the selected workspace's library roots, including symlink
resolution. Rename permits files only and never overwrites an existing target.
The ordinary rename path now updates bookmark source paths transactionally with
research references, preserving annotations and identity for unindexed files too.

Successful edits emit `research-state-updated`. Desktop and HTTP subscribers
refresh the existing bookmark, research and file stores, so an edit initiated by
EOS is visible in Wilkes without reopening it. There is no inference in this
adapter and no change to indexing-worker ownership or job lifecycles.

Validation includes agent tests for managed-workspace refusal, path confinement,
pagination and HTTP MCP initialization; an API round trip through actual research
storage and rename; and the UI notification subscription/cleanup test.
