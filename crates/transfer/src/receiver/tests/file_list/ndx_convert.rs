//! INC_RECURSE NDX conversion: counter-bumping invariants and
//! `wire_to_flat_ndx` / `flat_to_wire_ndx` round-trip coverage across a
//! multi-segment table.

use std::path::PathBuf;

use protocol::flist::FileEntry;

use super::super::super::ReceiverContext;
use super::super::support::{test_config, test_handshake};

#[test]
fn receiver_ndx_convert_call_counter_increments() {
    // INC_RECURSE diagnostic I4 (#2199): every flat_to_wire_ndx invocation
    // must bump the global call counter. The assertion uses >= because the
    // counter is shared across the process and other tests may run
    // concurrently.
    use super::super::super::ndx_convert_totals;

    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new(&handshake, config);

    let (calls_before, _) = ndx_convert_totals();

    let _ = ctx.flat_to_wire_ndx(0);
    let _ = ctx.flat_to_wire_ndx(0);
    let _ = ctx.flat_to_wire_ndx(0);

    let (calls_after, _) = ndx_convert_totals();
    assert!(
        calls_after >= calls_before + 3,
        "expected at least 3 new ndx_convert calls (before={calls_before}, after={calls_after})"
    );
}

#[test]
fn receiver_ndx_convert_partition_point_depth_grows() {
    // INC_RECURSE diagnostic I4 (#2199): the cumulative partition_point depth
    // must monotonically grow as the segment table is queried. A 4-segment
    // table contributes at least depth(4)=3 per call. Uses >= because the
    // counter is shared across the process.
    use super::super::super::{ndx_convert_totals, partition_point_depth};

    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new(&handshake, config);
    // Default ndx_segments has one entry; extend it to four.
    ctx.ndx_segments.push((10, 11));
    ctx.ndx_segments.push((20, 22));
    ctx.ndx_segments.push((30, 33));

    let per_call_depth = partition_point_depth(ctx.ndx_segments.len());
    assert!(
        per_call_depth >= 3,
        "expected partition_point_depth(4) >= 3, got {per_call_depth}"
    );

    const N: u64 = 8;
    let (_, cmps_before) = ndx_convert_totals();
    for _ in 0..N {
        let _ = ctx.flat_to_wire_ndx(0);
    }
    let (_, cmps_after) = ndx_convert_totals();

    assert!(
        cmps_after >= cmps_before + N * per_call_depth,
        "cumulative partition_point depth should grow by at least {} \
         (before={cmps_before}, after={cmps_after})",
        N * per_call_depth
    );
}

/// Verifies that [`super::super::super::ReceiverContext::wire_to_flat_ndx`]
/// is the inverse of
/// [`super::super::super::ReceiverContext::flat_to_wire_ndx`] across a
/// multi-segment table built up by INC_RECURSE.
#[test]
fn wire_to_flat_ndx_round_trips_with_flat_to_wire() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Simulate INC_RECURSE: initial segment (0, 1) plus two extras.
    ctx.ndx_segments = vec![(0, 1), (5, 7), (12, 15)];
    ctx.file_list = (0..18)
        .map(|i| FileEntry::new_file(PathBuf::from(format!("f{i}")), 0, 0o644))
        .collect();

    for flat in 0..18usize {
        let wire = ctx.flat_to_wire_ndx(flat);
        assert_eq!(
            ctx.wire_to_flat_ndx(wire),
            Some(flat),
            "round-trip failed at flat={flat} wire={wire}"
        );
    }

    // Out-of-range wire NDXes (the reserved 0 under INC_RECURSE and any
    // value above the last segment's max) must return None.
    assert_eq!(ctx.wire_to_flat_ndx(0), None);
    assert_eq!(ctx.wire_to_flat_ndx(i32::MAX), None);
}
