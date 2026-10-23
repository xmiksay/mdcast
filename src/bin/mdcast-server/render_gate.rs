//! The global render concurrency limit (issue #96). Every render holds one
//! slot from just after its cheap validation (so a `400` or a `409` round
//! trip never occupies one) until its response is built; blob check/upload,
//! `/health` and `/v1/capabilities` never take one. Sized so that N
//! concurrent compiles × `MDCAST_TYPST_MAX_MEMORY_BYTES`, plus the blob
//! cache, fits the pod — without it, a few heavy templates at once take the
//! whole service down instead of just waiting their turn.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::error::ApiError;

pub struct RenderGate {
    slots: Arc<Semaphore>,
    queue: Duration,
}

impl RenderGate {
    pub fn new(slots: usize, queue: Duration) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(slots)),
            queue,
        }
    }

    /// A slot, waiting up to the queue time for one. Dropping the permit
    /// frees the slot.
    pub async fn enter(&self) -> Result<OwnedSemaphorePermit, ApiError> {
        let permit = if self.queue.is_zero() {
            self.slots.clone().try_acquire_owned().ok()
        } else {
            tokio::time::timeout(self.queue, self.slots.clone().acquire_owned())
                .await
                .ok()
                .and_then(Result::ok)
        };
        permit.ok_or(ApiError::Overloaded {
            retry_after_secs: self.retry_after_secs(),
        })
    }

    /// One full queue wait (at least a second): a request that just waited
    /// that long for nothing gains little from coming back any sooner.
    fn retry_after_secs(&self) -> u64 {
        self.queue.as_secs().max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_free_slot_is_taken_and_given_back_on_drop() {
        let gate = RenderGate::new(1, Duration::ZERO);

        let permit = gate.enter().await.ok().unwrap();
        assert!(gate.enter().await.is_err(), "the only slot is held");
        drop(permit);
        assert!(gate.enter().await.is_ok(), "dropping the permit frees it");
    }

    #[tokio::test]
    async fn a_queued_request_gets_a_slot_freed_while_it_waits() {
        let gate = Arc::new(RenderGate::new(1, Duration::from_secs(10)));
        let held = gate.enter().await.ok().unwrap();

        let waiter = tokio::spawn({
            let gate = gate.clone();
            async move { gate.enter().await.is_ok() }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(held);

        assert!(waiter.await.unwrap());
    }

    #[tokio::test]
    async fn a_full_gate_answers_overloaded_after_the_queue_time() {
        let gate = RenderGate::new(1, Duration::from_secs(1));
        let _held = gate.enter().await.ok().unwrap();
        let start = std::time::Instant::now();

        match gate.enter().await {
            Err(ApiError::Overloaded { retry_after_secs }) => assert_eq!(retry_after_secs, 1),
            _ => panic!("expected Overloaded"),
        }
        assert!(start.elapsed() >= Duration::from_secs(1), "it waited first");
    }

    #[test]
    fn retry_after_is_never_zero() {
        assert_eq!(RenderGate::new(1, Duration::ZERO).retry_after_secs(), 1);
    }
}
