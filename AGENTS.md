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

## Invariant: workers are never started inside a loop

A worker is started by submitting to a manager that has no live process. So
every host loop that submits per item is a loop that starts workers, and the
only question is when.

**Why.** Cancelling is killing — there is no cooperative token in the
extraction path, by design, because the only way to stop inference that has
stopped making progress is to kill the process running it. A kill ends exactly
one in-flight request. If the caller was looping, its next iteration submits
again, finds no process, and spawns a replacement that reloads the model. The
user pressed cancel once; the work needs to be killed once per item. Layout
detection was asked page by page, so cancelling a four-hundred-page book meant
four hundred kills and four hundred ONNX loads, and the progress bar sat at
"Cancelling..." indefinitely with a freshly-started worker in the log.

**How to hold it.**
- **The unit that crosses the pipe is the unit the loop was over.** A
  document's pages go out in one `LayoutRequest`, its crops in one
  `RecognitionRequest`. The loop then runs in the worker, next to the resident
  model, where killing the process ends it. Rendering, decoding and staging
  stay in the host — what moves across is the loop, not the rasterizer.
- **Where a loop genuinely cannot be moved, it must end rather than restart.**
  Embedding batches exist to bound peak memory and cannot become one request;
  the batch loop stops instead. That is what `worker::fault` is for: a
  `WorkerFault::gone` says the process ended, and every loop that submits
  treats it as terminal rather than as this item's failure. Never answer this
  question by matching an error message.
- **Raise the build's cancel flag before killing the workers**, never after.
  The kill surfaces as an error on whatever the build was waiting for; if the
  flag is not up by the time the build reaches its next between-document check,
  the next document starts and its first request spawns a replacement.

**Enforcement.** `WorkerRuntime` counts every worker it starts to replace one
that died under a request, and logs an error naming this invariant on the
first. Zero is the only correct count. A violation is loud in the log rather
than silent in a progress bar.

**Boundary.** This is about *starting* processes, not about how much work
crosses. A loop inside a worker over a resident model — `DocLayout::detect_document`
over a document's pages, `vision::spot_batch` over its images — is where a loop
belongs, and is not a violation.

## Invariant: every document an indexing job touches has one durable verdict

For every document in an indexing job, exactly one durable record says what the
job did with it, and that record outlives the process that wrote it.

**Why.** Reading a corpus is hours, and hours are long enough to be interrupted
— cancelled, crashed, quit. A build used to report itself through an mpsc
channel carrying a counter and a formatted sentence, and that sentence was the
only place a document's name ever appeared; closing the window erased it. A
document that failed to extract was logged and skipped, so the corpus finished
with a hole in it and nothing said which document was missing or why. And the
work itself was thrown away: the temporary database a build fills was deleted
rather than published, so pausing a four-hundred-page corpus overnight meant
starting again at page one.

**Ownership.** The semantic index owns *whether a document is indexed and with
what content*. `IndexJobJournal` owns *what a job attempted and what became of
each attempt* — which is a different question, and one an index cannot answer:
it has no row for a document that failed, none for one that yielded no text, and
none for one the job never reached. Those three are what "needs attention" and
"what is left" are made of. `DocumentOutcome::Indexed` is a fact about the job,
not a second assertion that the index contains it; where they could disagree the
index is right.

**How to hold it.**
- **One reporter, one order.** Everything a build says about a document goes
  through `BuildReporter`, which writes the journal and *then* notifies the
  interface. The event is a notification carrying a copy; a listener that missed
  it reads the journal and loses nothing. Never add a second channel for a
  document's state, and never accumulate one in the interface.
- **A stopped build publishes what it finished.** `RootMembership::Authoritative`
  is only for a build that reached the end of a whole-root list; it may drop the
  documents it omits, which is how a completed build removes files that left the
  disk. Anything else — stopped, or run over a chosen subset — is `Additive` and
  speaks only for what it carries.
- **Ended is not defeated.** A batch that raises while the cancel flag is up, or
  whose worker is gone, leaves its documents unfinished for a continuation. Never
  settle them as failed: that manufactures failures the user is then invited to
  retry. And the loop ends there rather than submitting again, per the invariant
  above.
- **A continuation inherits the verdicts it is not repeating.** Its scope is what
  is left, so a job built from that scope alone would contain no failures, and
  the document that broke the reader would stop being reported and stop being
  retryable. It carries the previous job's settled rows for every path outside
  its own scope — verdicts, with their errors, never work.
- **Retrying is a separate act.** Continuing is over `Pending`; retrying is over
  `Failed`. A continuation that swept failures up would re-attempt the file that
  breaks the reader on every continuation, forever.
- **The journal is not the build's business.** A journal that cannot be opened or
  written is logged at error level and the build proceeds: indexing the corpus is
  the job, and losing the ability to describe it afterwards is not a reason to
  refuse. It is never swallowed silently, because an activity view that is empty
  for no visible reason is what that would present as.

**Boundary.** This is about indexing *jobs* — the host-driven builds a user
starts, continues or retries. The directory watcher's incremental updates are not
jobs: nobody waits on them, there is nothing to resume, and they have no scope to
record.

## Rust Guidelines
- Never index or slice strings by byte offset; always use character-aware method. Byte indexing (&s[..n]) is only safe when you can prove the offset is a char boundary, which is almost never true for arbitrary runtime strings.
