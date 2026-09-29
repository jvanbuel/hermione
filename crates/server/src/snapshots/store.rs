//! The latest snapshot per student, kept in memory.
//!
//! Two things live here that used to share one entry: the snapshots themselves,
//! and when each student's editor was last asked for one. They have different
//! lifetimes and different questions asked of them, and sharing an entry meant
//! some entries held a snapshot slot with nothing in it — a placeholder that
//! then aged and pruned as if it were a snapshot. Kept apart, every entry in
//! `latest` is a real snapshot.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::model::Snapshot;
use crate::student::Slot;

/// A snapshot older than this is dropped: a stale buffer is worse than none,
/// and it caps how long a student's text can sit in the server's memory.
const TTL: Duration = Duration::from_secs(180);

/// Don't ask a student's editor more often than this, however many teachers
/// are watching them.
const ASK_INTERVAL: Duration = Duration::from_millis(750);

/// Entries kept per process before expired ones are swept out.
const PRUNE_THRESHOLD: usize = 256;

/// A snapshot with what a reader needs to judge it.
pub struct Latest {
    pub snapshot: Arc<Snapshot>,
    /// How long ago the server received it — by the server's own clock, so a
    /// student's laptop clock being wrong can't make it look fresh or stale.
    pub age: Duration,
    /// Identifies this exact snapshot: different for every one received. Lets a
    /// reader tell "the same one again" from "a new one" without comparing them.
    pub rev: u64,
}

#[derive(Clone, Default)]
pub struct SnapshotStore {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    latest: HashMap<Slot, Stored>,
    asked: HashMap<Slot, Instant>,
    next_rev: u64,
}

struct Stored {
    snapshot: Arc<Snapshot>,
    received: Instant,
    rev: u64,
}

impl SnapshotStore {
    /// Keeps a snapshot as its student's latest, replacing the last one.
    pub fn insert(&self, slot: Slot, snapshot: Snapshot) {
        let mut inner = self.lock();
        if inner.latest.len() > PRUNE_THRESHOLD {
            inner.latest.retain(|_, s| s.received.elapsed() < TTL);
        }
        inner.next_rev += 1;
        let rev = inner.next_rev;
        inner.latest.insert(
            slot,
            Stored {
                snapshot: Arc::new(snapshot),
                received: Instant::now(),
                rev,
            },
        );
    }

    /// The student's latest snapshot, if one arrived recently enough to trust.
    pub fn latest(&self, slot: &Slot) -> Option<Latest> {
        let inner = self.lock();
        let stored = inner.latest.get(slot)?;
        let age = stored.received.elapsed();
        (age < TTL).then(|| Latest {
            snapshot: Arc::clone(&stored.snapshot),
            age,
            rev: stored.rev,
        })
    }

    /// Whether it is time to ask this student's editor for a fresh snapshot.
    /// Saying yes counts as asking: the next call within the interval says no.
    pub fn claim_ask(&self, slot: &Slot) -> bool {
        let mut inner = self.lock();
        if inner.asked.len() > PRUNE_THRESHOLD {
            inner.asked.retain(|_, at| at.elapsed() < TTL);
        }
        let due = inner
            .asked
            .get(slot)
            .is_none_or(|at| at.elapsed() >= ASK_INTERVAL);
        if due {
            inner.asked.insert(slot.clone(), Instant::now());
        }
        due
    }

    // Nothing here awaits or can leave the maps half-updated, so a poisoned lock
    // still guards consistent data.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::student::Student;
    use uuid::Uuid;

    fn slot(name: &str) -> Slot {
        Slot {
            course: Uuid::nil(),
            student: Student::try_from(name.to_string()).unwrap(),
        }
    }

    #[test]
    fn a_stored_snapshot_comes_back_fresh() {
        let store = SnapshotStore::default();
        assert!(store.latest(&slot("alice")).is_none());
        store.insert(slot("alice"), Snapshot::Empty);
        let latest = store.latest(&slot("alice")).unwrap();
        assert!(latest.age < Duration::from_secs(5));
        assert!(matches!(*latest.snapshot, Snapshot::Empty));
        assert!(store.latest(&slot("bob")).is_none(), "per student");
    }

    #[test]
    fn every_snapshot_gets_its_own_rev() {
        let store = SnapshotStore::default();
        store.insert(slot("alice"), Snapshot::Empty);
        let first = store.latest(&slot("alice")).unwrap().rev;
        assert_eq!(
            store.latest(&slot("alice")).unwrap().rev,
            first,
            "same one again"
        );
        store.insert(slot("alice"), Snapshot::Empty);
        assert_ne!(store.latest(&slot("alice")).unwrap().rev, first);
    }

    #[test]
    fn expired_snapshots_are_not_served() {
        let store = SnapshotStore::default();
        store.insert(slot("alice"), Snapshot::Empty);
        // Age the entry past the TTL without sleeping for it.
        let old = Instant::now() - TTL - Duration::from_secs(1);
        store
            .lock()
            .latest
            .get_mut(&slot("alice"))
            .unwrap()
            .received = old;
        assert!(store.latest(&slot("alice")).is_none());
    }

    #[test]
    fn asking_is_throttled_per_student() {
        let store = SnapshotStore::default();
        assert!(store.claim_ask(&slot("alice")));
        assert!(!store.claim_ask(&slot("alice")));
        assert!(
            store.claim_ask(&slot("bob")),
            "another student is independent"
        );
    }

    #[test]
    fn asking_and_storing_do_not_interfere() {
        let store = SnapshotStore::default();
        assert!(store.claim_ask(&slot("alice")));
        // Asking leaves no placeholder behind to be mistaken for a snapshot.
        assert!(store.latest(&slot("alice")).is_none());
        store.insert(slot("alice"), Snapshot::Empty);
        assert!(
            !store.claim_ask(&slot("alice")),
            "a new snapshot does not reset the throttle"
        );
    }

    #[test]
    fn the_same_name_in_another_course_is_another_student() {
        let store = SnapshotStore::default();
        store.insert(slot("alice"), Snapshot::Empty);
        let elsewhere = Slot {
            course: Uuid::new_v4(),
            ..slot("alice")
        };
        assert!(store.latest(&elsewhere).is_none());
    }
}
