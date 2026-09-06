//! The durable record of what an indexing job did with each document.
//!
//! # The invariant
//!
//! For every document an indexing job touches, exactly one durable record says
//! what the job did with it, and that record outlives the process.
//!
//! Before this module there was no such record. A build reported itself through
//! a `tokio::mpsc` channel carrying a counter and a formatted sentence, and the
//! sentence was the only place a document's name ever appeared. Closing the
//! window erased it; so did a crash, and so did quitting. A document that
//! failed to extract was logged and skipped, which is to say it was invisible:
//! the corpus finished with a hole in it and nothing anywhere said which
//! document was missing or why.
//!
//! # What this owns, and what it does not
//!
//! The semantic index owns *whether a document is indexed and with what
//! content*. That does not change, and nothing here is a second copy of it.
//!
//! This journal owns *what a job attempted and what became of each attempt* —
//! which is a different question, and one the index structurally cannot answer.
//! An index has no row for a document that failed to extract, no row for one
//! that yielded no text, and no row for one the job never reached. Those three
//! are precisely what "what needs attention" and "what is left" are made of.
//!
//! [`DocumentOutcome::Indexed`] is a fact about the job — this job handed this
//! document to the index and the index took it — not an independent assertion
//! that the index contains it. When the two could disagree the index is right.
//!
//! # Why a separate database
//!
//! A build fills `semantic_index.db.tmp` and publishes it by merge or by
//! rename. A journal living inside the index would be destroyed by the very
//! operation whose interruption it exists to describe. So it is its own file,
//! `index_jobs.db`, next to the index and never swapped.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// The file name of the journal inside the workspace data directory.
pub const JOB_DB_FILE: &str = "index_jobs.db";

/// How many finished jobs a root keeps. A job is one row plus one row per
/// document, so the history is bounded per root rather than left to grow for
/// the life of the workspace.
const HISTORY_PER_ROOT: usize = 10;

/// Where a document is in the reading, while the job is still working on it.
///
/// These are the build's three passes, which is to say the three places a
/// document can be waiting when a user asks what is happening. They are stages,
/// not outcomes: a document in any of them is unfinished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentStage {
    /// Queued, not yet looked at.
    Queued,
    /// Pass 1 — deciding whether the previous index already read it.
    Checking,
    /// Pass 2 — recognition over its figures, in the recognition worker.
    ReadingFigures,
    /// Pass 3 — extraction and chunking, in this process.
    Extracting,
    /// Pass 3 — its chunks are in a batch awaiting the embedding worker.
    Embedding,
}

impl DocumentStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Checking => "checking",
            Self::ReadingFigures => "reading_figures",
            Self::Extracting => "extracting",
            Self::Embedding => "embedding",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "checking" => Self::Checking,
            "reading_figures" => Self::ReadingFigures,
            "extracting" => Self::Extracting,
            "embedding" => Self::Embedding,
            _ => Self::Queued,
        }
    }
}

/// What became of a document. Terminal except for [`Self::Pending`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentOutcome {
    /// Not finished. The job either is still working on it or never reached it.
    Pending,
    /// The previous index had already read it unchanged, so it cost nothing.
    Reused,
    /// Extracted, embedded and written.
    Indexed,
    /// Read successfully and yielded no chunk. A reading, not a failure — an
    /// empty text file is empty, and saying so is the correct answer.
    Empty,
    /// Reading or embedding raised. `error` on the row says what.
    Failed,
}

impl DocumentOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Reused => "reused",
            Self::Indexed => "indexed",
            Self::Empty => "empty",
            Self::Failed => "failed",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "reused" => Self::Reused,
            "indexed" => Self::Indexed,
            "empty" => Self::Empty,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }

    /// Whether the document is finished with, whatever the verdict.
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }
}

/// What became of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Started and not yet ended — by this process, right now.
    Running,
    /// Ran to the end of its document list.
    Completed,
    /// Stopped because the user cancelled it.
    Cancelled,
    /// Stopped because something other than the user ended it.
    Failed,
    /// Left `Running` by a process that is no longer alive. Only
    /// [`IndexJobJournal::adopt_orphaned_jobs`] writes this, at startup.
    Interrupted,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            "failed" => Self::Failed,
            "interrupted" => Self::Interrupted,
            _ => Self::Running,
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }

    /// Whether the job stopped before finishing its list. These are the states
    /// that leave work to continue.
    pub fn stopped_early(self) -> bool {
        matches!(self, Self::Cancelled | Self::Failed | Self::Interrupted)
    }
}

/// One document's row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobDocument {
    pub path: PathBuf,
    pub stage: DocumentStage,
    pub outcome: DocumentOutcome,
    /// Present only on [`DocumentOutcome::Failed`]. The error, kept verbatim,
    /// because a failure the user cannot read is a failure they cannot act on.
    pub error: Option<String>,
    pub chunks: Option<i64>,
    pub updated_at_ms: i64,
}

/// How many documents ended each way. Derived by counting rows, never stored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCounts {
    pub pending: usize,
    pub reused: usize,
    pub indexed: usize,
    pub empty: usize,
    pub failed: usize,
}

impl JobCounts {
    /// Documents the job finished with, whatever the verdict.
    pub fn settled(&self) -> usize {
        self.reused + self.indexed + self.empty + self.failed
    }

    /// Documents that are in the index because of this job or were confirmed
    /// still in it. What "already saved" means on screen.
    pub fn saved(&self) -> usize {
        self.reused + self.indexed
    }
}

/// A job and its tallies, without its document rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSummary {
    pub id: i64,
    pub root: PathBuf,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub state: JobState,
    /// Why it ended, when that is not self-evident from `state`.
    pub detail: Option<String>,
    pub total_documents: usize,
    pub counts: JobCounts,
}

impl JobSummary {
    /// Whether there is anything left for a continuation to do. Failed
    /// documents are excluded on purpose: retrying them is a separate,
    /// deliberate act, never something a "continue" quietly sweeps up.
    pub fn has_remaining_work(&self) -> bool {
        self.counts.pending > 0
    }
}

/// One root's indexing activity: what is happening, what happened, and what
/// happened before that.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexActivity {
    pub root: PathBuf,
    /// The running job, or the most recent one to have ended. `None` when this
    /// root has never been indexed by this workspace.
    pub job: Option<JobSummary>,
    /// A bounded slice of `job`'s documents, failures and unfinished ones
    /// first. `job.counts` is the whole truth about how many there are.
    pub documents: Vec<JobDocument>,
    /// How many documents `documents` was allowed to carry, so a reader can
    /// tell a short list from a truncated one.
    pub document_limit: usize,
    /// Earlier jobs for this root, newest first.
    pub history: Vec<JobSummary>,
}

/// A `jobs` row as read, before its document counts are gathered.
type SummaryRow = (i64, String, i64, Option<i64>, String, Option<String>, i64);

/// The journal. One connection, opened once per workspace and held for the life
/// of the context.
pub struct IndexJobJournal {
    conn: Connection,
    path: PathBuf,
}

impl IndexJobJournal {
    /// Open (creating if absent) the journal for a workspace data directory.
    pub fn open(data_dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(data_dir).with_context(|| {
            format!(
                "Failed to create data directory {} for the index job journal",
                data_dir.display()
            )
        })?;
        let path = data_dir.join(JOB_DB_FILE);
        let conn = Connection::open(&path)
            .with_context(|| format!("Failed to open index job journal at {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(3))?;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS jobs (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                root            TEXT    NOT NULL,
                started_at_ms   INTEGER NOT NULL,
                ended_at_ms     INTEGER,
                state           TEXT    NOT NULL,
                detail          TEXT,
                total_documents INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS job_documents (
                job_id        INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                path          TEXT    NOT NULL,
                stage         TEXT    NOT NULL,
                outcome       TEXT    NOT NULL,
                error         TEXT,
                chunks        INTEGER,
                position      INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY (job_id, path)
            );
            CREATE INDEX IF NOT EXISTS idx_jobs_root ON jobs(root, started_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_job_documents_outcome
                ON job_documents(job_id, outcome);
            ",
        )
        .with_context(|| format!("Failed to prepare index job journal at {}", path.display()))?;
        Ok(Self { conn, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    fn key(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// Record a job about to start over `paths`, and return its id.
    ///
    /// Every path is written up front, as `Queued`/`Pending`. The list is the
    /// job's scope, and a scope discovered incrementally could not answer "what
    /// is left" until it had already finished finding out.
    pub fn begin(&mut self, root: &Path, paths: &[PathBuf]) -> anyhow::Result<i64> {
        self.begin_continuing(root, paths, None)
    }

    /// Record a job about to start over `paths`, carrying forward the verdicts
    /// of `continues` for every document not in that list.
    ///
    /// A continuation is the same piece of work resuming, so it has to inherit
    /// what the previous run already decided. Without this, continuing an
    /// interrupted build over its unread documents would produce a job whose
    /// scope contained no failures — and the document that broke the reader an
    /// hour ago would quietly stop being reported, stop being retryable, and
    /// leave a hole in the corpus that nothing named.
    ///
    /// The inherited rows are verdicts, not work: they are already settled and
    /// the build is never handed them.
    pub fn begin_continuing(
        &mut self,
        root: &Path,
        paths: &[PathBuf],
        continues: Option<i64>,
    ) -> anyhow::Result<i64> {
        let inherited: Vec<JobDocument> = match continues {
            Some(previous) => {
                let in_scope: HashSet<String> = paths.iter().map(|p| Self::key(p)).collect();
                self.documents(previous, None, usize::MAX)?
                    .into_iter()
                    .filter(|doc| {
                        doc.outcome.is_terminal() && !in_scope.contains(&Self::key(&doc.path))
                    })
                    .collect()
            }
            None => Vec::new(),
        };

        let now = Self::now_ms();
        let total = paths.len() + inherited.len();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO jobs (root, started_at_ms, ended_at_ms, state, detail, total_documents)
             VALUES (?1, ?2, NULL, ?3, NULL, ?4)",
            params![
                Self::key(root),
                now,
                JobState::Running.as_str(),
                total as i64
            ],
        )?;
        let job_id = tx.last_insert_rowid();
        {
            // One statement, one transaction: a ten-thousand-file corpus would
            // otherwise pay a full fsync per row before the first document is
            // even read.
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO job_documents
                     (job_id, path, stage, outcome, error, chunks, position, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            // The scope leads, because it is what the build will visit and in
            // what order.
            for (position, path) in paths.iter().enumerate() {
                stmt.execute(params![
                    job_id,
                    Self::key(path),
                    DocumentStage::Queued.as_str(),
                    DocumentOutcome::Pending.as_str(),
                    None::<String>,
                    None::<i64>,
                    position as i64,
                    now
                ])?;
            }
            for (offset, doc) in inherited.iter().enumerate() {
                stmt.execute(params![
                    job_id,
                    Self::key(&doc.path),
                    doc.stage.as_str(),
                    doc.outcome.as_str(),
                    doc.error.as_deref(),
                    doc.chunks,
                    (paths.len() + offset) as i64,
                    doc.updated_at_ms
                ])?;
            }
        }
        tx.commit()?;
        self.prune_history(root)?;
        Ok(job_id)
    }

    /// Keep the most recent [`HISTORY_PER_ROOT`] jobs for a root, plus anything
    /// still running. Document rows go with them, by cascade.
    fn prune_history(&self, root: &Path) -> anyhow::Result<()> {
        self.conn.execute(
            "DELETE FROM jobs
             WHERE root = ?1
               AND state != ?2
               AND id NOT IN (
                   SELECT id FROM jobs WHERE root = ?1
                   ORDER BY started_at_ms DESC, id DESC LIMIT ?3
               )",
            params![
                Self::key(root),
                JobState::Running.as_str(),
                HISTORY_PER_ROOT as i64
            ],
        )?;
        Ok(())
    }

    /// Move a document to a stage. Its outcome stays `Pending` — a stage is
    /// where it is, not what became of it.
    pub fn note_stage(&self, job_id: i64, path: &Path, stage: DocumentStage) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE job_documents SET stage = ?3, updated_at_ms = ?4
             WHERE job_id = ?1 AND path = ?2",
            params![job_id, Self::key(path), stage.as_str(), Self::now_ms()],
        )?;
        Ok(())
    }

    /// Settle a document.
    pub fn note_outcome(
        &self,
        job_id: i64,
        path: &Path,
        outcome: DocumentOutcome,
        error: Option<&str>,
        chunks: Option<i64>,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE job_documents
             SET outcome = ?3, error = ?4, chunks = ?5, updated_at_ms = ?6
             WHERE job_id = ?1 AND path = ?2",
            params![
                job_id,
                Self::key(path),
                outcome.as_str(),
                error,
                chunks,
                Self::now_ms()
            ],
        )?;
        Ok(())
    }

    /// Settle several documents at once.
    ///
    /// Embedding is batched — a flush writes a whole queue of documents in one
    /// go — so their rows settle together in one transaction rather than in as
    /// many transactions as the batch had documents.
    pub fn note_outcomes(
        &mut self,
        job_id: i64,
        entries: &[(PathBuf, DocumentOutcome, Option<String>, Option<i64>)],
    ) -> anyhow::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let now = Self::now_ms();
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "UPDATE job_documents
                 SET outcome = ?3, error = ?4, chunks = ?5, updated_at_ms = ?6
                 WHERE job_id = ?1 AND path = ?2",
            )?;
            for (path, outcome, error, chunks) in entries {
                stmt.execute(params![
                    job_id,
                    Self::key(path),
                    outcome.as_str(),
                    error.as_deref(),
                    chunks,
                    now
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// End a job. `detail` is kept verbatim for `Failed`.
    pub fn finish(&self, job_id: i64, state: JobState, detail: Option<&str>) -> anyhow::Result<()> {
        anyhow::ensure!(
            state.is_terminal(),
            "finish() needs a terminal state, got {}",
            state.as_str()
        );
        self.conn.execute(
            "UPDATE jobs SET state = ?2, detail = ?3, ended_at_ms = ?4 WHERE id = ?1",
            params![job_id, state.as_str(), detail, Self::now_ms()],
        )?;
        Ok(())
    }

    /// Reclassify every job still marked `Running` as `Interrupted`.
    ///
    /// Called once at startup, and only there. A job row says `Running` because
    /// the process that wrote it was alive; if this process is only now opening
    /// the journal, no such process is. Returns how many were adopted, so the
    /// caller can log a number that should almost always be zero.
    pub fn adopt_orphaned_jobs(&self) -> anyhow::Result<usize> {
        let adopted = self.conn.execute(
            "UPDATE jobs SET state = ?1, ended_at_ms = COALESCE(ended_at_ms, ?2)
             WHERE state = ?3",
            params![
                JobState::Interrupted.as_str(),
                Self::now_ms(),
                JobState::Running.as_str()
            ],
        )?;
        Ok(adopted)
    }

    fn counts_for(&self, job_id: i64) -> anyhow::Result<JobCounts> {
        let mut stmt = self.conn.prepare(
            "SELECT outcome, COUNT(*) FROM job_documents WHERE job_id = ?1 GROUP BY outcome",
        )?;
        let rows = stmt
            .query_map(params![job_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
            })?
            .collect::<Result<HashMap<_, _>, _>>()?;
        let get = |outcome: DocumentOutcome| *rows.get(outcome.as_str()).unwrap_or(&0);
        Ok(JobCounts {
            pending: get(DocumentOutcome::Pending),
            reused: get(DocumentOutcome::Reused),
            indexed: get(DocumentOutcome::Indexed),
            empty: get(DocumentOutcome::Empty),
            failed: get(DocumentOutcome::Failed),
        })
    }

    fn summary_from_row(&self, row: SummaryRow) -> anyhow::Result<JobSummary> {
        let (id, root, started_at_ms, ended_at_ms, state, detail, total_documents) = row;
        Ok(JobSummary {
            id,
            root: PathBuf::from(root),
            started_at_ms,
            ended_at_ms,
            state: JobState::from_str(&state),
            detail,
            total_documents: total_documents as usize,
            counts: self.counts_for(id)?,
        })
    }

    const SUMMARY_COLUMNS: &'static str =
        "id, root, started_at_ms, ended_at_ms, state, detail, total_documents";

    fn read_summary_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SummaryRow> {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
        ))
    }

    /// The most recent job for a root, running or not.
    pub fn latest_for_root(&self, root: &Path) -> anyhow::Result<Option<JobSummary>> {
        let sql = format!(
            "SELECT {} FROM jobs WHERE root = ?1 ORDER BY started_at_ms DESC, id DESC LIMIT 1",
            Self::SUMMARY_COLUMNS
        );
        let row = self
            .conn
            .query_row(&sql, params![Self::key(root)], Self::read_summary_row)
            .optional()?;
        row.map(|row| self.summary_from_row(row)).transpose()
    }

    /// One job by id.
    pub fn job(&self, job_id: i64) -> anyhow::Result<Option<JobSummary>> {
        let sql = format!("SELECT {} FROM jobs WHERE id = ?1", Self::SUMMARY_COLUMNS);
        let row = self
            .conn
            .query_row(&sql, params![job_id], Self::read_summary_row)
            .optional()?;
        row.map(|row| self.summary_from_row(row)).transpose()
    }

    /// Recent jobs across every root, newest first.
    pub fn recent(&self, limit: usize) -> anyhow::Result<Vec<JobSummary>> {
        let sql = format!(
            "SELECT {} FROM jobs ORDER BY started_at_ms DESC, id DESC LIMIT ?1",
            Self::SUMMARY_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![limit as i64], Self::read_summary_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|row| self.summary_from_row(row))
            .collect()
    }

    /// A job's document rows in the order the job will visit them.
    ///
    /// `limit` bounds what crosses to the interface; a hundred-thousand-file
    /// corpus is not a list anyone scrolls, and the counts already say how many
    /// there are.
    pub fn documents(
        &self,
        job_id: i64,
        outcome: Option<DocumentOutcome>,
        limit: usize,
    ) -> anyhow::Result<Vec<JobDocument>> {
        let mut sql = String::from(
            "SELECT path, stage, outcome, error, chunks, updated_at_ms
             FROM job_documents WHERE job_id = ?1",
        );
        if outcome.is_some() {
            sql.push_str(" AND outcome = ?3");
        }
        // Unsettled first, then in job order: what is happening now and what
        // needs attention are what the user came to see.
        sql.push_str(
            " ORDER BY CASE outcome WHEN 'failed' THEN 0 WHEN 'pending' THEN 1 ELSE 2 END,
                       position ASC LIMIT ?2",
        );
        let mut stmt = self.conn.prepare(&sql)?;
        // `usize::MAX` is a legitimate ask -- a continuation inherits the whole
        // of the previous job -- and saturating here keeps that from arriving
        // at SQLite as a negative literal that happens to mean "no limit".
        let limit = limit.min(i64::MAX as usize) as i64;
        let read = |row: &rusqlite::Row<'_>| -> rusqlite::Result<JobDocument> {
            Ok(JobDocument {
                path: PathBuf::from(row.get::<_, String>(0)?),
                stage: DocumentStage::from_str(&row.get::<_, String>(1)?),
                outcome: DocumentOutcome::from_str(&row.get::<_, String>(2)?),
                error: row.get(3)?,
                chunks: row.get(4)?,
                updated_at_ms: row.get(5)?,
            })
        };
        let rows = match outcome {
            Some(outcome) => stmt
                .query_map(params![job_id, limit, outcome.as_str()], read)?
                .collect::<Result<Vec<_>, _>>()?,
            None => stmt
                .query_map(params![job_id, limit], read)?
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(rows)
    }

    /// Every path in a job with a given outcome, in job order and unbounded.
    ///
    /// This is what a continuation or a retry is run over, so it must be the
    /// whole set — not the bounded slice [`Self::documents`] renders.
    pub fn paths_with_outcome(
        &self,
        job_id: i64,
        outcome: DocumentOutcome,
    ) -> anyhow::Result<Vec<PathBuf>> {
        let mut stmt = self.conn.prepare(
            "SELECT path FROM job_documents
             WHERE job_id = ?1 AND outcome = ?2 ORDER BY position ASC",
        )?;
        let rows = stmt
            .query_map(params![job_id, outcome.as_str()], |row| {
                Ok(PathBuf::from(row.get::<_, String>(0)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every path any of `root`'s jobs settled with `outcome`, deduplicated.
    ///
    /// The per-job [`paths_with_outcome`](Self::paths_with_outcome) answers
    /// "what should this continuation or retry be over"; this one answers
    /// "what does this root's history say about this file", which spans the
    /// jobs. A document read once and found to hold no text stays read: the
    /// job that reported it may be three continuations ago.
    pub fn paths_with_outcome_for_root(
        &self,
        root: &Path,
        outcome: DocumentOutcome,
    ) -> anyhow::Result<Vec<PathBuf>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT d.path FROM job_documents d
             JOIN jobs j ON j.id = d.job_id
             WHERE j.root = ?1 AND d.outcome = ?2",
        )?;
        let rows = stmt
            .query_map(params![Self::key(root), outcome.as_str()], |row| {
                Ok(PathBuf::from(row.get::<_, String>(0)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Everything one root's activity view shows: the current or most recent
    /// job, a bounded slice of its documents, and the jobs before it.
    ///
    /// Assembled here, in one query set against one connection, rather than by
    /// the caller making three calls that could each see a different moment of
    /// a running build.
    pub fn activity_for_root(
        &self,
        root: &Path,
        document_limit: usize,
    ) -> anyhow::Result<IndexActivity> {
        let job = self.latest_for_root(root)?;
        let documents = match job.as_ref() {
            Some(job) => self.documents(job.id, None, document_limit)?,
            None => Vec::new(),
        };
        let sql = format!(
            "SELECT {} FROM jobs WHERE root = ?1 ORDER BY started_at_ms DESC, id DESC LIMIT ?2",
            Self::SUMMARY_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                params![Self::key(root), HISTORY_PER_ROOT as i64],
                Self::read_summary_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let history = rows
            .into_iter()
            .map(|row| self.summary_from_row(row))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(IndexActivity {
            root: root.to_path_buf(),
            job,
            documents,
            document_limit,
            history,
        })
    }

    /// Forget every job for a root. Used when its index coverage is deleted:
    /// a job history describing an index that no longer exists is a report
    /// about nothing.
    pub fn forget_root(&self, root: &Path) -> anyhow::Result<()> {
        self.conn
            .execute("DELETE FROM jobs WHERE root = ?1", params![Self::key(root)])?;
        Ok(())
    }

    /// Forget everything. Used when the whole index is deleted.
    pub fn forget_all(&self) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM jobs", [])?;
        Ok(())
    }
}

// ── Reporting ────────────────────────────────────────────────────────────────

/// The one place an index build says anything about a document.
///
/// A build has two audiences: the interface, which wants to know now, and the
/// next process to open this workspace, which wants to know later. They are the
/// same facts, so they are not two mechanisms — they are one write and one
/// notification, in that order, behind one call. Writing first is what makes
/// the notification droppable: a listener that missed it, or was not there,
/// reads the journal.
///
/// A reporter with no journal ([`Self::without_journal`]) still reports to the
/// channel. That is for callers with nothing to resume — the tests, and any
/// build not run on a user's behalf.
pub struct BuildReporter {
    tx: crate::models::progress::ProgressTx,
    sink: Option<JobSink>,
}

/// The journal half of a reporter: which journal, and which job in it.
struct JobSink {
    journal: std::sync::Arc<std::sync::Mutex<IndexJobJournal>>,
    job_id: i64,
}

impl BuildReporter {
    /// Report to the channel and to `job_id` in `journal`.
    pub fn journalled(
        tx: crate::models::progress::ProgressTx,
        journal: std::sync::Arc<std::sync::Mutex<IndexJobJournal>>,
        job_id: i64,
    ) -> Self {
        Self {
            tx,
            sink: Some(JobSink { journal, job_id }),
        }
    }

    /// Report to the channel only. Nothing is resumable afterwards.
    pub fn without_journal(tx: crate::models::progress::ProgressTx) -> Self {
        Self { tx, sink: None }
    }

    /// The job being reported into, when there is one.
    pub fn job_id(&self) -> Option<i64> {
        self.sink.as_ref().map(|sink| sink.job_id)
    }

    /// The channel this reports on, for the one part of a build that is not
    /// about a document: the model download that may precede it.
    pub fn progress_tx(&self) -> crate::models::progress::ProgressTx {
        self.tx.clone()
    }

    /// Run `f` against the journal, logging rather than propagating a failure.
    ///
    /// A journal write that fails must not fail the build: the build's job is
    /// to index the corpus, and losing the ability to describe that later is
    /// not a reason to stop doing it. It is logged at error level, never
    /// swallowed silently, because a journal that has stopped recording is
    /// exactly the condition that would otherwise be discovered as an empty
    /// activity view with no explanation.
    fn with_journal<F>(&self, what: &str, f: F)
    where
        F: FnOnce(&mut IndexJobJournal, i64) -> anyhow::Result<()>,
    {
        let Some(sink) = self.sink.as_ref() else {
            return;
        };
        match sink.journal.lock() {
            Ok(mut journal) => {
                if let Err(e) = f(&mut journal, sink.job_id) {
                    tracing::error!("[BuildReporter] {what} could not be journalled: {e:#}");
                }
            }
            Err(poisoned) => {
                tracing::error!(
                    "[BuildReporter] {what} could not be journalled: the job journal lock is \
                     poisoned by an earlier panic"
                );
                // The mutex guards a SQLite connection, not an invariant a
                // panic could have half-broken, so the guard is recoverable.
                let mut journal = poisoned.into_inner();
                if let Err(e) = f(&mut journal, sink.job_id) {
                    tracing::error!("[BuildReporter] {what} could not be journalled: {e:#}");
                }
            }
        }
    }

    fn emit(&self, progress: IndexBuildEvent<'_>) {
        let _ = self
            .tx
            .blocking_send(crate::models::progress::EmbedProgress::Build(
                crate::models::progress::IndexBuildProgress {
                    files_processed: progress.done_units,
                    total_files: progress.total_units,
                    job_id: self.job_id(),
                    document: progress.path.map(|p| p.to_string_lossy().into_owned()),
                    stage: progress.stage,
                    outcome: progress.outcome,
                    done: progress.done,
                },
            ));
    }

    /// A document has entered `stage`.
    pub fn stage(&self, path: &Path, stage: DocumentStage, done_units: usize, total_units: usize) {
        self.with_journal("stage", |journal, job_id| {
            journal.note_stage(job_id, path, stage)
        });
        self.emit(IndexBuildEvent {
            path: Some(path),
            stage: Some(stage),
            outcome: None,
            done_units,
            total_units,
            done: false,
        });
    }

    /// A document is finished with.
    pub fn settle(
        &self,
        path: &Path,
        outcome: DocumentOutcome,
        error: Option<&str>,
        chunks: Option<i64>,
        done_units: usize,
        total_units: usize,
    ) {
        self.with_journal("outcome", |journal, job_id| {
            journal.note_outcome(job_id, path, outcome, error, chunks)
        });
        self.emit(IndexBuildEvent {
            path: Some(path),
            stage: None,
            outcome: Some(outcome),
            done_units,
            total_units,
            done: false,
        });
    }

    /// A whole embedding batch is finished with, in one transaction.
    pub fn settle_batch(
        &self,
        entries: &[(PathBuf, DocumentOutcome, Option<String>, Option<i64>)],
        done_units: usize,
        total_units: usize,
    ) {
        self.with_journal("batch outcome", |journal, job_id| {
            journal.note_outcomes(job_id, entries)
        });
        for (path, outcome, _, _) in entries {
            self.emit(IndexBuildEvent {
                path: Some(path),
                stage: None,
                outcome: Some(*outcome),
                done_units,
                total_units,
                done: false,
            });
        }
    }

    /// The build reached the end of its list.
    pub fn finished(&self, total_units: usize) {
        self.emit(IndexBuildEvent {
            path: None,
            stage: None,
            outcome: None,
            done_units: total_units,
            total_units,
            done: true,
        });
    }
}

/// The arguments of one emission, named so the call sites read as sentences.
struct IndexBuildEvent<'a> {
    path: Option<&'a Path>,
    stage: Option<DocumentStage>,
    outcome: Option<DocumentOutcome>,
    done_units: usize,
    total_units: usize,
    done: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn begin_records_every_path_as_pending() {
        let dir = tempdir().unwrap();
        let mut journal = IndexJobJournal::open(dir.path()).unwrap();
        let root = PathBuf::from("/corpus");
        let job = journal
            .begin(&root, &paths(&["/corpus/a.pdf", "/corpus/b.pdf"]))
            .unwrap();

        let summary = journal.job(job).unwrap().unwrap();
        assert_eq!(summary.state, JobState::Running);
        assert_eq!(summary.total_documents, 2);
        assert_eq!(summary.counts.pending, 2);
        assert_eq!(summary.counts.settled(), 0);
        assert!(summary.has_remaining_work());
    }

    #[test]
    fn stages_and_outcomes_are_recorded_per_document() {
        let dir = tempdir().unwrap();
        let mut journal = IndexJobJournal::open(dir.path()).unwrap();
        let root = PathBuf::from("/corpus");
        let a = PathBuf::from("/corpus/a.pdf");
        let b = PathBuf::from("/corpus/b.pdf");
        let job = journal.begin(&root, &[a.clone(), b.clone()]).unwrap();

        journal
            .note_stage(job, &a, DocumentStage::ReadingFigures)
            .unwrap();
        let docs = journal.documents(job, None, 10).unwrap();
        let row = docs.iter().find(|d| d.path == a).unwrap();
        assert_eq!(row.stage, DocumentStage::ReadingFigures);
        assert_eq!(row.outcome, DocumentOutcome::Pending);

        journal
            .note_outcome(job, &a, DocumentOutcome::Indexed, None, Some(12))
            .unwrap();
        journal
            .note_outcome(
                job,
                &b,
                DocumentOutcome::Failed,
                Some("mupdf: broken xref"),
                None,
            )
            .unwrap();

        let summary = journal.job(job).unwrap().unwrap();
        assert_eq!(summary.counts.indexed, 1);
        assert_eq!(summary.counts.failed, 1);
        assert_eq!(summary.counts.pending, 0);
        assert!(!summary.has_remaining_work());

        let failed = journal
            .documents(job, Some(DocumentOutcome::Failed), 10)
            .unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].path, b);
        assert_eq!(failed[0].error.as_deref(), Some("mupdf: broken xref"));
    }

    #[test]
    fn batched_outcomes_settle_together() {
        let dir = tempdir().unwrap();
        let mut journal = IndexJobJournal::open(dir.path()).unwrap();
        let root = PathBuf::from("/corpus");
        let all = paths(&["/corpus/a.txt", "/corpus/b.txt", "/corpus/c.txt"]);
        let job = journal.begin(&root, &all).unwrap();

        journal
            .note_outcomes(
                job,
                &[
                    (all[0].clone(), DocumentOutcome::Indexed, None, Some(3)),
                    (all[1].clone(), DocumentOutcome::Indexed, None, Some(4)),
                ],
            )
            .unwrap();

        let summary = journal.job(job).unwrap().unwrap();
        assert_eq!(summary.counts.indexed, 2);
        assert_eq!(summary.counts.pending, 1);
    }

    /// The point of the whole module: a job that ends without being finished is
    /// still there, with its per-document verdicts, after the process dies.
    #[test]
    fn an_interrupted_job_survives_the_process_and_names_what_is_left() {
        let dir = tempdir().unwrap();
        let root = PathBuf::from("/corpus");
        let all = paths(&["/corpus/a.txt", "/corpus/b.txt", "/corpus/c.txt"]);

        {
            let mut journal = IndexJobJournal::open(dir.path()).unwrap();
            let job = journal.begin(&root, &all).unwrap();
            journal
                .note_outcome(job, &all[0], DocumentOutcome::Indexed, None, Some(2))
                .unwrap();
            journal
                .note_outcome(
                    job,
                    &all[1],
                    DocumentOutcome::Failed,
                    Some("no reader"),
                    None,
                )
                .unwrap();
            // No finish(): the process is gone mid-job.
        }

        let journal = IndexJobJournal::open(dir.path()).unwrap();
        assert_eq!(journal.adopt_orphaned_jobs().unwrap(), 1);

        let summary = journal.latest_for_root(&root).unwrap().unwrap();
        assert_eq!(summary.state, JobState::Interrupted);
        assert!(summary.state.stopped_early());
        assert_eq!(summary.counts.saved(), 1);
        assert_eq!(summary.counts.failed, 1);
        assert!(summary.has_remaining_work());

        // Continuing is over what was never reached, and excludes the failure.
        let remaining = journal
            .paths_with_outcome(summary.id, DocumentOutcome::Pending)
            .unwrap();
        assert_eq!(remaining, vec![all[2].clone()]);

        // Retrying is its own, separate set.
        let failed = journal
            .paths_with_outcome(summary.id, DocumentOutcome::Failed)
            .unwrap();
        assert_eq!(failed, vec![all[1].clone()]);
    }

    /// Continuing must not lose the failure the previous run found.
    ///
    /// The continuation's scope is the unread documents, so a job built from
    /// that scope alone would contain no failures at all — and the document
    /// that broke the reader would silently stop being reported and stop being
    /// retryable, leaving a hole in the corpus that nothing named.
    #[test]
    fn a_continuation_inherits_the_verdicts_of_the_job_it_continues() {
        let dir = tempdir().unwrap();
        let mut journal = IndexJobJournal::open(dir.path()).unwrap();
        let root = PathBuf::from("/corpus");
        let all = paths(&["/corpus/saved", "/corpus/broke", "/corpus/never"]);

        let first = journal.begin(&root, &all).unwrap();
        journal
            .note_outcome(first, &all[0], DocumentOutcome::Indexed, None, Some(4))
            .unwrap();
        journal
            .note_outcome(
                first,
                &all[1],
                DocumentOutcome::Failed,
                Some("no reader"),
                None,
            )
            .unwrap();
        journal.finish(first, JobState::Cancelled, None).unwrap();

        // Continue over what was never reached.
        let second = journal
            .begin_continuing(&root, &all[2..], Some(first))
            .unwrap();

        let summary = journal.job(second).unwrap().unwrap();
        assert_eq!(
            summary.total_documents, 3,
            "the continuation still describes the whole corpus"
        );
        assert_eq!(summary.counts.pending, 1, "only the unread one is work");
        assert_eq!(
            summary.counts.indexed, 1,
            "the saved one is carried, not redone"
        );
        assert_eq!(summary.counts.failed, 1, "and so is the failure");

        // The failure is still retryable, from the new job.
        let failed = journal
            .paths_with_outcome(second, DocumentOutcome::Failed)
            .unwrap();
        assert_eq!(failed, vec![all[1].clone()]);
        let carried = journal
            .documents(second, Some(DocumentOutcome::Failed), 10)
            .unwrap();
        assert_eq!(
            carried[0].error.as_deref(),
            Some("no reader"),
            "the error is carried with it, not just the verdict"
        );

        // And the continuation is not handed the documents it inherited.
        let work = journal
            .paths_with_outcome(second, DocumentOutcome::Pending)
            .unwrap();
        assert_eq!(work, vec![all[2].clone()]);
    }

    /// A retry's scope *is* the failed set, so those documents are work again
    /// rather than inherited verdicts.
    #[test]
    fn a_retry_makes_the_failed_documents_work_again() {
        let dir = tempdir().unwrap();
        let mut journal = IndexJobJournal::open(dir.path()).unwrap();
        let root = PathBuf::from("/corpus");
        let all = paths(&["/corpus/saved", "/corpus/broke"]);

        let first = journal.begin(&root, &all).unwrap();
        journal
            .note_outcome(first, &all[0], DocumentOutcome::Indexed, None, Some(2))
            .unwrap();
        journal
            .note_outcome(first, &all[1], DocumentOutcome::Failed, Some("boom"), None)
            .unwrap();
        journal.finish(first, JobState::Completed, None).unwrap();

        let retry = journal
            .begin_continuing(&root, &all[1..], Some(first))
            .unwrap();

        let summary = journal.job(retry).unwrap().unwrap();
        assert_eq!(
            summary.counts.failed, 0,
            "the failure is being re-attempted"
        );
        assert_eq!(summary.counts.pending, 1);
        assert_eq!(summary.counts.indexed, 1, "the rest is still accounted for");
        assert_eq!(summary.total_documents, 2);
    }

    /// An unfinished document of the previous job is never inherited as a
    /// verdict: it had none. If it is not in the new scope it is simply not
    /// this job's business.
    #[test]
    fn a_continuation_inherits_verdicts_only() {
        let dir = tempdir().unwrap();
        let mut journal = IndexJobJournal::open(dir.path()).unwrap();
        let root = PathBuf::from("/corpus");
        let all = paths(&["/corpus/a", "/corpus/b", "/corpus/c"]);

        let first = journal.begin(&root, &all).unwrap();
        journal
            .note_outcome(first, &all[0], DocumentOutcome::Reused, None, None)
            .unwrap();
        journal.finish(first, JobState::Interrupted, None).unwrap();

        // Continue over only one of the two unread documents.
        let second = journal
            .begin_continuing(&root, &all[1..2], Some(first))
            .unwrap();
        let summary = journal.job(second).unwrap().unwrap();
        assert_eq!(
            summary.total_documents, 2,
            "one reused verdict, one document"
        );
        assert_eq!(summary.counts.reused, 1);
        assert_eq!(summary.counts.pending, 1);
    }

    #[test]
    fn adopting_orphans_leaves_finished_jobs_alone() {
        let dir = tempdir().unwrap();
        let root = PathBuf::from("/corpus");
        {
            let mut journal = IndexJobJournal::open(dir.path()).unwrap();
            let job = journal.begin(&root, &paths(&["/corpus/a.txt"])).unwrap();
            journal.finish(job, JobState::Completed, None).unwrap();
        }
        let journal = IndexJobJournal::open(dir.path()).unwrap();
        assert_eq!(journal.adopt_orphaned_jobs().unwrap(), 0);
        assert_eq!(
            journal.latest_for_root(&root).unwrap().unwrap().state,
            JobState::Completed
        );
    }

    #[test]
    fn finish_refuses_a_non_terminal_state() {
        let dir = tempdir().unwrap();
        let mut journal = IndexJobJournal::open(dir.path()).unwrap();
        let job = journal
            .begin(&PathBuf::from("/c"), &paths(&["/c/a"]))
            .unwrap();
        let err = journal.finish(job, JobState::Running, None).unwrap_err();
        assert!(err.to_string().contains("terminal state"));
    }

    #[test]
    fn failed_and_pending_documents_sort_ahead_of_settled_ones() {
        let dir = tempdir().unwrap();
        let mut journal = IndexJobJournal::open(dir.path()).unwrap();
        let root = PathBuf::from("/corpus");
        let all = paths(&["/corpus/a", "/corpus/b", "/corpus/c"]);
        let job = journal.begin(&root, &all).unwrap();
        journal
            .note_outcome(job, &all[0], DocumentOutcome::Indexed, None, Some(1))
            .unwrap();
        journal
            .note_outcome(job, &all[2], DocumentOutcome::Failed, Some("boom"), None)
            .unwrap();

        let docs = journal.documents(job, None, 10).unwrap();
        assert_eq!(docs[0].path, all[2], "the failure leads");
        assert_eq!(docs[1].path, all[1], "then what is unfinished");
        assert_eq!(docs[2].path, all[0]);
    }

    #[test]
    fn history_is_bounded_per_root_but_keeps_a_running_job() {
        let dir = tempdir().unwrap();
        let mut journal = IndexJobJournal::open(dir.path()).unwrap();
        let root = PathBuf::from("/corpus");
        let mut ids = Vec::new();
        for _ in 0..(HISTORY_PER_ROOT + 5) {
            let job = journal.begin(&root, &paths(&["/corpus/a"])).unwrap();
            journal.finish(job, JobState::Completed, None).unwrap();
            ids.push(job);
        }
        // The newest begin() prunes; one running job is then added on top.
        let running = journal.begin(&root, &paths(&["/corpus/a"])).unwrap();
        let recent = journal.recent(100).unwrap();
        assert!(
            recent.len() <= HISTORY_PER_ROOT + 1,
            "history is bounded: {}",
            recent.len()
        );
        assert!(recent.iter().any(|j| j.id == running));
        assert!(journal.job(ids[0]).unwrap().is_none(), "oldest was pruned");
    }

    #[test]
    fn history_of_one_root_does_not_prune_another() {
        let dir = tempdir().unwrap();
        let mut journal = IndexJobJournal::open(dir.path()).unwrap();
        let kept = journal
            .begin(&PathBuf::from("/other"), &paths(&["/other/a"]))
            .unwrap();
        journal.finish(kept, JobState::Completed, None).unwrap();
        for _ in 0..(HISTORY_PER_ROOT + 3) {
            let job = journal
                .begin(&PathBuf::from("/corpus"), &paths(&["/corpus/a"]))
                .unwrap();
            journal.finish(job, JobState::Cancelled, None).unwrap();
        }
        assert!(journal.job(kept).unwrap().is_some());
    }

    /// A verdict outlives the job that reached it. Asking per job would answer
    /// "what did the last run say", and the run that read a document and found
    /// nothing in it may be three continuations ago.
    #[test]
    fn a_root_verdict_spans_the_jobs_that_reached_it() {
        let dir = tempdir().unwrap();
        let mut journal = IndexJobJournal::open(dir.path()).unwrap();
        let root = PathBuf::from("/corpus");

        let first = journal
            .begin(&root, &paths(&["/corpus/a", "/corpus/b"]))
            .unwrap();
        journal
            .note_outcome(first, &PathBuf::from("/corpus/a"), DocumentOutcome::Empty, None, Some(0))
            .unwrap();
        journal.finish(first, JobState::Cancelled, None).unwrap();

        let second = journal.begin(&root, &paths(&["/corpus/b"])).unwrap();
        journal
            .note_outcome(second, &PathBuf::from("/corpus/b"), DocumentOutcome::Empty, None, Some(0))
            .unwrap();
        journal.finish(second, JobState::Completed, None).unwrap();

        let mut empty = journal
            .paths_with_outcome_for_root(&root, DocumentOutcome::Empty)
            .unwrap();
        empty.sort();
        assert_eq!(empty, paths(&["/corpus/a", "/corpus/b"]));

        // The per-job question still gets the per-job answer.
        assert_eq!(
            journal
                .paths_with_outcome(second, DocumentOutcome::Empty)
                .unwrap(),
            paths(&["/corpus/b"])
        );
        // And another root's history is not this root's.
        assert!(journal
            .paths_with_outcome_for_root(&PathBuf::from("/elsewhere"), DocumentOutcome::Empty)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn forgetting_a_root_leaves_other_roots_intact() {
        let dir = tempdir().unwrap();
        let mut journal = IndexJobJournal::open(dir.path()).unwrap();
        let a = journal
            .begin(&PathBuf::from("/a"), &paths(&["/a/1"]))
            .unwrap();
        let b = journal
            .begin(&PathBuf::from("/b"), &paths(&["/b/1"]))
            .unwrap();
        journal.forget_root(&PathBuf::from("/a")).unwrap();
        assert!(journal.job(a).unwrap().is_none());
        assert!(journal.job(b).unwrap().is_some());

        journal.forget_all().unwrap();
        assert!(journal.job(b).unwrap().is_none());
    }

    #[test]
    fn documents_of_a_forgotten_job_go_with_it() {
        let dir = tempdir().unwrap();
        let mut journal = IndexJobJournal::open(dir.path()).unwrap();
        let root = PathBuf::from("/a");
        let job = journal.begin(&root, &paths(&["/a/1", "/a/2"])).unwrap();
        journal.forget_root(&root).unwrap();
        assert!(journal.documents(job, None, 10).unwrap().is_empty());
    }

    #[test]
    fn outcome_and_state_round_trip_through_their_strings() {
        for outcome in [
            DocumentOutcome::Pending,
            DocumentOutcome::Reused,
            DocumentOutcome::Indexed,
            DocumentOutcome::Empty,
            DocumentOutcome::Failed,
        ] {
            assert_eq!(DocumentOutcome::from_str(outcome.as_str()), outcome);
        }
        for state in [
            JobState::Running,
            JobState::Completed,
            JobState::Cancelled,
            JobState::Failed,
            JobState::Interrupted,
        ] {
            assert_eq!(JobState::from_str(state.as_str()), state);
        }
        for stage in [
            DocumentStage::Queued,
            DocumentStage::Checking,
            DocumentStage::ReadingFigures,
            DocumentStage::Extracting,
            DocumentStage::Embedding,
        ] {
            assert_eq!(DocumentStage::from_str(stage.as_str()), stage);
        }
        // Unknown strings decay to the safest reading rather than panicking.
        assert_eq!(
            DocumentOutcome::from_str("nonsense"),
            DocumentOutcome::Pending
        );
        assert_eq!(JobState::from_str("nonsense"), JobState::Running);
        assert_eq!(DocumentStage::from_str("nonsense"), DocumentStage::Queued);
    }

    #[test]
    fn counts_distinguish_saved_from_settled() {
        let counts = JobCounts {
            pending: 1,
            reused: 2,
            indexed: 3,
            empty: 4,
            failed: 5,
        };
        assert_eq!(counts.saved(), 5);
        assert_eq!(counts.settled(), 14);
    }
}
