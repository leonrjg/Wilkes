# Indexing activity — what changed, and what is left

Work done overnight on 2026-09-05, on `develop`. Five commits, all tests green.

## The invariant this was about

**For every document in an indexing job, exactly one durable record says what
the job did with it, and that record outlives the process.**

Before, no such record existed. A build reported itself through a `tokio::mpsc`
channel carrying a counter and a preformatted sentence, and that sentence was
the only place a document's name ever appeared. Closing the window erased it; so
did a crash. A document that failed to extract was `error!`-logged and skipped,
so a corpus finished with a hole in it and nothing anywhere said which document
was missing or why. And the work itself was discarded: a build fills
`semantic_index.db.tmp` and publishes it at the end, so cancelling one threw away
everything it had read.

Three gaps, one cause — there was nowhere to record what a job had done, so the
only safe thing to do with a partial build was destroy it.

## Completed

### 1. A durable job journal — `crates/core/src/embed/index/job.rs` (new)

`index_jobs.db`, its own SQLite database beside the index. Its own file because a
build publishes by *replacing* the index file, so a journal living inside it
would be destroyed by the very interruption it exists to describe.

- `jobs` — root, start, end, state (`running` / `completed` / `cancelled` /
  `failed` / `interrupted`), scope size.
- `job_documents` — one row per document: stage (`queued`, `checking`,
  `reading_figures`, `extracting`, `embedding`), outcome (`pending`, `reused`,
  `indexed`, `empty`, `failed`), the error **verbatim**, chunk count.

**Ownership.** The index owns *whether a document is indexed*. The journal owns
*what a job attempted and what became of each attempt* — a different question,
and one an index cannot answer: it has no row for a document that failed, none
for one that yielded no text, none for one the job never reached. Those three are
exactly what "needs attention" and "what is left" are made of.

Jobs left `running` by a process that no longer exists are adopted as
`interrupted` when the journal is first opened in a new process. That is done at
open rather than in a startup hook, because open is the first moment it is
knowably true and a hook is something a new entry point can forget to call.
History is bounded to ten jobs per root.

### 2. A stopped build keeps what it finished — `db.rs`

`merge_root_from` gained a `RootMembership`:

- `Authoritative` — the build reached the end of a whole-root list, so the root
  contains exactly what it produced and anything omitted has left the disk.
  Unchanged behaviour for a completed build.
- `Additive` — the build was stopped, or was run over a chosen subset. It speaks
  only for what it carries; documents it never looked at keep their coverage.

That is the whole fix. The temporary database is still the write boundary and
the merge is still atomic; what changed is that an interrupted build is now
*published* additively instead of deleted. Pausing a four-hundred-page corpus
overnight no longer means starting again at page one.

A guard covers the case where nothing was finished: an empty temporary database
must never be renamed over a working index. That guard distinguishes "stopped
before the first document" from "ran to the end and added nothing" — a retry in
which every document fails again looks identical from the database and must not
be reported as a cancellation.

### 3. One reporter, one order — `BuildReporter`

Everything a build says about a document goes through one object that writes the
journal and *then* notifies the interface. The event is a notification carrying a
copy; a listener that missed it reads the journal and loses nothing. This is why
there is no second progress mechanism.

`IndexBuildProgress` lost its `message: String` — the preformatted sentence that
was the old mechanism — and gained `job_id`, `document`, `stage`, `outcome`. The
sentence was never read by the UI; it was formatted and thrown away.

### 4. Cancellation, unchanged in meaning and firmer in practice

The cancel flag is still raised before the workers are killed, in `cancel_embed`.
What is new is that `flush_batch` distinguishes a batch that was **ended** from
one that was **defeated**: a raised cancel flag or `is_worker_gone(&e)` leaves the
batch's documents `pending` for a continuation and ends the loop, rather than
settling them as failures the user would then be invited to retry. No loop
submits after the flag is up, so no loop starts a replacement worker.

### 5. Continue and retry — two sets, two actions

- **Continue** runs over the `pending` documents — the ones never reached.
- **Retry failed documents** runs over the `failed` ones, and is never automatic.

A continuation that swept failures up would re-attempt the file that breaks the
reader on every continuation, forever, and never say so.

A continuation **inherits the previous job's verdicts** for every document
outside its own scope. Without that, continuing over the unread documents would
produce a job containing no failures at all, and the file that broke an hour ago
would silently stop being reported and stop being retryable — the exact failure
this work exists to end, reintroduced by the fix for it.

Deleting a root's index deletes its job history: a report about coverage the
workspace no longer has would offer to continue into a database that is gone.

### 6. One place, with diagnostics beneath it

Settings › **Activity** (was "Workers"). `IndexActivityPanel` shows the job
state, a plain-English sentence naming what was saved, per-outcome counts, the
document list with failures and unfinished documents first, each failure's error
verbatim, earlier jobs, and the two action buttons. `WorkersPanel` is unchanged
and rendered *beneath* it in a collapsed "Worker diagnostics" disclosure — a view
above it, not a second copy of it.

The panel treats every progress event as a signal to re-read the journal, never
as a fact to accumulate. That is what makes it identical whether it watched the
whole build or was opened for the first time after a restart.

New commands: `index_activity`, `continue_index_job`, `retry_failed_documents`
(Tauri) and `GET /api/embed/activity`, `POST /api/embed/continue`,
`POST /api/embed/retry-failed` (server).

## Verification

- `cargo test --workspace` — all green: core 977 passed (6 ignored), api 247,
  desktop 54, agent 71.
- `cargo clippy --workspace --all-targets` — zero errors; no new warnings (the
  one I introduced, a complex tuple type, is fixed).
- `npx tsc --noEmit` — clean.
- `npx vitest run` — 58 files, 572 tests, all green.

New tests: 6 in `job.rs` for durable state, interruption and inheritance; 3 in
`db.rs` for the stopped-build publish, the continuation merge, and the
adds-nothing case; 6 in `context.rs` for continue/retry selection, the activity
report and history deletion; 2 delegate tests in `desktop`; 10 in
`IndexActivityPanel.test.tsx`; 2 service-layer tests.

One note on the working tree: the schedule carried a list of six files with
uncommitted changes as of 00:57. By the time this ran they were already yours in
`f3364d6..5bd9211` — six commits you made before turning in — so nothing of
yours was in the tree when I started, and nothing of yours was swept into my
commits. My first commit contains two files, both new.

## Not done, and why

- **No browser verification.** `CLAUDE.md` forbids Claude Preview, so the UI was
  verified through the React Testing Library suite (rendering, both actions, the
  running/complete/empty/error states, and the diagnostics disclosure) rather
  than in a running app. Worth a look by eye before release.

- **The in-memory index is not refreshed after a cancelled build.** The build now
  saves documents to disk that the process's live `SemanticIndex` handle does not
  hold, so until the next build or restart, search over that root can return
  fewer results than the index contains. It never returns wrong ones, and
  clicking *Continue* heals it, since that path ends in `finish_build_index`
  which reopens everything. Refreshing on the cancel path would mean reopening
  the index just after every worker was killed, which is a change I did not want
  to make unattended.

- **No toast pointing at the activity view.** After a cancelled build the
  "Indexing..." toast just closes. A line saying what was saved and where to
  continue would close the loop, but the brief asked for *one* place and a second
  reporting surface is what `AGENTS.md` warns against. Deliberate omission, not
  an oversight — say the word and it is a small change.

- **`SemanticPanel`'s progress bar still counts units of work**, not documents,
  so its denominator differs from the activity view's. That is deliberate in the
  original code (a document with figures is visited twice) and I left it alone;
  the two are labelled differently and do not claim to be each other.
