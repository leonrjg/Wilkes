//! Worker faults, as errors a caller can recognise without reading a message.
//!
//! Every loop that submits to a worker has to tell two failures apart. One
//! item failed to be read, or embedded, or labelled — the next item is worth
//! attempting. Or the worker is not going to serve the next item either, in
//! which case attempting it spawns a replacement, reloads the model, and hands
//! the user a cancel that takes one more iteration to outrun every time they
//! press it.
//!
//! Only the second is a [`WorkerFault`], in two kinds. [`Fault::Gone`] is the
//! process having ended — killed by a cancel, or its pipes broken.
//! [`Fault::Reported`] is the worker answering the request with a failure: it
//! is alive, but it could not do the thing, and for a whole-batch call like
//! embedding that is the batch's whole outcome rather than one item's.
//!
//! Both are asked about by type. This used to be asked by matching the first
//! characters of a message, which made every rewording of a log line a chance
//! to silently turn a fatal error into a skipped batch.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The process ended under the request.
    Gone,
    /// The process answered the request with a failure.
    Reported,
}

#[derive(Debug, Clone)]
pub struct WorkerFault {
    pub kind: Fault,
    pub detail: String,
}

impl fmt::Display for WorkerFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            Fault::Gone => write!(f, "the worker process is gone: {}", self.detail),
            Fault::Reported => write!(f, "the worker reported a failure: {}", self.detail),
        }
    }
}

impl std::error::Error for WorkerFault {}

impl WorkerFault {
    pub fn gone(detail: impl Into<String>) -> anyhow::Error {
        anyhow::Error::new(Self {
            kind: Fault::Gone,
            detail: detail.into(),
        })
    }

    pub fn reported(detail: impl Into<String>) -> anyhow::Error {
        anyhow::Error::new(Self {
            kind: Fault::Reported,
            detail: detail.into(),
        })
    }
}

/// The fault behind `error`, if it is one.
///
/// Asked through the chain rather than of the outermost error, because every
/// layer between the pipe and the loop adds its own context.
pub fn fault_of(error: &anyhow::Error) -> Option<Fault> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<WorkerFault>())
        .map(|fault| fault.kind)
}

/// Whether the worker process behind `error` ended.
///
/// The question a loop asks: a gone worker means the next submit *starts* one,
/// so the loop must end instead.
pub fn is_worker_gone(error: &anyhow::Error) -> bool {
    fault_of(error) == Some(Fault::Gone)
}

/// Whether `error` came from the worker at all, either kind.
pub fn is_worker_fault(error: &anyhow::Error) -> bool {
    fault_of(error).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn a_dead_worker_is_recognised_through_the_context_stacked_on_it() {
        let error = Err::<(), _>(WorkerFault::gone("closed stdout"))
            .context("could not reach the recognizer")
            .context("reading page 4")
            .unwrap_err();
        assert!(is_worker_gone(&error));
        assert!(is_worker_fault(&error));
    }

    /// The two kinds are both faults, but only one of them means the next
    /// submit would spawn a process.
    #[test]
    fn a_reported_failure_is_a_fault_but_not_a_gone_worker() {
        let error = Err::<(), _>(WorkerFault::reported("could not load the model"))
            .context("embedding a batch")
            .unwrap_err();
        assert!(is_worker_fault(&error));
        assert!(!is_worker_gone(&error));
    }

    #[test]
    fn an_ordinary_failure_is_neither() {
        let error = anyhow::anyhow!("that page would not rasterize");
        assert!(!is_worker_fault(&error));
        assert!(!is_worker_gone(&error));
    }
}
