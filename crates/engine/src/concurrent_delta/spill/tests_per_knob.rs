//! Isolated unit tests for each currently-wired [`SpillPolicy`] knob.
//!
//! Each test constructs a [`SpillPolicy`] via
//! `SpillPolicy { <field>: <value>, ..Default::default() }`, builds a minimal
//! [`SpillableReorderBuffer`] from that policy using the same construction
//! logic the consumer pipeline uses (see `consumer::build_spillable`), and
//! asserts the observable behaviour implied by the knob.
//!
//! Only knobs whose runtime behaviour is wired today are covered:
//!
//! - [`SpillPolicy::threshold_bytes`] (STN-2) - drives the spill trigger.
//! - [`SpillPolicy::dir`] (STN-3) - drives the on-disk scratch location.
//!
//! The remaining knobs ([`reclaim_mode`], [`granularity`], [`compression`])
//! are placeholders for STN-4..7 and have no observable runtime effect yet,
//! so no tests are emitted for them here.
//!
//! [`reclaim_mode`]: super::policy::SpillPolicy::reclaim_mode
//! [`granularity`]: super::policy::SpillPolicy::granularity
//! [`compression`]: super::policy::SpillPolicy::compression

use std::fs;
use std::io::{self, Read, Write};

use super::policy::SpillPolicy;
use super::{SpillCodec, SpillError, SpillableReorderBuffer};

/// Local test item with a tunable encoded size so threshold maths line up
/// without depending on the shared `SpillCodec for u64` impl defined in
/// `spill::tests`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SizedItem {
    value: u64,
    size: usize,
}

impl SizedItem {
    fn new(value: u64, size: usize) -> Self {
        assert!(size >= 8, "SizedItem must hold at least the u64 payload");
        Self { value, size }
    }
}

impl SpillCodec for SizedItem {
    fn encode(&self, w: &mut dyn Write) -> io::Result<()> {
        w.write_all(&self.value.to_le_bytes())?;
        if self.size > 8 {
            w.write_all(&vec![0u8; self.size - 8])?;
        }
        Ok(())
    }

    fn decode(r: &mut dyn Read) -> io::Result<Self> {
        let mut buf = [0u8; 8];
        r.read_exact(&mut buf)?;
        Ok(Self {
            value: u64::from_le_bytes(buf),
            size: 8,
        })
    }

    fn estimated_size(&self) -> usize {
        self.size
    }
}

/// Mirrors `consumer::build_spillable` so each per-knob test exercises the
/// same `SpillPolicy` -> buffer mapping the runtime pipeline uses.
fn build_buffer_from_policy(
    capacity: usize,
    policy: &SpillPolicy,
) -> SpillableReorderBuffer<SizedItem> {
    let threshold = usize::try_from(
        policy
            .threshold_bytes
            .expect("per-knob tests always opt the spill layer in"),
    )
    .unwrap_or(usize::MAX);
    match policy.dir.clone() {
        Some(dir) => SpillableReorderBuffer::with_spill_dir(capacity, threshold, dir)
            .expect("spill directory must be creatable in tests"),
        None => SpillableReorderBuffer::new(capacity, threshold),
    }
}

fn drain_all(buf: &mut SpillableReorderBuffer<SizedItem>) -> Result<Vec<SizedItem>, SpillError> {
    buf.drain_ready()
}

// ---- threshold_bytes (STN-2, wired) ----

#[test]
fn threshold_bytes_does_not_spill_at_exact_threshold() {
    // Five 8-byte items fit exactly at the 40-byte threshold. The spill
    // trigger is strictly greater-than, so no spill must occur here.
    let policy = SpillPolicy {
        threshold_bytes: Some(40),
        ..Default::default()
    };
    let mut buf = build_buffer_from_policy(32, &policy);

    for i in 0..5u64 {
        buf.insert(i, SizedItem::new(i, 8)).unwrap();
    }

    let stats = buf.spill_stats();
    assert_eq!(stats.spill_events, 0, "exactly at threshold must not spill");
    assert_eq!(stats.memory_used, 40);
    assert_eq!(stats.threshold, 40);
}

#[test]
fn threshold_bytes_spills_when_exceeded() {
    // Crossing the 40-byte threshold by one item must trigger at least one
    // spill event and the buffer must still deliver items in order.
    let policy = SpillPolicy {
        threshold_bytes: Some(40),
        ..Default::default()
    };
    let mut buf = build_buffer_from_policy(32, &policy);

    for i in 0..5u64 {
        buf.insert(i, SizedItem::new(i, 8)).unwrap();
    }
    assert_eq!(buf.spill_stats().spill_events, 0);

    // Sixth insert pushes memory_used to 48, strictly above the threshold.
    buf.insert(5, SizedItem::new(5, 8)).unwrap();

    let stats = buf.spill_stats();
    assert!(
        stats.spill_events > 0,
        "crossing the threshold must trigger a spill, got {stats:?}"
    );
    assert_eq!(stats.threshold, 40);

    let items = drain_all(&mut buf).unwrap();
    let values: Vec<u64> = items.iter().map(|i| i.value).collect();
    assert_eq!(values, (0..6u64).collect::<Vec<_>>());
}

#[test]
fn threshold_bytes_zero_spills_on_first_insert() {
    // A zero-byte budget forces a spill on the very first insert that has
    // any non-zero footprint. Confirms the wiring respects extreme values.
    let policy = SpillPolicy {
        threshold_bytes: Some(0),
        ..Default::default()
    };
    let mut buf = build_buffer_from_policy(16, &policy);

    // Insert in reverse so the highest-sequence item is the spill candidate.
    buf.insert(1, SizedItem::new(11, 8)).unwrap();
    buf.insert(0, SizedItem::new(10, 8)).unwrap();

    let stats = buf.spill_stats();
    assert!(
        stats.spill_events > 0,
        "zero threshold must spill on the first over-budget insert"
    );

    let items = drain_all(&mut buf).unwrap();
    let values: Vec<u64> = items.iter().map(|i| i.value).collect();
    assert_eq!(values, vec![10, 11]);
}

// ---- dir (STN-3, wired) ----

#[test]
fn dir_override_routes_spill_through_supplied_directory() {
    // The directory backend opens an anonymous tempfile inside the
    // policy-supplied directory. The buffer must report that directory
    // and round-trip items through it. The directory itself must exist
    // after `with_spill_dir` runs (created eagerly even when the spill
    // backend later uses an unlinked anonymous file).
    let scratch = tempfile::tempdir().expect("scratch root");
    let spill_dir = scratch.path().join("policy-spill");

    let policy = SpillPolicy {
        threshold_bytes: Some(16),
        dir: Some(spill_dir.clone()),
        ..Default::default()
    };
    let mut buf = build_buffer_from_policy(16, &policy);

    assert_eq!(
        buf.spill_dir(),
        Some(spill_dir.as_path()),
        "buffer must observe the policy-supplied dir"
    );
    assert!(
        spill_dir.is_dir(),
        "with_spill_dir must create the override eagerly"
    );

    // Drive enough inserts to push memory_used past 16 bytes. Reverse order
    // makes the highest-sequence item the spill candidate.
    for i in (0..6u64).rev() {
        buf.insert(i, SizedItem::new(i, 8)).unwrap();
    }

    let stats = buf.spill_stats();
    assert!(stats.spill_events > 0, "spill must have engaged");

    let items = drain_all(&mut buf).unwrap();
    let values: Vec<u64> = items.iter().map(|i| i.value).collect();
    assert_eq!(
        values,
        (0..6u64).collect::<Vec<_>>(),
        "items must round-trip through the override dir"
    );
}

#[test]
fn dir_override_recreates_vanished_directory_before_first_spill() {
    // When the override dir disappears before any item has been spilled,
    // the buffer must recreate it once and continue. This is observable
    // via `spill_stats().dir_recreate_events`.
    let scratch = tempfile::tempdir().expect("scratch root");
    let spill_dir = scratch.path().join("vanish-spill");

    let policy = SpillPolicy {
        threshold_bytes: Some(8),
        dir: Some(spill_dir.clone()),
        ..Default::default()
    };
    let mut buf = build_buffer_from_policy(16, &policy);

    // Operator wipes the directory before the first spill.
    fs::remove_dir_all(&spill_dir).expect("remove spill dir");
    assert!(!spill_dir.exists());

    for i in (0..3u64).rev() {
        buf.insert(i, SizedItem::new(i * 100, 8)).unwrap();
    }

    let stats = buf.spill_stats();
    assert_eq!(
        stats.dir_recreate_events, 1,
        "exactly one recreate expected, got {stats:?}"
    );
    assert!(spill_dir.exists(), "policy-supplied dir must be back");
    assert!(stats.spill_events > 0, "spill must have engaged");
}

#[test]
fn dir_override_default_none_keeps_buffer_without_a_dir() {
    // Default `dir: None` selects the spooled backend, so the buffer must
    // not report a spill directory even with spilling fully engaged.
    let policy = SpillPolicy {
        threshold_bytes: Some(8),
        ..Default::default()
    };
    let mut buf = build_buffer_from_policy(16, &policy);

    assert!(
        buf.spill_dir().is_none(),
        "default policy must not pin a dir"
    );

    for i in (0..4u64).rev() {
        buf.insert(i, SizedItem::new(i, 8)).unwrap();
    }
    assert!(buf.spill_stats().spill_events > 0);
    assert!(
        buf.spill_dir().is_none(),
        "spooled backend never grows a directory"
    );
}
