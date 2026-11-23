use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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

/// Consumer-scoped async request slots.
///
/// Each UI consumer owns an independent monotonic epoch slot so concurrent
/// requests from sibling views (e.g. `LogFileDiff` vs `FileHistoryDiff` vs
/// `StashFileDiff`) never cancel each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum RequestSlotId {
    RepoSnapshot = 0,
    StageStatus = 1,
    LogCommitDetails = 2,
    StashCommitDetails = 3,
    CommitSignature = 4,
    LogFileDiff = 5,
    StashFileDiff = 6,
    FileHistoryDiff = 7,
    StageFileDiff = 8,
    FileHistoryList = 9,
    PathFilter = 10,
    AmendPreview = 11,
    ConflictDocument = 12,
    Annotate = 13,
    AnnotateDiff = 14,
    RemoteTagCheck = 15,
    PushOutgoing = 16,
    PushCommitDetails = 17,
    PushFileDiff = 18,
    PatchCheck = 19,
    TreeBrowser = 20,
    TreeBlob = 21,
    LogSignatures = 22,
}

impl RequestSlotId {
    pub const COUNT: usize = 23;

    pub const ALL: [Self; Self::COUNT] = [
        Self::RepoSnapshot,
        Self::StageStatus,
        Self::LogCommitDetails,
        Self::StashCommitDetails,
        Self::CommitSignature,
        Self::LogFileDiff,
        Self::StashFileDiff,
        Self::FileHistoryDiff,
        Self::StageFileDiff,
        Self::FileHistoryList,
        Self::PathFilter,
        Self::AmendPreview,
        Self::ConflictDocument,
        Self::Annotate,
        Self::AnnotateDiff,
        Self::RemoteTagCheck,
        Self::PushOutgoing,
        Self::PushCommitDetails,
        Self::PushFileDiff,
        Self::PatchCheck,
        Self::TreeBrowser,
        Self::TreeBlob,
        Self::LogSignatures,
    ];

    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Lock-free per-slot epoch table shared between an engine handle and its worker tasks.
#[derive(Debug, Clone)]
pub struct SlotRegistry {
    slots: Arc<[RequestEpoch; RequestSlotId::COUNT]>,
}

impl Default for SlotRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SlotRegistry {
    pub fn new() -> Self {
        Self {
            slots: Arc::new(std::array::from_fn(|_| RequestEpoch::new())),
        }
    }

    pub fn epoch_for(&self, slot: RequestSlotId) -> &RequestEpoch {
        &self.slots[slot.index()]
    }

    pub fn next_epoch(&self, slot: RequestSlotId) -> u64 {
        self.epoch_for(slot).next_epoch()
    }

    pub fn issue(&self, slot: RequestSlotId) -> EpochToken {
        self.epoch_for(slot).issue()
    }

    pub fn token_for(&self, slot: RequestSlotId, epoch: u64) -> EpochToken {
        EpochToken::new(self.epoch_for(slot).clone(), epoch)
    }
}

/// Unified single-flight slot controller for both local cancellable jobs
/// (syntax highlighting, containing-branch BFS, visible-graph rebuild) and
/// remote engine requests.
#[derive(Debug)]
pub struct RequestSlot<K = ()> {
    latest_epoch: u64,
    in_flight: Option<K>,
    cancel: Option<Arc<AtomicBool>>,
    epochs: RequestEpoch,
}

impl<K> Default for RequestSlot<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K> RequestSlot<K> {
    pub fn new() -> Self {
        Self {
            latest_epoch: 0,
            in_flight: None,
            cancel: None,
            epochs: RequestEpoch::new(),
        }
    }

    pub fn latest_epoch(&self) -> u64 {
        self.latest_epoch
    }

    pub fn in_flight(&self) -> Option<&K> {
        self.in_flight.as_ref()
    }

    pub fn is_busy(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Cancels any previous in-flight work, advances the local epoch counter,
    /// and clears the in-flight key.
    pub fn invalidate(&mut self) {
        if let Some(prev) = self.cancel.take() {
            prev.store(true, Ordering::Relaxed);
        }
        self.latest_epoch = self.epochs.next_epoch();
        self.in_flight = None;
    }

    /// Records the epoch returned by an external engine backend for this slot.
    pub fn note_dispatched(&mut self, epoch: u64) {
        self.latest_epoch = self.latest_epoch.max(epoch);
    }

    /// Accepts an external engine response if `epoch` is not older than the
    /// latest dispatched/accepted epoch for this slot.
    pub fn accept_epoch(&mut self, epoch: u64) -> bool {
        if epoch < self.latest_epoch {
            return false;
        }
        self.latest_epoch = epoch;
        true
    }

    /// Returns `true` if `epoch` matches the currently accepted epoch for this slot.
    pub fn matches_latest(&self, epoch: u64) -> bool {
        epoch == self.latest_epoch
    }

    /// Clears only the in-flight key without altering the latest epoch.
    pub fn clear_in_flight(&mut self) {
        self.in_flight = None;
    }
}

impl<K: PartialEq> RequestSlot<K> {
    pub fn is_in_flight(&self, key: &K) -> bool {
        self.in_flight.as_ref() == Some(key)
    }

    pub fn clear_if_matches(&mut self, key: &K) {
        if self.in_flight.as_ref() == Some(key) {
            self.in_flight = None;
        }
    }

    /// Starts a local cooperative-cancellation job backed by an `AtomicBool` flag.
    /// Returns `None` if an identical key is already in flight.
    pub fn begin_cancelable(&mut self, key: K) -> Option<(u64, Arc<AtomicBool>)> {
        if self.in_flight.as_ref() == Some(&key) {
            return None;
        }
        self.invalidate();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        self.in_flight = Some(key);
        Some((self.latest_epoch, cancel))
    }

    /// Completes a local cancelable job, returning `true` when the result is still current.
    pub fn finish_cancelable(&mut self, key: &K, epoch: u64) -> bool {
        if self.in_flight.as_ref() == Some(key) && epoch == self.latest_epoch {
            self.in_flight = None;
        }
        epoch >= self.latest_epoch
    }

    /// Starts a local cooperative-cancellation job backed by an [`EpochToken`].
    /// Returns `None` if an identical key is already in flight.
    pub fn begin_token(&mut self, key: K) -> Option<EpochToken> {
        if self.in_flight.as_ref() == Some(&key) {
            return None;
        }
        if let Some(prev) = self.cancel.take() {
            prev.store(true, Ordering::Relaxed);
        }
        let token = self.epochs.issue();
        self.latest_epoch = token.epoch();
        self.in_flight = Some(key);
        Some(token)
    }

    /// Completes a local [`EpochToken`]-backed job, returning `true` if `token` is still current.
    pub fn finish_token(&mut self, key: &K, token: &EpochToken) -> bool {
        if token.is_stale() {
            return false;
        }
        if self.in_flight.as_ref() == Some(key) {
            self.in_flight = None;
        }
        true
    }
}

/// Consumer-side table of latest request epochs indexed by [`RequestSlotId`].
#[derive(Debug, Default)]
pub struct RequestSlotTable {
    latest_epochs: [u64; RequestSlotId::COUNT],
}

impl RequestSlotTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn note_dispatched(&mut self, id: RequestSlotId, epoch: u64) {
        let slot = &mut self.latest_epochs[id.index()];
        *slot = (*slot).max(epoch);
    }

    pub fn accept(&mut self, id: RequestSlotId, epoch: u64) -> bool {
        let slot = &mut self.latest_epochs[id.index()];
        if epoch < *slot {
            return false;
        }
        *slot = epoch;
        true
    }

    pub fn matches_latest(&self, id: RequestSlotId, epoch: u64) -> bool {
        self.latest_epochs[id.index()] == epoch
    }

    pub fn latest(&self, id: RequestSlotId) -> u64 {
        self.latest_epochs[id.index()]
    }
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

    #[test]
    fn slot_registry_isolates_sibling_consumers() {
        let registry = SlotRegistry::new();
        let log_tok = registry.issue(RequestSlotId::LogFileDiff);
        let fh_tok = registry.issue(RequestSlotId::FileHistoryDiff);
        let stash_tok = registry.issue(RequestSlotId::StashFileDiff);

        assert!(!log_tok.is_stale());
        assert!(!fh_tok.is_stale());
        assert!(!stash_tok.is_stale());

        let log_tok_2 = registry.issue(RequestSlotId::LogFileDiff);
        assert!(log_tok.is_stale());
        assert!(!log_tok_2.is_stale());
        assert!(!fh_tok.is_stale());
        assert!(!stash_tok.is_stale());
    }

    #[test]
    fn request_slot_cancelable_and_token_lifecycle() {
        let mut slot = RequestSlot::<&str>::new();
        let (e1, c1) = slot.begin_cancelable("a").expect("first begin");
        assert!(slot.begin_cancelable("a").is_none());
        let (e2, c2) = slot.begin_cancelable("b").expect("superseding begin");
        assert!(c1.load(Ordering::Relaxed));
        assert!(!c2.load(Ordering::Relaxed));
        assert!(!slot.finish_cancelable(&"a", e1));
        assert!(slot.finish_cancelable(&"b", e2));
        assert!(!slot.is_busy());

        let mut tok_slot = RequestSlot::<u32>::new();
        let t1 = tok_slot.begin_token(1).unwrap();
        let t2 = tok_slot.begin_token(2).unwrap();
        assert!(t1.is_stale());
        assert!(!tok_slot.finish_token(&1, &t1));
        assert!(tok_slot.finish_token(&2, &t2));
        assert!(!tok_slot.is_busy());
    }
}
