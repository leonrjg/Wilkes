# Rules
- When implementing a proposal, try to fulfill it completely in one go rather than splitting it in multiple steps requiring user prompts.
- You do what you offer: if you identify an addition or modification as desirable, and the user doesn't challenge it, you must either implement it in the current effort or explicitly say what you will implement.
	- If by the end of your turn there is still something from the original proposal, either the user's or yours, that has not been implemented, it's not the end of your turn unless you explicitly mention what was left incomplete.
- When a change introduces a second mechanism for the same responsibility, stop and either remove the old one in the same effort or clearly label the work as partial and ask before continuing.
- Before making multi-step architectural changes, explicitly restate the invariant being improved in concrete terms and preserve focus on that invariant throughout the work.
- In status updates and final summaries, distinguish clearly between “completed”, “partial”, and “duplicated”. Never present partial convergence as completion.

## Invariant: no inference in the host process

Every model — recognizers, the layout detector, embedders, generators — runs in
a worker subprocess. The host holds the settings, the extraction recipe, the
geometry, the admission rules and the caches, and reaches each model through a
proxy that stages its input and awaits a reply.

**Why.** Reading a corpus is hours of inference, and the only way to stop
inference that has stopped making progress is to kill the process running it. A
model loaded in the host is one the user can only stop by quitting the
application, and a fault in it takes the application down. So: anything in this
path that can outlive a second must be endable by a forced process kill, and
that is only true of work that is not in the host.

**How to hold it.**
- A `*_local` loader (`dispatch::load_recognizer_local`,
  `dispatch::load_layout_detector_local`) loads into whatever process calls it.
  The only caller allowed to is the worker binary. The host calls
  `worker_ocr::attach` or `worker_layout::attach` instead, which load nothing.
- Facts a model can be asked for without running it — its identity, inventory,
  admission threshold, required input size — are derived from constants so the
  host can answer them while no worker is up. A fact that needed a round trip
  would be one that differed depending on whether the subprocess was alive.
- A worker holds a small bounded map of resident models, not one, because
  reading a document alternates between three of them continuously. Which
  *process* serves a role is `WorkerRole::process_kind`, and that is what
  bounds worker reuse — asked once, never re-derived.

**Boundary.** This binds the host: the process that owns the interface and the
extraction recipe. It does not bind `crates/core/examples/*`, where the whole
process exists to exercise one model directly and Ctrl-C is the kill. It also
does not extend to work that is not inference — mupdf rendering, image decode,
chunking and index writing stay in the host by design; see `WorkerRequest`'s
own doc comment for why.

## Rust Guidelines
- Never index or slice strings by byte offset; always use character-aware method. Byte indexing (&s[..n]) is only safe when you can prove the offset is a char boundary, which is almost never true for arbitrary runtime strings.
