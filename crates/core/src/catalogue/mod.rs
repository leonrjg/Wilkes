//! A local mirror of the open teaching catalogues, and text search over it.
//!
//! # Why a mirror rather than a passthrough
//!
//! Every provider here exposes a search endpoint, and every one of them ranks
//! badly for this purpose. Asked for `NP-complete optimization`, Wikibooks'
//! full-text search returns `Statistics/Distributions/Binomial`, having matched
//! `np` inside a probability formula; OpenAlex returns a well-cited physics
//! paper on Ising formulations. Both are working correctly. Neither is ranking
//! for *teaches this to someone who does not know it*, because neither was
//! asked to and neither could be.
//!
//! What makes a passthrough avoidable is that the teaching catalogues are
//! small. Papers number in the hundreds of millions; open textbooks, courses
//! and documentation sets number in the thousands. A few thousand records fit
//! in a table, which means the ranking can happen here, against a query the
//! caller wrote, instead of inside a remote index tuned for something else.
//!
//! # What this returns, and what it deliberately does not
//!
//! [`CatalogueStore::search`] returns **recall**: BM25 over title, subject and
//! summary, wide enough that the right record is somewhere in the returned
//! few dozen. It is not a final ranking and must not be consumed as one.
//!
//! That is a boundary, not modesty. Deciding which of these records is the
//! best next thing for a reader requires knowing what that reader already
//! knows, which is not a fact about documents and is not something Wilkes
//! holds. The caller re-ranks. This narrows.

pub mod ocw;
pub mod providers;
pub mod store;

pub use providers::{registry, CatalogueSource};
pub use store::CatalogueStore;
