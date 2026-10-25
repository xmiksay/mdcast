//! The global render concurrency limit (issue #96). Every render holds one
//! slot from just after its cheap validation (so a `400` or a `409` round
//! trip never occupies one) until its response is built; blob check/upload,
//! `/health` and `/v1/capabilities` never take one. Sized so that N
//! concurrent compiles × `MDCAST_TYPST_MAX_MEMORY_BYTES`, plus the blob
//! cache, fits the pod — without it, a few heavy templates at once take the
//! whole service down instead of just waiting their turn.
//!
//! The queue in front of the slots is bounded too (`MDCAST_RENDER_QUEUE_MAX`):
//! a queued request already holds its whole decoded body, so an unbounded
//! queue would only move the out-of-memory from the compile to the wait.
//! A request that finds the queue full is answered `503` at once.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::error::ApiError;

pub struct RenderGate {
    slots: Arc<Semaphore>,
    queue: Duration,
    waiting: AtomicUsize,
    max_waiting: usize,
}

impl RenderGate {
    pub fn new(slots: usize, queue: Duration, max_waiting: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(slots)),
            queue,
            waiting: AtomicUsize::new(0),
            max_waiting,
        }
    }

    /// A slot, waiting up to the queue time for one if the queue has room.
    /// Dropping the permit frees the slot.
    pub async fn enter(&self) -> Result<OwnedSemaphorePermit, ApiError> {
        if let Ok(permit) = self.slots.clone().try_acquire_owned() {
            return Ok(permit);
        }
        if self.queue.is_zero() {
            return Err(self.overloaded());
        }
        let Some(_place) = QueuePlace::take(&self.waiting, self.max_waiting) else {
            return Err(self.overloaded());
        };
        tokio::time::timeout(self.queue, self.slots.clone().acquire_owned())
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or_else(|| self.overloaded())
    }

    fn overloaded(&self) -> ApiError {
        ApiError::Overloaded {
            retry_after_secs: self.retry_after_secs(),
        }
    }

    /// One full queue wait (at least a second): a request that just waited
    /// that long for nothing gains little from coming back any sooner.
    fn retry_after_secs(&self) -> u64 {
        self.queue.as_secs().max(1)
    }
}

/// One of the `max` places in the queue, given back on drop — including when
/// the waiting request is cancelled by its client disconnecting.
struct QueuePlace<'a>(&'a AtomicUsize);

impl<'a> QueuePlace<'a> {
    fn take(waiting: &'a AtomicUsize, max: usize) -> Option<Self> {
        // Optimistic increment, undone when over: two racing requests can
        // both be refused, never both admitted past `max`.
        if waiting.fetch_add(1, Ordering::AcqRel) >= max {
            waiting.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self(waiting))
    }
}

impl Drop for QueuePlace<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_free_slot_is_taken_and_given_back_on_drop() {
        let gate = RenderGate::new(1, Duration::ZERO, 2);

        let permit = gate.enter().await.ok().unwrap();
        assert!(gate.enter().await.is_err(), "the only slot is held");
        drop(permit);
        assert!(gate.enter().await.is_ok(), "dropping the permit frees it");
    }

    #[tokio::test]
    async fn a_queued_request_gets_a_slot_freed_while_it_waits() {
        let gate = Arc::new(RenderGate::new(1, Duration::from_secs(10), 2));
        let held = gate.enter().await.ok().unwrap();

        let waiter = tokio::spawn({
            let gate = gate.clone();
            async move { gate.enter().await.is_ok() }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(held);

        assert!(waiter.await.unwrap());
        assert_eq!(gate.waiting.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn a_full_gate_answers_overloaded_after_the_queue_time() {
        let gate = RenderGate::new(1, Duration::from_secs(1), 2);
        let _held = gate.enter().await.ok().unwrap();
        let start = std::time::Instant::now();

        match gate.enter().await {
            Err(ApiError::Overloaded { retry_after_secs }) => assert_eq!(retry_after_secs, 1),
            _ => panic!("expected Overloaded"),
        }
        assert!(start.elapsed() >= Duration::from_secs(1), "it waited first");
    }

    #[tokio::test]
    async fn a_full_queue_answers_overloaded_at_once_and_frees_places_on_cancel() {
        let gate = Arc::new(RenderGate::new(1, Duration::from_secs(30), 1));
        let _held = gate.enter().await.ok().unwrap();
        let queued = tokio::spawn({
            let gate = gate.clone();
            async move { gate.enter().await.is_ok() }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let start = std::time::Instant::now();
        assert!(gate.enter().await.is_err(), "the only queue place is taken");
        assert!(start.elapsed() < Duration::from_secs(1), "no waiting first");

        queued.abort();
        let _ = queued.await;
        assert_eq!(gate.waiting.load(Ordering::Acquire), 0, "cancel frees it");
    }

    #[tokio::test]
    async fn a_zero_length_queue_never_waits() {
        let gate = RenderGate::new(1, Duration::from_secs(30), 0);
        let _held = gate.enter().await.ok().unwrap();
        let start = std::time::Instant::now();
        assert!(gate.enter().await.is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn retry_after_is_never_zero() {
        assert_eq!(RenderGate::new(1, Duration::ZERO, 0).retry_after_secs(), 1);
    }
}
