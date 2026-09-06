//! SQLite mirror of the learning catalogues, with an FTS5 index for recall.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use rusqlite::{params, Connection};

use crate::types::{
    CatalogueGrain, CatalogueHit, CatalogueProviderStatus, CatalogueRecall, CatalogueRecord,
};

/// v2 rebuilt `catalogue_records` without the grain CHECK constraint; see
/// [`CatalogueStore::drop_grain_check`].
const SCHEMA_VERSION: i64 = 2;
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// Ceiling on one search's returned rows. Recall is meant to be wide but a
/// caller that asks for everything is asking for the table, and the table is
/// what `sync` produced — there is a status endpoint for that question.
pub const MAX_SEARCH_LIMIT: usize = 200;

/// Everything the store needs to exist, stated once so that the initial
/// creation and the post-rebuild repair cannot drift apart.
///
/// The record table and its FTS index are kept in step by triggers rather than
/// by call sites, so a future writer cannot forget one.
///
/// `grain` carries no CHECK constraint. It did in v1, which put the enum's
/// three variants into the schema: SQLite cannot drop a CHECK in place, so a
/// fourth grain would have meant a constraint no existing database could be
/// talked out of. The column's domain belongs to [`CatalogueGrain`] — every
/// writer reaches this table through `as_str`, and every reader through
/// `parse` — so the constraint restated a guarantee the type already made,
/// and charged a migration for it.
const BASE_SCHEMA: &str = r#"
            CREATE TABLE IF NOT EXISTS catalogue_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS catalogue_records (
                rowid        INTEGER PRIMARY KEY AUTOINCREMENT,
                provider     TEXT NOT NULL,
                external_id  TEXT NOT NULL,
                title        TEXT NOT NULL,
                summary      TEXT NOT NULL DEFAULT '',
                subject      TEXT NOT NULL DEFAULT '',
                authors      TEXT NOT NULL DEFAULT '',
                license      TEXT NOT NULL DEFAULT '',
                landing_url  TEXT,
                pdf_url      TEXT,
                outline_url  TEXT,
                grain        TEXT NOT NULL,
                pages        INTEGER,
                UNIQUE (provider, external_id)
            );
            CREATE INDEX IF NOT EXISTS idx_catalogue_provider
                ON catalogue_records(provider);
            CREATE INDEX IF NOT EXISTS idx_catalogue_grain
                ON catalogue_records(grain);

            CREATE TABLE IF NOT EXISTS catalogue_sync (
                provider     TEXT PRIMARY KEY,
                synced_at_ms INTEGER NOT NULL,
                records      INTEGER NOT NULL
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS catalogue_fts USING fts5(
                title, subject, summary,
                content = 'catalogue_records',
                content_rowid = 'rowid',
                tokenize = 'porter unicode61'
            );

            CREATE TRIGGER IF NOT EXISTS catalogue_ai AFTER INSERT ON catalogue_records BEGIN
                INSERT INTO catalogue_fts(rowid, title, subject, summary)
                VALUES (new.rowid, new.title, new.subject, new.summary);
            END;
            CREATE TRIGGER IF NOT EXISTS catalogue_ad AFTER DELETE ON catalogue_records BEGIN
                INSERT INTO catalogue_fts(catalogue_fts, rowid, title, subject, summary)
                VALUES ('delete', old.rowid, old.title, old.subject, old.summary);
            END;
            CREATE TRIGGER IF NOT EXISTS catalogue_au AFTER UPDATE ON catalogue_records BEGIN
                INSERT INTO catalogue_fts(catalogue_fts, rowid, title, subject, summary)
                VALUES ('delete', old.rowid, old.title, old.subject, old.summary);
                INSERT INTO catalogue_fts(rowid, title, subject, summary)
                VALUES (new.rowid, new.title, new.subject, new.summary);
            END;
"#;

/// The scratch table a v1 rebuild copies into: `catalogue_records` without the
/// grain CHECK. A second statement of the column list, which
/// `a_rebuilt_table_has_the_shape_a_fresh_one_has` exists to keep honest.
const REBUILD_TABLE: &str = "
            CREATE TABLE catalogue_records_rebuilt (
                rowid        INTEGER PRIMARY KEY AUTOINCREMENT,
                provider     TEXT NOT NULL,
                external_id  TEXT NOT NULL,
                title        TEXT NOT NULL,
                summary      TEXT NOT NULL DEFAULT '',
                subject      TEXT NOT NULL DEFAULT '',
                authors      TEXT NOT NULL DEFAULT '',
                license      TEXT NOT NULL DEFAULT '',
                landing_url  TEXT,
                pdf_url      TEXT,
                outline_url  TEXT,
                grain        TEXT NOT NULL,
                pages        INTEGER,
                UNIQUE (provider, external_id)
            );";

/// A `grain` value no variant of [`CatalogueGrain`] covers.
///
/// Only reachable from a row this crate did not write, now that the column has
/// no CHECK behind it. It is an error rather than a default because the
/// default was `Textbook`: a caller that filtered for textbooks would have
/// been handed an unknown kind of source and told it was one, which is worse
/// than the search failing and saying why.
#[derive(Debug)]
struct UnknownGrain(String);

impl std::fmt::Display for UnknownGrain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown catalogue grain {:?}", self.0)
    }
}

impl std::error::Error for UnknownGrain {}

fn parse_grain(column: usize, text: String) -> rusqlite::Result<CatalogueGrain> {
    CatalogueGrain::parse(&text).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(UnknownGrain(text)),
        )
    })
}

fn store_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("catalogue.db")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// What one provider sync actually did.
///
/// `offered` and `stored` differ whenever a provider repeats an id or ships a
/// record with no identity, which both do. Reporting only `stored` would hide
/// a provider that started sending nothing but duplicates; reporting only
/// `offered` would overstate the mirror. Both are returned so neither failure
/// is invisible.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncOutcome {
    pub offered: usize,
    pub stored: usize,
    pub duplicates: usize,
    pub unusable: usize,
}

pub struct CatalogueStore {
    conn: Connection,
}

impl CatalogueStore {
    pub fn open(data_dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("creating catalogue dir {}", data_dir.display()))?;
        let path = store_path(data_dir);
        let conn = Connection::open(&path)
            .with_context(|| format!("opening catalogue store {}", path.display()))?;
        conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    #[cfg(test)]
    pub fn in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> anyhow::Result<()> {
        let current: i64 = self
            .conn
            .query_row(
                "SELECT value FROM catalogue_meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if current >= SCHEMA_VERSION {
            return Ok(());
        }
        self.conn.execute_batch(BASE_SCHEMA)?;
        self.drop_grain_check()?;
        self.conn.execute(
            "INSERT OR REPLACE INTO catalogue_meta (key, value) VALUES ('schema_version', ?1)",
            [SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// Rebuilds `catalogue_records` without v1's grain CHECK constraint.
    ///
    /// SQLite cannot drop a CHECK in place, so the constraint has to be left
    /// behind by a table it is not part of: create, copy, drop, rename. Rowids
    /// are carried across rather than reassigned because `catalogue_fts` is an
    /// external-content index keyed by them — renumbering would leave every
    /// FTS row pointing at some other record, and the index would keep
    /// answering, wrongly.
    ///
    /// Keyed off the stored DDL rather than the version counter, so a database
    /// whose meta row was lost is repaired rather than trusted, and a database
    /// already rebuilt is not rebuilt twice.
    fn drop_grain_check(&self) -> anyhow::Result<()> {
        let ddl: String = self.conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'catalogue_records'",
            [],
            |row| row.get(0),
        )?;
        if !ddl.contains("CHECK") {
            return Ok(());
        }
        // The rename must not touch anything else: `catalogue_fts` names its
        // content table in its own DDL, and letting SQLite reparse the schema
        // mid-rebuild is how that reference gets rewritten to the scratch name.
        self.conn.pragma_update(None, "legacy_alter_table", true)?;
        let rebuild = self.conn.execute_batch(&format!(
            "BEGIN;
             {REBUILD_TABLE}
             INSERT INTO catalogue_records_rebuilt
                 (rowid, provider, external_id, title, summary, subject, authors,
                  license, landing_url, pdf_url, outline_url, grain, pages)
             SELECT rowid, provider, external_id, title, summary, subject, authors,
                    license, landing_url, pdf_url, outline_url, grain, pages
               FROM catalogue_records;
             DROP TABLE catalogue_records;
             ALTER TABLE catalogue_records_rebuilt RENAME TO catalogue_records;
             COMMIT;"
        ));
        self.conn.pragma_update(None, "legacy_alter_table", false)?;
        if rebuild.is_err() {
            // The batch stops at the failing statement, leaving the BEGIN it
            // opened; without this the connection stays in a transaction and
            // every later write fails for a reason that names neither this
            // migration nor the real fault.
            let _ = self.conn.execute_batch("ROLLBACK;");
        }
        rebuild.context("rebuilding catalogue_records without the grain CHECK")?;
        // `DROP TABLE` took the old table's indexes and triggers with it.
        self.conn.execute_batch(BASE_SCHEMA)?;
        Ok(())
    }

    /// Replaces one provider's records wholesale.
    ///
    /// Wholesale because these catalogues have no change feed: a provider can
    /// only be asked what it currently holds, so a diff would be invented
    /// rather than observed. Withdrawn titles disappearing is a feature — a
    /// record that no longer exists must stop being offered.
    pub fn replace_provider(
        &mut self,
        provider: &str,
        records: &[CatalogueRecord],
    ) -> anyhow::Result<SyncOutcome> {
        anyhow::ensure!(!provider.is_empty(), "catalogue provider id is empty");
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM catalogue_records WHERE provider = ?1",
            [provider],
        )?;
        let mut outcome = SyncOutcome {
            offered: records.len(),
            ..SyncOutcome::default()
        };
        // Providers repeat themselves: LibreTexts and MIT OpenCourseWare both
        // return the same external id more than once across a paged fetch.
        // `INSERT OR REPLACE` would absorb that silently and leave the caller
        // told it stored 4,100 records when the table gained 2,219. Dedupe
        // here instead, and report the drop, so the number a caller sees is
        // the number of rows that exist.
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        {
            let mut stmt = tx.prepare(
                "INSERT INTO catalogue_records
                    (provider, external_id, title, summary, subject, authors, license,
                     landing_url, pdf_url, outline_url, grain, pages)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            )?;
            for record in records {
                if record.external_id.trim().is_empty() || record.title.trim().is_empty() {
                    // A record with no identity or no name cannot be offered
                    // or fetched later.
                    outcome.unusable += 1;
                    continue;
                }
                if !seen.insert(record.external_id.as_str()) {
                    outcome.duplicates += 1;
                    continue;
                }
                stmt.execute(params![
                    provider,
                    record.external_id,
                    record.title,
                    record.summary,
                    record.subject,
                    record.authors,
                    record.license,
                    record.landing_url,
                    record.pdf_url,
                    record.outline_url,
                    record.grain.as_str(),
                    record.pages,
                ])?;
                outcome.stored += 1;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO catalogue_sync (provider, synced_at_ms, records)
             VALUES (?1, ?2, ?3)",
            params![provider, now_ms(), outcome.stored as i64],
        )?;
        tx.commit()?;
        Ok(outcome)
    }

    /// BM25 recall over the mirror. See the module docs for why this is not a
    /// ranking.
    ///
    /// `grains` is the set of kinds the caller will accept; empty means all of
    /// them. A set rather than one value because the distinction that matters
    /// is not always the one the caller asked for: a broad subject is better
    /// served by a course than by a textbook, but a textbook still teaches it,
    /// and filtering to the single preferred kind hides every other provider's
    /// answer. Which kinds are admissible for a given question is a judgement
    /// about the question, so it belongs to the caller — this only applies it.
    pub fn search(
        &self,
        query: &str,
        grains: &[CatalogueGrain],
        limit: usize,
    ) -> anyhow::Result<CatalogueRecall> {
        let terms = query_terms(query);
        if terms.is_empty() {
            // Not an error: a probe can legitimately reduce to stopwords, and
            // "no terms survived" is a real answer to give back — which is why
            // the terms travel with the hits rather than being inferred from an
            // empty result that has two quite different causes.
            return Ok(CatalogueRecall {
                terms,
                hits: Vec::new(),
            });
        }
        let expression = fts_expression(&query_phrases(query), &terms);
        let limit = limit.clamp(1, MAX_SEARCH_LIMIT);
        // bm25() is negative-better in FTS5; negate so a larger score is a
        // better match and callers do not have to know that.
        let sql = "SELECT r.provider, r.external_id, r.title, r.summary, r.subject,
                          r.authors, r.license, r.landing_url, r.pdf_url, r.outline_url,
                          r.grain, r.pages, -bm25(catalogue_fts, 4.0, 2.0, 1.0)
                     FROM catalogue_fts
                     JOIN catalogue_records r ON r.rowid = catalogue_fts.rowid
                    WHERE catalogue_fts MATCH ?1
                      AND (?2 IS NULL OR r.grain IN (SELECT value FROM json_each(?2)))
                    ORDER BY bm25(catalogue_fts, 4.0, 2.0, 1.0)
                    LIMIT ?3";
        // A JSON array rather than a generated placeholder list, so the SQL
        // stays one static string whatever the caller accepts.
        let accepted: Option<String> = if grains.is_empty() {
            None
        } else {
            Some(serde_json::to_string(
                &grains.iter().map(|g| g.as_str()).collect::<Vec<_>>(),
            )?)
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![expression, accepted, limit as i64], |row| {
            let grain_text: String = row.get(10)?;
            let record = CatalogueRecord {
                provider: row.get(0)?,
                external_id: row.get(1)?,
                title: row.get(2)?,
                summary: row.get(3)?,
                subject: row.get(4)?,
                authors: row.get(5)?,
                license: row.get(6)?,
                landing_url: row.get(7)?,
                pdf_url: row.get(8)?,
                outline_url: row.get(9)?,
                grain: parse_grain(10, grain_text)?,
                pages: row.get(11)?,
            };
            Ok(CatalogueHit {
                acquisition: super::providers::acquisition(&record),
                record,
                recall_score: row.get(12)?,
            })
        })?;
        let hits = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(CatalogueRecall { terms, hits })
    }

    pub fn status(&self) -> anyhow::Result<Vec<CatalogueProviderStatus>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.provider, r.grain, COUNT(*), s.synced_at_ms
               FROM catalogue_records r
               LEFT JOIN catalogue_sync s ON s.provider = r.provider
              GROUP BY r.provider, r.grain
              ORDER BY r.provider",
        )?;
        let rows = stmt.query_map([], |row| {
            let grain_text: String = row.get(1)?;
            Ok(CatalogueProviderStatus {
                provider: row.get(0)?,
                grain: parse_grain(1, grain_text)?,
                records: row.get(2)?,
                synced_at_ms: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn total_records(&self) -> anyhow::Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM catalogue_records", [], |row| {
                row.get(0)
            })?)
    }
}

/// The terms free text reduces to, in the order they were taken.
///
/// Separate from [`fts_expression`] because the terms are an answer in their
/// own right: a caller handed an empty result needs to know whether the mirror
/// held nothing or the query held nothing, and only this function knows.
fn query_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for raw in query.split(|c: char| !c.is_alphanumeric()) {
        // Character-aware throughout: `chars().count()`, never a byte length.
        if raw.chars().count() < 2 {
            continue;
        }
        let lowered = raw.to_lowercase();
        if STOPWORDS.contains(&lowered.as_str()) {
            continue;
        }
        if !terms.contains(&lowered) {
            terms.push(lowered);
        }
        if terms.len() >= MAX_QUERY_TERMS {
            break;
        }
    }
    terms
}

/// The contiguous word runs a query contains, each kept whole as a phrase.
///
/// A term of art is not the sum of its terms. `difference in differences`
/// loses `in` to the stopword list and both remaining words stem to `differ`,
/// so by the time it reaches FTS5 it is a one-word query for `difference` —
/// which is why it ranked anthropology courses ahead of the econometrics
/// course whose blurb says `differences-in-differences` in as many words. The
/// run keeps the words adjacent and in order, which is the only form in which
/// that query is distinguishable from a query about difference.
///
/// Stopwords stay in: here they are not signal, they are position. Words are
/// rebuilt from their alphanumeric characters and joined with single spaces,
/// so a run can carry no character that FTS5 would read as syntax, and the
/// phrase this produces is tokenized exactly as the indexed text was — which
/// is what lets the query match `differences-in-differences` across its
/// hyphens.
///
/// Runs are bounded by [`MAX_QUERY_TERMS`], the same budget the terms spend:
/// a clause longer than that is prose rather than a name, and matching it as a
/// phrase would cost a positional scan to find nothing.
fn query_phrases(query: &str) -> Vec<String> {
    let mut phrases: Vec<String> = Vec::new();
    let mut budget = MAX_QUERY_TERMS;
    // Punctuation ends a run; spaces, hyphens and apostrophes are interior to
    // one, so `difference-in-differences` typed with hyphens is the same run as
    // the same words typed with spaces.
    for run in query.split(|c: char| !(c.is_alphanumeric() || c == ' ' || c == '-' || c == '\'')) {
        let words: Vec<String> = run
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(|word| word.to_lowercase())
            .collect();
        // One word is already a term; a phrase of it would say nothing new.
        if words.len() < 2 || words.len() > budget {
            continue;
        }
        budget -= words.len();
        phrases.push(words.join(" "));
    }
    phrases
}

/// Turns a query's phrases and terms into an FTS5 MATCH expression.
///
/// The input is a description written by a caller — prose, punctuation and
/// all — and FTS5's query language would read `NP-complete` as a NOT and
/// `"quoted"` as a phrase, so raw text is a syntax error waiting to happen.
/// Everything is therefore quoted and OR-ed: OR because this is a recall
/// stage, where a record matching four of a probe's fifteen terms is exactly
/// the sort of thing that must survive to be ranked properly later.
///
/// The phrases from [`query_phrases`] are OR-ed in alongside the terms rather
/// than replacing them, so the recall set is unchanged — a record matching a
/// phrase already matched that phrase's words — and only the order moves. A
/// record that matched the whole run scores it in addition to each of its
/// words, which is what lifts the record that names the thing above the
/// records that merely share a word with it.
///
/// This reaches a term of art only where a record spells it out. A concept a
/// record teaches under other words — `causal inference` in a blurb that says
/// "instrumental variables" and "program evaluation" — is not reachable from
/// any lexical expression, and is not what this is for.
fn fts_expression(phrases: &[String], terms: &[String]) -> String {
    phrases
        .iter()
        .chain(terms.iter())
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// A blurb-shaped probe is mostly connective tissue; these carry no signal and
/// each one costs a full posting-list scan.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "into", "is", "it",
    "its", "of", "on", "or", "that", "the", "their", "them", "then", "there", "these", "this",
    "to", "was", "were", "which", "who", "with", "who", "you", "your",
];

/// Long probes are welcome; unbounded ones are not. Twenty-four terms is well
/// past what a two-sentence description contributes after stopwords.
const MAX_QUERY_TERMS: usize = 24;

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, title: &str, summary: &str, grain: CatalogueGrain) -> CatalogueRecord {
        CatalogueRecord {
            provider: "test".into(),
            external_id: id.into(),
            title: title.into(),
            summary: summary.into(),
            subject: "computer science".into(),
            authors: "Someone".into(),
            license: "cc-by".into(),
            landing_url: Some("https://example.invalid/x".into()),
            pdf_url: None,
            outline_url: None,
            grain,
            pages: Some(200),
        }
    }

    #[test]
    fn a_blurb_shaped_probe_finds_the_book_it_describes() {
        let mut store = CatalogueStore::in_memory().expect("store");
        store
            .replace_provider(
                "test",
                &[
                    record(
                        "1",
                        "Combinatorial Optimization",
                        "Computational complexity, NP-completeness and polynomial-time reductions.",
                        CatalogueGrain::Textbook,
                    ),
                    record(
                        "2",
                        "Introduction to Marine Biology",
                        "Tide pools, coral reefs and the ecology of shallow seas.",
                        CatalogueGrain::Textbook,
                    ),
                ],
            )
            .expect("replace");

        let hits = store
            .search(
                "An introductory treatment of computational complexity covering NP-completeness \
                 and polynomial-time reductions.",
                &[],
                5,
            )
            .expect("search")
            .hits;

        assert_eq!(
            hits.first().map(|h| h.record.title.as_str()),
            Some("Combinatorial Optimization")
        );
    }

    /// The case the phrase handling exists for, taken from the mirror: MIT
    /// OpenCourseWare's applied econometrics blurb says
    /// `differences-in-differences`, and before the run was kept whole this
    /// query returned `Identity and Difference` and three more like it while
    /// the econometrics course did not appear at all. Both words stem to
    /// `differ` and `in` is a stopword, so the terms alone cannot tell the two
    /// records apart.
    #[test]
    fn a_term_of_art_outranks_the_records_that_merely_share_a_word() {
        let mut store = CatalogueStore::in_memory().expect("store");
        store
            .replace_provider(
                "test",
                &[
                    record(
                        "1",
                        "Identity and Difference",
                        "This course explores how identities, whether of individuals or \
                         groups, are produced, maintained and transformed.",
                        CatalogueGrain::Course,
                    ),
                    record(
                        "2",
                        "Psychology of Gender",
                        "Current research and theory regarding the validity of commonly \
                         accepted gender differences in many realms.",
                        CatalogueGrain::Course,
                    ),
                    record(
                        "3",
                        "Applied Econometrics: Mostly Harmless Big Data",
                        "This course covers empirical strategies for applied micro research \
                         questions: regression and matching, instrumental variables, \
                         differences-in-differences, regression discontinuity designs.",
                        CatalogueGrain::Course,
                    ),
                ],
            )
            .expect("replace");

        let hits = store
            .search("difference in differences", &[], 5)
            .expect("search")
            .hits;

        assert_eq!(
            hits.first().map(|h| h.record.title.as_str()),
            Some("Applied Econometrics: Mostly Harmless Big Data")
        );
        // The others are still recalled: the phrase moves the order, it does
        // not narrow the set.
        assert_eq!(hits.len(), 3);
    }

    /// Hyphens are the provider's choice, not the reader's. `unicode61` splits
    /// on them, so the words are what has to line up.
    #[test]
    fn a_hyphenated_query_and_a_hyphenated_record_meet_in_the_middle() {
        let mut store = CatalogueStore::in_memory().expect("store");
        store
            .replace_provider(
                "test",
                &[
                    record(
                        "1",
                        "Differences in Learning",
                        "How difference shapes the classroom.",
                        CatalogueGrain::Course,
                    ),
                    record(
                        "2",
                        "Program Evaluation",
                        "Panel methods, including differences-in-differences.",
                        CatalogueGrain::Course,
                    ),
                ],
            )
            .expect("replace");

        for probe in ["difference-in-differences", "difference in differences"] {
            let hits = store.search(probe, &[], 5).expect("search").hits;
            assert_eq!(
                hits.first().map(|h| h.record.title.as_str()),
                Some("Program Evaluation"),
                "probe {probe:?}"
            );
        }
    }

    #[test]
    fn a_run_keeps_its_stopwords_and_stops_at_punctuation() {
        assert_eq!(
            query_phrases("difference in differences"),
            vec!["difference in differences".to_string()]
        );
        // A comma ends a run: two names, not one ten-word phrase.
        assert_eq!(
            query_phrases("instrumental variables, regression discontinuity"),
            vec![
                "instrumental variables".to_string(),
                "regression discontinuity".to_string()
            ]
        );
        // One word is already a term.
        assert!(query_phrases("econometrics").is_empty());
        // Nothing a run can carry is FTS5 syntax.
        assert_eq!(
            query_phrases("\"C++ / C#\" AND (x"),
            Vec::<String>::new(),
            "single-word runs only"
        );
        assert_eq!(
            query_phrases("NP-complete reductions"),
            vec!["np complete reductions".to_string()]
        );
    }

    /// A blurb-shaped probe spends the same budget the terms do, so a long
    /// clause cannot turn one search into a dozen positional scans.
    #[test]
    fn phrase_runs_are_bounded_by_the_term_budget() {
        let long = (0..40)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(query_phrases(&long).is_empty());

        let mut probe = long.clone();
        probe.push_str(", causal inference");
        // The clause that does not fit is skipped; the one that fits is kept.
        assert_eq!(query_phrases(&probe), vec!["causal inference".to_string()]);
    }

    #[test]
    fn punctuation_that_is_fts_syntax_does_not_blow_up_the_query() {
        let store = CatalogueStore::in_memory().expect("store");
        // Each of these is a syntax error if handed to FTS5 verbatim.
        for probe in [
            "NP-complete",
            "\"unterminated",
            "a OR b AND (c",
            "C++ / C#",
            "*",
        ] {
            store.search(probe, &[], 5).expect("must not error");
        }
    }

    #[test]
    fn a_probe_of_only_stopwords_returns_nothing_rather_than_everything() {
        let mut store = CatalogueStore::in_memory().expect("store");
        store
            .replace_provider(
                "test",
                &[record("1", "Anything", "At all", CatalogueGrain::Textbook)],
            )
            .expect("replace");
        assert!(store
            .search("the and of it is", &[], 5)
            .expect("search")
            .hits
            .is_empty());
    }

    #[test]
    fn grain_filters_out_the_wrong_kind_of_source() {
        let mut store = CatalogueStore::in_memory().expect("store");
        store
            .replace_provider(
                "test",
                &[
                    record(
                        "1",
                        "Python Language Reference",
                        "Built-in sequence types and lists.",
                        CatalogueGrain::Reference,
                    ),
                    record(
                        "2",
                        "Python for Beginners",
                        "Built-in sequence types and lists.",
                        CatalogueGrain::Textbook,
                    ),
                ],
            )
            .expect("replace");
        let hits = store
            .search(
                "built-in sequence types and lists",
                &[CatalogueGrain::Reference],
                5,
            )
            .expect("search")
            .hits;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].record.external_id, "1");
    }

    /// Accepting two kinds returns both, and that is the point of the set.
    ///
    /// A learner who declares a broad subject is best served by a course, so
    /// the query stage says `course` — and a mirror where only one provider
    /// publishes courses would then answer with that one provider and nothing
    /// else, hiding every textbook on the subject. Measured against the live
    /// mirror on 2026-08-25: "organic chemistry" filtered to `course` returned
    /// 24 MIT OpenCourseWare records and zero LibreTexts, which holds an
    /// organic chemistry library.
    #[test]
    fn a_query_that_accepts_two_grains_is_answered_by_both() {
        let mut store = CatalogueStore::in_memory().expect("store");
        store
            .replace_provider(
                "test",
                &[
                    record(
                        "1",
                        "Organic Chemistry I",
                        "Structure, bonding and reaction mechanisms.",
                        CatalogueGrain::Course,
                    ),
                    record(
                        "2",
                        "Organic Chemistry",
                        "Structure, bonding and reaction mechanisms.",
                        CatalogueGrain::Textbook,
                    ),
                    record(
                        "3",
                        "Organic Chemistry Reference",
                        "Structure, bonding and reaction mechanisms.",
                        CatalogueGrain::Reference,
                    ),
                ],
            )
            .expect("replace");
        let hits = store
            .search(
                "structure bonding and reaction mechanisms",
                &[CatalogueGrain::Course, CatalogueGrain::Textbook],
                10,
            )
            .expect("search")
            .hits;
        let ids: Vec<&str> = hits.iter().map(|h| h.record.external_id.as_str()).collect();
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert!(ids.contains(&"1") && ids.contains(&"2"), "{ids:?}");
        // And the kind that was not accepted stays out: a set is still a
        // filter, not a suggestion.
        assert!(!ids.contains(&"3"), "{ids:?}");
    }

    #[test]
    fn resyncing_a_provider_withdraws_records_it_no_longer_offers() {
        let mut store = CatalogueStore::in_memory().expect("store");
        store
            .replace_provider(
                "test",
                &[
                    record(
                        "1",
                        "Kept Book",
                        "Graph algorithms.",
                        CatalogueGrain::Textbook,
                    ),
                    record(
                        "2",
                        "Withdrawn Book",
                        "Graph algorithms.",
                        CatalogueGrain::Textbook,
                    ),
                ],
            )
            .expect("first");
        store
            .replace_provider(
                "test",
                &[record(
                    "1",
                    "Kept Book",
                    "Graph algorithms.",
                    CatalogueGrain::Textbook,
                )],
            )
            .expect("second");

        assert_eq!(store.total_records().expect("count"), 1);
        // The FTS index must have been withdrawn with the row, not merely the
        // table: a stale posting would surface a record that cannot be fetched.
        let hits = store
            .search("graph algorithms", &[], 10)
            .expect("search")
            .hits;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].record.title, "Kept Book");
    }

    #[test]
    fn a_record_without_identity_or_title_is_not_stored() {
        let mut store = CatalogueStore::in_memory().expect("store");
        let outcome = store
            .replace_provider(
                "test",
                &[
                    record("", "No Id", "x", CatalogueGrain::Textbook),
                    record("3", "   ", "x", CatalogueGrain::Textbook),
                    record("4", "Real Book", "x", CatalogueGrain::Textbook),
                ],
            )
            .expect("replace");
        assert_eq!(outcome.stored, 1);
        assert_eq!(outcome.unusable, 2);
        assert_eq!(store.total_records().expect("count"), 1);
    }

    #[test]
    fn a_provider_that_repeats_an_id_is_counted_honestly() {
        let mut store = CatalogueStore::in_memory().expect("store");
        let outcome = store
            .replace_provider(
                "test",
                &[
                    record("1", "Once", "Graph algorithms.", CatalogueGrain::Textbook),
                    record("1", "Again", "Graph algorithms.", CatalogueGrain::Textbook),
                    record("2", "Other", "Graph algorithms.", CatalogueGrain::Textbook),
                ],
            )
            .expect("replace");
        assert_eq!(outcome.offered, 3);
        assert_eq!(outcome.stored, 2);
        assert_eq!(outcome.duplicates, 1);
        assert_eq!(store.total_records().expect("count"), 2);
    }

    #[test]
    fn a_probe_reports_the_terms_it_was_run_with() {
        let store = CatalogueStore::in_memory().expect("store");
        // Two empty results with different causes. Only the terms tell them
        // apart, which is why they are returned rather than inferred.
        let nothing_usable = store.search("the and of it", &[], 5).expect("search");
        assert!(nothing_usable.terms.is_empty());
        assert!(nothing_usable.hits.is_empty());

        let nothing_matched = store
            .search("hydrodynamic stability", &[], 5)
            .expect("search");
        assert_eq!(nothing_matched.terms, ["hydrodynamic", "stability"]);
        assert!(nothing_matched.hits.is_empty());

        // A one-character term is dropped, and saying so is the difference
        // between "we have nothing on C" and "we never looked".
        let single_letter = store.search("C", &[], 5).expect("search");
        assert!(single_letter.terms.is_empty());
    }

    #[test]
    fn status_reports_what_each_provider_holds() {
        let mut store = CatalogueStore::in_memory().expect("store");
        store
            .replace_provider("test", &[record("1", "One", "x", CatalogueGrain::Course)])
            .expect("replace");
        let status = store.status().expect("status");
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].provider, "test");
        assert_eq!(status[0].records, 1);
        assert_eq!(status[0].grain, CatalogueGrain::Course);
        assert!(status[0].synced_at_ms.is_some());
    }

    /// The v1 schema, stated as today's schema plus the constraint v1 had, so
    /// this fixture cannot drift away from the thing being migrated.
    fn v1_schema() -> String {
        let with_check = BASE_SCHEMA.replace(
            "grain        TEXT NOT NULL,",
            "grain        TEXT NOT NULL CHECK (grain IN ('textbook','course','reference')),",
        );
        assert!(
            with_check.contains("CHECK"),
            "v1 fixture lost its constraint"
        );
        with_check
    }

    fn columns(conn: &Connection, table: &str) -> Vec<(String, String, i64)> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .expect("table_info");
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows");
        rows
    }

    /// Writes a v1 database with one record in it, and returns its directory.
    fn v1_database(grain: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = Connection::open(dir.path().join("catalogue.db")).expect("open");
        conn.execute_batch(&v1_schema()).expect("v1 schema");
        conn.execute(
            "INSERT INTO catalogue_records
                (rowid, provider, external_id, title, summary, subject, authors,
                 license, landing_url, pdf_url, outline_url, grain, pages)
             VALUES (7,'test','1','Convex Optimization','Duality and gradient methods.',
                     'mathematics','Someone','cc-by',NULL,NULL,NULL,?1,400)",
            [grain],
        )
        .expect("insert");
        conn.execute(
            "INSERT OR REPLACE INTO catalogue_meta (key, value) VALUES ('schema_version','1')",
            [],
        )
        .expect("version");
        dir
    }

    #[test]
    fn opening_a_v1_database_drops_the_grain_check_and_keeps_the_rows() {
        let dir = v1_database("textbook");
        let store = CatalogueStore::open(dir.path()).expect("open");

        let ddl: String = store
            .conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='catalogue_records'",
                [],
                |row| row.get(0),
            )
            .expect("ddl");
        assert!(!ddl.contains("CHECK"), "{ddl}");

        // The row survived, at the rowid it had. Anything else and the FTS
        // index — which is keyed by that rowid — would be pointing elsewhere.
        let (rowid, title): (i64, String) = store
            .conn
            .query_row(
                "SELECT rowid, title FROM catalogue_records WHERE external_id = '1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(rowid, 7);
        assert_eq!(title, "Convex Optimization");

        // And the index still answers for it, through the rebuilt table.
        let hits = store
            .search("duality and gradient methods", &[], 10)
            .expect("search")
            .hits;
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].record.external_id, "1");
    }

    #[test]
    fn a_migrated_database_accepts_a_grain_the_old_constraint_would_have_refused() {
        let dir = v1_database("textbook");
        let store = CatalogueStore::open(dir.path()).expect("open");
        // The point of the rebuild: the schema no longer holds an opinion the
        // enum would have to be talked out of.
        store
            .conn
            .execute(
                "INSERT INTO catalogue_records
                    (provider, external_id, title, grain)
                 VALUES ('test','2','Later Grain','monograph')",
                [],
            )
            .expect("a fourth grain must not be refused by the schema");
    }

    #[test]
    fn a_rebuilt_table_has_the_shape_a_fresh_one_has() {
        let migrated_dir = v1_database("course");
        let migrated = CatalogueStore::open(migrated_dir.path()).expect("open migrated");
        let fresh_dir = tempfile::tempdir().expect("tempdir");
        let fresh = CatalogueStore::open(fresh_dir.path()).expect("open fresh");
        assert_eq!(
            columns(&migrated.conn, "catalogue_records"),
            columns(&fresh.conn, "catalogue_records"),
            "REBUILD_TABLE has drifted from BASE_SCHEMA"
        );
        // The triggers and indexes went with the dropped table; they have to
        // have come back, or writes would stop reaching the index silently.
        for object in [
            "catalogue_ai",
            "catalogue_ad",
            "catalogue_au",
            "idx_catalogue_provider",
        ] {
            let present: i64 = migrated
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                    [object],
                    |row| row.get(0),
                )
                .expect("count");
            assert_eq!(present, 1, "{object} did not survive the rebuild");
        }
    }

    #[test]
    fn opening_a_migrated_database_again_is_a_no_op() {
        let dir = v1_database("reference");
        drop(CatalogueStore::open(dir.path()).expect("first open"));
        let store = CatalogueStore::open(dir.path()).expect("second open");
        assert_eq!(store.total_records().expect("count"), 1);
        let scratch: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'catalogue_records_rebuilt'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(scratch, 0, "the scratch table outlived the rebuild");
    }

    #[test]
    fn an_unknown_grain_is_reported_rather_than_served_as_a_textbook() {
        // Written after the migration, because v1's CHECK would have refused
        // it — which is the whole point: with the constraint gone, nothing in
        // the database turns this row away, so the read path has to.
        let dir = v1_database("textbook");
        let store = CatalogueStore::open(dir.path()).expect("open");
        store
            .conn
            .execute(
                "UPDATE catalogue_records SET grain = 'monograph' WHERE external_id = '1'",
                [],
            )
            .expect("update");
        let error = store
            .search("duality and gradient methods", &[], 10)
            .expect_err("an unknown grain must not be served as a textbook");
        assert!(
            format!("{error:#}").contains("monograph"),
            "the failure must name the grain it could not read: {error:#}"
        );
        store
            .status()
            .expect_err("status must not launder it either");
    }
}
