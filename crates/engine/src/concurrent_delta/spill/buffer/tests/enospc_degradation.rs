//! SPL-33.c assertion suite: drive the spill path through the SPL-33.b
//! [`MockEnoSpcWriter`] harness and prove it degrades gracefully under every
//! ENOSPC scenario the SPL-33.a design enumerates.
//!
//! Each tier mirrors a row in the SPL-33.a scenario matrix
//! (`docs/design/spl-33a-enospc-injection-mechanism.md`):
//!
//! - Tier 1: spill of 1 MiB hits ENOSPC at byte 500 KiB - typed error,
//!   no panic, no data loss.
//! - Tier 2: multiple sequential spills, second hits ENOSPC - the first
//!   spill's bytes are durable, the second surfaces `Err`.
//! - Tier 3: per-chunk write hits ENOSPC mid-chunk - buffer length matches
//!   actually-written bytes (atomic-fail contract).
//! - Tier 4: post-ENOSPC writes return `Err` immediately on a persistent
//!   plan - no spin-wait, no silent retry.
//! - Tier 5 (Linux only): real-kernel ENOSPC via [`with_full_tmpfs`], skip-
//!   gated via `CAP_SYS_ADMIN`.
//!
//! A property test (`property_enospc_threshold_always_surfaces_storage_full`)
//! varies the byte threshold across 100 random samples and asserts the
//! surfaced error variant stays [`SpillError::Io`] with kind
//! [`ErrorKind::StorageFull`].

use std::io::{ErrorKind, Write};
use std::panic;

use proptest::prelude::*;

use super::super::super::{SpillError, SpillableReorderBuffer};
use super::FailingCodec;
use super::fault::{ENOSPC_KIND, FaultEvent, FaultPlan, MockEnoSpcWriter};

/// Sentinel size used by the 1 MiB Tier 1 scenarios; matches the SPL-33.a
/// design's example boundary (spill of 1 MB, ENOSPC at 500 KB).
const ONE_MIB: usize = 1024 * 1024;
const HALF_MIB: u64 = (ONE_MIB / 2) as u64;

/// Convenience: assert the error is `SpillError::Io(StorageFull)` and panic
/// with the actual variant otherwise. Used by every tier so the failure
/// diagnostic is uniform across the suite.
fn assert_storage_full(err: &SpillError) {
    match err {
        SpillError::Io(e) => assert_eq!(
            e.kind(),
            ErrorKind::StorageFull,
            "expected StorageFull, got {:?}",
            e.kind()
        ),
        other => panic!("expected SpillError::Io(StorageFull), got {other:?}"),
    }
    assert!(
        err.is_out_of_space(),
        "is_out_of_space() must return true for the injected ENOSPC"
    );
}

// Tier 1: 1 MiB spill hits ENOSPC at byte 500 KiB.

#[test]
fn tier1_one_mib_spill_hits_enospc_at_500k_no_panic_no_data_loss() {
    // Drive a 1 MiB write through the harness in 64 KiB chunks. The first
    // eight chunks (512 KiB) succeed, the ninth crosses the 500 KiB
    // threshold and surfaces ENOSPC atomically.
    let backing = Vec::<u8>::with_capacity(ONE_MIB);
    let mut writer = MockEnoSpcWriter::new(backing, HALF_MIB);
    let chunk = vec![0xAB_u8; 64 * 1024];

    let mut successful_chunks = 0usize;
    let mut trip_seen = false;
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        for _ in 0..(ONE_MIB / chunk.len()) {
            match writer.write(&chunk) {
                Ok(n) => {
                    assert_eq!(n, chunk.len(), "chunk must be accepted whole");
                    successful_chunks += 1;
                }
                Err(e) => {
                    assert_eq!(e.kind(), ENOSPC_KIND);
                    trip_seen = true;
                    break;
                }
            }
        }
    }));

    assert!(
        result.is_ok(),
        "the spill path must never panic under ENOSPC"
    );
    assert!(trip_seen, "ENOSPC must surface within the 1 MiB write");
    assert!(writer.has_tripped(), "chassis counter must mark the trip");
    assert_eq!(
        successful_chunks * chunk.len(),
        writer.bytes_written() as usize,
        "chassis counter must equal the bytes actually written below the threshold"
    );
    let bytes_written = writer.bytes_written();
    assert!(
        bytes_written <= HALF_MIB,
        "no bytes past the 500 KiB threshold may reach the inner writer; got {bytes_written}"
    );

    let backing = writer.into_inner();
    assert_eq!(
        backing.len(),
        bytes_written as usize,
        "inner buffer length must match the chassis byte counter"
    );
    assert!(
        backing.iter().all(|&b| b == 0xAB),
        "no committed byte may differ from the data we wrote (no data loss)"
    );
}

#[test]
fn tier1_spillable_buffer_surfaces_storage_full_during_spill() {
    // Wire the SPL-33.b ENOSPC contract through the production spill path:
    // a `FailingCodec` whose `encode` returns `StorageFull` is the
    // equivalent injection point at the encoder boundary. The buffer must
    // surface `SpillError::Io(StorageFull)` without panicking.
    let mut buf: SpillableReorderBuffer<FailingCodec> = SpillableReorderBuffer::new(8, 16);

    let healthy_a = FailingCodec {
        value: 1,
        size: 8,
        fail_kind: None,
    };
    let healthy_b = FailingCodec {
        value: 2,
        size: 16,
        fail_kind: None,
    };
    let poisoned = FailingCodec {
        value: 99,
        size: 64,
        fail_kind: Some(ErrorKind::StorageFull),
    };

    buf.insert(0, healthy_a)
        .expect("seed insert below threshold");
    buf.insert(1, healthy_b)
        .expect("seed insert near threshold");

    let pre_failure_count = buf.buffered_count();

    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| buf.insert(2, poisoned)));
    let outcome = result.expect("spill path must not panic under ENOSPC");
    let err = outcome.expect_err("ENOSPC injection must surface as Err");
    assert_storage_full(&err);

    // Buffer length must be consistent: the failed item is re-inserted via
    // `force_insert`, so the count includes it but no item is lost.
    let post_failure_count = buf.buffered_count();
    assert!(
        post_failure_count >= pre_failure_count,
        "buffered_count must be consistent post-failure (no data loss)"
    );
}

// Tier 2: sequential spills, second hits ENOSPC.

#[test]
fn tier2_first_spill_survives_second_spill_hits_enospc() {
    // Threshold = 200 bytes. The first 200 bytes survive; the second write
    // that crosses the line must fail and leave the surviving bytes intact.
    let backing: Vec<u8> = Vec::new();
    let mut writer = MockEnoSpcWriter::new(backing, 200);

    let first = [0x11_u8; 200];
    let n = writer
        .write(&first)
        .expect("first spill must fully commit below threshold");
    assert_eq!(n, 200);
    assert!(
        !writer.has_tripped(),
        "no trip on the surviving first spill"
    );

    let second = [0x22_u8; 200];
    let err = writer
        .write(&second)
        .expect_err("second spill crossing the threshold must fail");
    assert_eq!(err.kind(), ENOSPC_KIND);
    assert!(writer.has_tripped(), "chassis counter records the trip");
    assert_eq!(
        writer.bytes_written(),
        200,
        "only the first spill's bytes ever reached the inner writer"
    );

    let backing = writer.into_inner();
    assert_eq!(backing.len(), 200, "first spill bytes are durable");
    assert!(
        backing.iter().all(|&b| b == 0x11),
        "first spill payload must survive byte-for-byte"
    );
}

#[test]
fn tier2_subsequent_inserts_after_failure_keep_buffer_consistent() {
    // Drive the SpillableReorderBuffer through a Tier 2 scenario: one
    // successful spill, then a failing one. The buffer must keep delivering
    // the surviving items in sequence order.
    let mut buf: SpillableReorderBuffer<FailingCodec> = SpillableReorderBuffer::new(16, 16);

    let ok = |v: u64| FailingCodec {
        value: v,
        size: 8,
        fail_kind: None,
    };
    let poison = FailingCodec {
        value: 999,
        size: 64,
        fail_kind: Some(ErrorKind::StorageFull),
    };

    for seq in 0..4 {
        buf.insert(seq, ok(seq))
            .expect("baseline insert must succeed");
    }

    // The fifth insert is the poisoned one. The buffer must surface
    // SpillError::Io(StorageFull) cleanly.
    let err = buf
        .insert(4, poison)
        .expect_err("poisoned insert must surface Err");
    assert_storage_full(&err);

    // All five items survive in the buffer: the spill path always
    // re-inserts taken items on encode failure via `restore_taken`, so the
    // failed insert leaves the buffer with the same item count as the
    // pre-failure state plus the newly inserted poisoned item.
    let mut delivered = Vec::new();
    while let Some(item) = buf.next_in_order().expect("delivery must not panic") {
        delivered.push(item.value);
    }
    assert_eq!(
        delivered.len(),
        5,
        "no data loss: every inserted item must drain in sequence order"
    );
    for (i, v) in delivered.iter().take(4).enumerate() {
        assert_eq!(*v, i as u64, "delivered values must match insertion order");
    }
    assert_eq!(
        delivered[4], 999,
        "the poisoned item itself is preserved post-failure"
    );
}

// Tier 3: per-chunk write hits ENOSPC mid-chunk; buffer state stays
// consistent (length equals actually-written bytes).

#[test]
fn tier3_mid_chunk_failure_keeps_buffer_length_consistent() {
    // Threshold = 100 bytes. A single 250-byte write would cross the
    // threshold and must fail atomically: no bytes reach the inner writer,
    // the chassis byte counter does not advance, and no panic fires.
    let backing: Vec<u8> = Vec::new();
    let mut writer = MockEnoSpcWriter::new(backing, 100);

    let payload = [0x55_u8; 250];
    let err = panic::catch_unwind(panic::AssertUnwindSafe(|| writer.write(&payload)))
        .expect("atomic-fail path must not panic")
        .expect_err("over-threshold single write must fail atomically");
    assert_eq!(err.kind(), ENOSPC_KIND);
    assert!(
        writer.has_tripped(),
        "chassis counter records the mid-chunk trip"
    );
    assert_eq!(
        writer.bytes_written(),
        0,
        "atomic-fail contract: zero bytes must reach the inner writer"
    );

    let backing = writer.into_inner();
    assert_eq!(
        backing.len(),
        0,
        "inner buffer length must match the chassis byte counter (zero) after atomic fail"
    );
}

#[test]
fn tier3_partial_progress_then_atomic_fail_on_threshold_cross() {
    // First write fits below the threshold, second write crosses it and
    // must fail atomically. Buffer state is consistent: length equals
    // first-write payload size, no second-write bytes leak in.
    let backing: Vec<u8> = Vec::new();
    let mut writer = MockEnoSpcWriter::new(backing, 64);

    writer
        .write_all(&[0x77; 32])
        .expect("first write fits below threshold");
    let err = writer
        .write_all(&[0x88; 64])
        .expect_err("second write crosses the threshold");
    assert_eq!(err.kind(), ENOSPC_KIND);
    assert_eq!(
        writer.bytes_written(),
        32,
        "only the first write's bytes are durable"
    );

    let backing = writer.into_inner();
    assert_eq!(backing.len(), 32);
    assert!(backing.iter().all(|&b| b == 0x77));
}

// Tier 4: post-ENOSPC writes return Err immediately on a persistent plan.

#[test]
fn tier4_persistent_plan_returns_err_immediately_post_trip() {
    // Default `FaultPlan::enospc` is persistent (`one_shot = false`). After
    // the first failure every subsequent write must fail immediately with
    // the same ENOSPC kind - no spinwait, no silent retry.
    let mut writer = MockEnoSpcWriter::new(Vec::<u8>::new(), 0);

    // First write trips the plan (threshold = 0 fires on any payload).
    let first = writer
        .write(&[0; 1])
        .expect_err("threshold=0 trips on first byte");
    assert_eq!(first.kind(), ENOSPC_KIND);

    // Hammer the writer 50 more times; every call must short-circuit to
    // ENOSPC without touching the inner writer.
    for attempt in 0..50 {
        let err = writer
            .write(&[0; 1])
            .expect_err("persistent plan must fail-fast on every attempt");
        assert_eq!(
            err.kind(),
            ENOSPC_KIND,
            "attempt {attempt} must surface ENOSPC"
        );
    }
    assert_eq!(
        writer.bytes_written(),
        0,
        "persistent plan must keep the inner writer untouched across every attempt"
    );
}

#[test]
fn tier4_one_shot_plan_recovers_but_persistent_plan_does_not() {
    // Contrast assertion: a one-shot plan does recover after the first
    // ENOSPC. This pins down the harness contract that Tier 4 relies on -
    // persistent vs one-shot is the only knob controlling recovery.
    let one_shot = FaultPlan {
        kind: ENOSPC_KIND,
        event: FaultEvent::DiskFull { after_bytes: 0 },
        one_shot: true,
    };
    let mut writer = MockEnoSpcWriter::with_plan(Vec::<u8>::new(), one_shot);
    writer
        .write(&[0; 1])
        .expect_err("one-shot trips on first byte");
    let n = writer
        .write(&[0xCD; 4])
        .expect("one-shot plan permits recovery on the next write");
    assert_eq!(n, 4);

    // The persistent plan must NOT recover. Use a fresh writer to prove it.
    let mut persistent = MockEnoSpcWriter::new(Vec::<u8>::new(), 0);
    persistent
        .write(&[0; 1])
        .expect_err("persistent plan trips");
    persistent
        .write(&[0; 1])
        .expect_err("persistent plan must keep failing");
}

// Tier 5 (Linux only): real-kernel ENOSPC via with_full_tmpfs.

#[cfg(target_os = "linux")]
#[test]
fn tier5_real_kernel_enospc_via_full_tmpfs() {
    use super::fault::{has_cap_sys_admin, with_full_tmpfs};

    // Skip-gate: the chassis returns None when CAP_SYS_ADMIN is missing.
    // Hosted CI runners almost always hit this path, so the test must
    // succeed by short-circuit.
    if !has_cap_sys_admin() {
        let outcome = with_full_tmpfs(4, |_| ());
        assert!(outcome.is_none(), "helper must skip without CAP_SYS_ADMIN");
        return;
    }

    // Privileged environment: mount a 4 MiB tmpfs pre-filled to within
    // ~4 KiB of full, then point a SpillableReorderBuffer at it and prove
    // the next spill surfaces SpillError::Io(StorageFull) cleanly.
    let outcome = with_full_tmpfs(4, |dir| {
        let spill_dir = dir.join("spill");
        let mut buf: SpillableReorderBuffer<FailingCodec> =
            SpillableReorderBuffer::with_spill_dir(64, 8 * 1024, &spill_dir)
                .expect("with_spill_dir on a near-full tmpfs must still create the dir");

        let big = FailingCodec {
            value: 0,
            size: 64 * 1024,
            fail_kind: None,
        };

        let mut surfaced: Option<SpillError> = None;
        for seq in 0..64 {
            match buf.insert(seq, big) {
                Ok(()) => continue,
                Err(e) => {
                    surfaced = Some(e);
                    break;
                }
            }
        }
        surfaced.expect("a near-full tmpfs must surface ENOSPC within 64 inserts")
    })
    .expect("CAP_SYS_ADMIN present, helper must execute body");

    assert_storage_full(&outcome);
}

// Property test: varying ENOSPC byte threshold across 100 random samples.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// For any byte threshold between 0 and 1 MiB, a write that exceeds it
    /// must surface ENOSPC and the surfaced kind must always be
    /// [`ErrorKind::StorageFull`]. This proves the harness mapping is
    /// invariant under the fault timing chosen by the test author - the
    /// SPL-33.a contract that `SpillError::is_out_of_space()` is true
    /// exactly when the kernel reports `StorageFull`.
    #[test]
    fn property_enospc_threshold_always_surfaces_storage_full(
        threshold in 0u64..=(ONE_MIB as u64),
    ) {
        let mut writer = MockEnoSpcWriter::new(Vec::<u8>::new(), threshold);
        // Force a write that always crosses the threshold by at least 1 byte.
        let payload = vec![0u8; (threshold as usize).saturating_add(1)];
        let err = writer.write(&payload).expect_err("over-threshold write must fail");
        prop_assert_eq!(err.kind(), ErrorKind::StorageFull);
        prop_assert!(writer.has_tripped(), "chassis counter must record the trip");
        prop_assert_eq!(
            writer.bytes_written(),
            0,
            "atomic-fail contract: no partial bytes for an over-threshold single write"
        );

        // Round-trip through SpillError to confirm the surfaced variant is
        // SpillError::Io(StorageFull) and is_out_of_space() returns true.
        let spill_err: SpillError = err.into();
        prop_assert!(
            matches!(&spill_err, SpillError::Io(e) if e.kind() == ErrorKind::StorageFull),
            "expected SpillError::Io(StorageFull), got {spill_err:?}"
        );
        prop_assert!(spill_err.is_out_of_space());
    }
}

// Chassis-counter assertions: prove the harness counters are consistent
// across the assertion suite, not just the harness self-tests.

#[test]
fn chassis_counter_matches_actually_written_bytes_across_many_writes() {
    let mut writer = MockEnoSpcWriter::new(Vec::<u8>::new(), 1_000);
    let mut expected = 0u64;
    for chunk_size in [1usize, 7, 32, 200, 256, 401] {
        let buf = vec![0u8; chunk_size];
        let n = writer.write(&buf).expect("fits below threshold");
        expected += n as u64;
        assert_eq!(writer.bytes_written(), expected);
    }
    // Final over-threshold write trips the plan; counter must stay put.
    let pre_trip = writer.bytes_written();
    let overflow = [0u8; 500];
    writer.write(&overflow).expect_err("threshold crossed");
    assert_eq!(
        writer.bytes_written(),
        pre_trip,
        "chassis counter must not advance on the failing write"
    );
}

#[test]
fn fault_event_count_advances_exactly_once_for_one_shot_plan() {
    // The one-shot plan must trip exactly once; downstream code may depend
    // on `has_tripped()` being a monotonic flag. We assert that:
    //   * the first failing write flips the flag,
    //   * subsequent successful writes do not clear it.
    let plan = FaultPlan {
        kind: ENOSPC_KIND,
        event: FaultEvent::DiskFull { after_bytes: 0 },
        one_shot: true,
    };
    let mut writer = MockEnoSpcWriter::with_plan(Vec::<u8>::new(), plan);
    assert!(!writer.has_tripped(), "no trip before first failing write");
    writer
        .write_all(&[0; 1])
        .expect_err("one-shot fires on first call");
    assert!(writer.has_tripped(), "trip flag set after first failure");
    writer
        .write_all(&[0xEE; 8])
        .expect("post-trip write succeeds under one-shot");
    assert!(
        writer.has_tripped(),
        "trip flag is monotonic - never cleared"
    );
}
