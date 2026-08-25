//! SQLite mirror of the teaching catalogues, with an FTS5 index for recall.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use rusqlite::{params, Connection};

use crate::types::{CatalogueGrain, CatalogueHit, CatalogueProviderStatus, CatalogueRecord};

const SCHEMA_VERSION: i64 = 1;
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// Ceiling on one search's returned rows. Recall is meant to be wide but a
/// caller that asks for everything is asking for the table, and the table is
/// what `sync` produced — there is a status endpoint for that question.
pub const MAX_SEARCH_LIMIT: usize = 200;

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
        // The record table and its FTS index are kept in step by triggers
        // rather than by call sites, so a future writer cannot forget one.
        self.conn.execute_batch(
            r#"
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
                grain        TEXT NOT NULL CHECK (grain IN ('textbook','course','reference')),
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
            "#,
        )?;
        self.conn.execute(
            "INSERT OR REPLACE INTO catalogue_meta (key, value) VALUES ('schema_version', ?1)",
            [SCHEMA_VERSION.to_string()],
        )?;
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
    ) -> anyhow::Result<Vec<CatalogueHit>> {
        let expression = fts_expression(query);
        if expression.is_empty() {
            // Not an error: a probe can legitimately reduce to stopwords, and
            // "no terms survived" is a real answer to give back.
            return Ok(Vec::new());
        }
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
            Ok(CatalogueHit {
                record: CatalogueRecord {
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
                    grain: CatalogueGrain::parse(&grain_text).unwrap_or(CatalogueGrain::Textbook),
                    pages: row.get(11)?,
                },
                recall_score: row.get(12)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
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
                grain: CatalogueGrain::parse(&grain_text).unwrap_or(CatalogueGrain::Textbook),
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

/// Turns free text into an FTS5 MATCH expression.
///
/// The input is a description written by a caller — prose, punctuation and
/// all — and FTS5's query language would read `NP-complete` as a NOT and
/// `"quoted"` as a phrase, so raw text is a syntax error waiting to happen.
/// Every term is therefore extracted, quoted and OR-ed: OR because this is a
/// recall stage, where a record matching four of a probe's fifteen terms is
/// exactly the sort of thing that must survive to be ranked properly later.
fn fts_expression(query: &str) -> String {
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
        .into_iter()
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

/// Long probes are welcome; unbounded ones are not. Fifteen terms is well past
/// what a two-sentence description contributes after stopwords.
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
            .expect("search");

        assert_eq!(
            hits.first().map(|h| h.record.title.as_str()),
            Some("Combinatorial Optimization")
        );
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
            .expect("search");
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
            .expect("search");
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
        let hits = store.search("graph algorithms", &[], 10).expect("search");
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
}
