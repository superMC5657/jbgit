use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonically increasing epoch counter used for single-flight async cancellation
/// (inspired by JetBrains `BaseSingleTaskController`).
#[derive(Debug, Clone, Default)]
pub struct RequestEpoch {
    latest: Arc<AtomicU64>,
}

impl RequestEpoch {
    pub fn new() -> Self {
        Self {
            latest: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Advances the epoch and returns the newly issued token.
    pub fn next_epoch(&self) -> u64 {
        self.latest.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Advances the epoch and returns a bundled [`EpochToken`].
    pub fn issue(&self) -> EpochToken {
        EpochToken {
            tracker: self.clone(),
            epoch: self.next_epoch(),
        }
    }

    /// Returns the most recently issued epoch.
    pub fn current(&self) -> u64 {
        self.latest.load(Ordering::Acquire)
    }

    /// Returns `true` if `epoch` has been superseded by a newer request.
    pub fn is_stale(&self, epoch: u64) -> bool {
        self.latest.load(Ordering::Acquire) != epoch
    }
}

/// Paired request epoch tracker and issued epoch number for cooperative cancellation.
#[derive(Debug, Clone)]
pub struct EpochToken {
    tracker: RequestEpoch,
    epoch: u64,
}

impl EpochToken {
    pub fn new(tracker: RequestEpoch, epoch: u64) -> Self {
        Self { tracker, epoch }
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn is_stale(&self) -> bool {
        self.tracker.is_stale(self.epoch)
    }
}

#[derive(Debug, Clone, Default)]
pub struct EpochTracker {
    pub repo_snapshot: RequestEpoch,
    pub commit_details: RequestEpoch,
    pub file_diff: RequestEpoch,
    pub path_filter: RequestEpoch,
    pub stage_status: RequestEpoch,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supersedes_previous_epochs() {
        let tracker = RequestEpoch::new();
        let e1 = tracker.next_epoch();
        assert!(!tracker.is_stale(e1));
        let e2 = tracker.next_epoch();
        assert!(tracker.is_stale(e1));
        assert!(!tracker.is_stale(e2));
    }
}
