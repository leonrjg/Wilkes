//! How many worker processes may hold models at once, for the whole process.
//!
//! # Why this is not per-manager
//!
//! A `WorkerManager` holds one worker and supervises one [`WorkerKind`], and
//! the host builds three of them — embed, generate, recognize — per
//! `AppContext`. But an `AppContext` is per *workspace*, and the host opens one
//! for every workspace anything reaches: the HTTP API opens one per `corpus_id`
//! it is asked about, a managed import opens one for the corpus and another for
//! each projection of it, and every one of them stays open for the life of the
//! application. Four live at once is ordinary.
//!
//! Nothing in that arrangement is wrong on its own. What was wrong is that each
//! context's managers were the only thing bounding residency, so the bound was
//! per workspace and the memory was per machine. A recognize worker keeps four
//! models resident by design — the layout detector, the page reader, the
//! formula reader and the table reader alternate continuously within a single
//! document — and PaddleOCR-VL alone is 1.8 GB of weights. Two workspaces
//! reading at once is two of those, and neither manager could see the other to
//! know it.
//!
//! So the bound belongs here, above every manager, keyed by the only thing that
//! describes what a worker costs: its kind.
//!
//! # Why one permit per kind, and not one overall
//!
//! Reading a document alternates recognition and embedding file by file, which
//! is why those are separate managers to begin with — sharing one would reload
//! a model on every alternation. A single flat cap low enough to bound memory
//! would therefore deadlock one build against itself. One permit per kind
//! cannot: a build holds one of each and needs no more, so a build that ran
//! before this existed runs unchanged. What waits is the *second* workspace
//! asking for the same kind, which is exactly the duplication that ran the
//! machine out of memory.
//!
//! # What waiting means
//!
//! A permit is held from the spawn of a worker to its death, so a second
//! context's first request blocks until the first context's worker goes. That
//! is not indefinite: a worker that stops being asked for anything dies at its
//! idle timeout and releases the permit. Two builds over two workspaces
//! therefore run one after the other rather than both at once — which is the
//! intended trade, and the alternative to it is the swap thrash that made
//! neither of them finish.

use std::sync::{Arc, OnceLock};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::ipc::WorkerKind;

/// Worker processes of one kind that may hold models at once.
///
/// One. A second process of the same kind is a second copy of the same weights,
/// and there is no reading it makes possible that the first could not serve.
const RESIDENT_WORKERS_PER_KIND: usize = 1;

/// The permit a live worker holds. Dropping it admits the next one.
pub(super) type Residency = OwnedSemaphorePermit;

fn semaphores() -> &'static [Arc<Semaphore>; 3] {
    static SEMAPHORES: OnceLock<[Arc<Semaphore>; 3]> = OnceLock::new();
    SEMAPHORES.get_or_init(|| {
        [
            Arc::new(Semaphore::new(RESIDENT_WORKERS_PER_KIND)),
            Arc::new(Semaphore::new(RESIDENT_WORKERS_PER_KIND)),
            Arc::new(Semaphore::new(RESIDENT_WORKERS_PER_KIND)),
        ]
    })
}

/// The one semaphore that admits workers of `kind`, for this process.
pub(super) fn for_kind(kind: WorkerKind) -> Arc<Semaphore> {
    let index = match kind {
        WorkerKind::Embed => 0,
        WorkerKind::Generate => 1,
        WorkerKind::Recognize => 2,
    };
    Arc::clone(&semaphores()[index])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_has_its_own_semaphore() {
        assert!(!Arc::ptr_eq(
            &for_kind(WorkerKind::Embed),
            &for_kind(WorkerKind::Recognize)
        ));
        assert!(!Arc::ptr_eq(
            &for_kind(WorkerKind::Embed),
            &for_kind(WorkerKind::Generate)
        ));
    }

    /// The same kind asked for twice is the same semaphore, which is the whole
    /// point: two managers in two contexts must contend, and they can only
    /// contend if they are handed the same one.
    #[test]
    fn the_same_kind_is_the_same_semaphore_every_time() {
        assert!(Arc::ptr_eq(
            &for_kind(WorkerKind::Recognize),
            &for_kind(WorkerKind::Recognize)
        ));
    }
}
