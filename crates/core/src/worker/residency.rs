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
//! context's first request blocks until the first context's worker goes. A
//! worker that stops being asked for anything dies at its idle timeout and
//! releases the permit — but one that keeps being asked never idles, and
//! waiting for that alone let one workspace's searches hold the kind for an
//! hour while another's build waited. So a holder that another manager is
//! waiting behind also gives the permit up at its next gap between requests,
//! once it has held it for [`MIN_TENURE_WHILE_OTHERS_WAIT`], and the waiters
//! are admitted in arrival order. Two workspaces sharing a kind therefore take
//! turns rather than both running at once — the intended trade, since the
//! alternative is the swap thrash that made neither of them finish — and the
//! tenure bounds how often a turn costs a model load.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::{AcquireError, Notify, OwnedSemaphorePermit, Semaphore};

use super::ipc::WorkerKind;

/// Worker processes of one kind that may hold models at once.
///
/// One. A second process of the same kind is a second copy of the same weights,
/// and there is no reading it makes possible that the first could not serve.
const RESIDENT_WORKERS_PER_KIND: usize = 1;

/// The permit a live worker holds. Dropping it admits the next one.
pub(super) type Residency = OwnedSemaphorePermit;

/// How long a worker keeps residency once it has it, before another
/// workspace's waiting manager may take it at the worker's next gap.
///
/// Waiting for the holder's *idle timeout* was not a bound at all: a holder
/// that kept being asked for something never idled. On 2026-11-05 a stream of
/// searches against one projection kept its worker alive for an hour while a
/// second projection's build waited for a worker the whole time, and the
/// build's caller gave up after twelve minutes. So a holder that someone is
/// waiting behind gives the permit up at its next gap between requests.
///
/// Not at once, because every hand-off is a model load on the other side and
/// another when it comes back — eighteen seconds for an 8B model. Thirty
/// seconds of tenure bounds how often two workspaces sharing one kind can
/// trade the worker, so each gets real work done between loads.
pub(super) const MIN_TENURE_WHILE_OTHERS_WAIT: Duration = Duration::from_secs(30);

/// Who may hold a worker of one kind, and who is waiting to.
pub(super) struct KindResidency {
    permits: Arc<Semaphore>,
    /// Managers blocked in [`Self::admit`]. Read by holders deciding whether
    /// to give the permit up at their next gap.
    waiting: AtomicUsize,
    /// Raised each time a manager starts waiting, so an idle holder learns
    /// of it without polling.
    waiter_arrived: Notify,
}

impl KindResidency {
    pub(super) fn new(permits: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(permits)),
            waiting: AtomicUsize::new(0),
            waiter_arrived: Notify::new(),
        }
    }

    /// A permit, waiting in arrival order for one if none is free. Counted
    /// among the waiters for exactly as long as it waits, including when the
    /// caller abandons the wait by dropping this future.
    pub(super) async fn admit(&self) -> Result<Residency, AcquireError> {
        if let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() {
            return Ok(permit);
        }
        struct Waiting<'a>(&'a AtomicUsize);
        impl Drop for Waiting<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::AcqRel);
            }
        }
        self.waiting.fetch_add(1, Ordering::AcqRel);
        let _waiting = Waiting(&self.waiting);
        self.waiter_arrived.notify_waiters();
        Arc::clone(&self.permits).acquire_owned().await
    }

    /// Whether another manager is waiting for a permit of this kind.
    pub(super) fn others_waiting(&self) -> bool {
        self.waiting.load(Ordering::Acquire) > 0
    }

    /// Resolves once another manager is waiting. Immediately if one already is.
    pub(super) async fn until_others_wait(&self) {
        loop {
            let arrived = self.waiter_arrived.notified();
            tokio::pin!(arrived);
            arrived.as_mut().enable();
            if self.others_waiting() {
                return;
            }
            arrived.await;
        }
    }

    #[cfg(test)]
    pub(super) fn available_permits(&self) -> usize {
        self.permits.available_permits()
    }
}

fn kinds() -> &'static [Arc<KindResidency>; 3] {
    static KINDS: OnceLock<[Arc<KindResidency>; 3]> = OnceLock::new();
    KINDS.get_or_init(|| {
        [
            Arc::new(KindResidency::new(RESIDENT_WORKERS_PER_KIND)),
            Arc::new(KindResidency::new(RESIDENT_WORKERS_PER_KIND)),
            Arc::new(KindResidency::new(RESIDENT_WORKERS_PER_KIND)),
        ]
    })
}

/// The one residency that admits workers of `kind`, for this process.
pub(super) fn for_kind(kind: WorkerKind) -> Arc<KindResidency> {
    let index = match kind {
        WorkerKind::Embed => 0,
        WorkerKind::Generate => 1,
        WorkerKind::Recognize => 2,
    };
    Arc::clone(&kinds()[index])
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
