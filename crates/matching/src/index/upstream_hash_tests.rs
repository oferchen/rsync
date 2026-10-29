use super::*;
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::num::NonZeroU8;

/// An index over `data`, whose identical blocks all share one weak sum and
/// so one `hash_table` chain.
fn index_over(data: &[u8]) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature =
        generate_file_signature(data, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

fn probe_bytes(table: &mut UpstreamHashTable, index: &DeltaSignatureIndex, window: &[u8]) {
    table.probe_window(index, RollingDigest::from_bytes(window), 0, window, &[]);
}

/// upstream: match.c:86-90 - `tablesize = (uint32)(s->count/8) * 10 + 11`,
/// floored at 2^16.
#[test]
fn table_size_follows_build_hash_table() {
    assert_eq!(upstream_table_size(0), TRADITIONAL_TABLESIZE);
    assert_eq!(upstream_table_size(52_421), TRADITIONAL_TABLESIZE);
    assert_eq!(upstream_table_size(52_424), 65_541);
    assert_eq!(upstream_table_size(800_000), 1_000_011);
}

/// upstream: match.c:73-76 - `SUM2HASH2` adds the two 16-bit halves for the
/// traditional table; `BIG_SUM2HASH` takes the full sum modulo the size.
#[test]
fn bucket_follows_sum2hash() {
    let sum = 0x1234_ff10;
    assert_eq!(upstream_bucket(sum, TRADITIONAL_TABLESIZE), 0x1144);
    assert_eq!(upstream_bucket(sum, 65_541), (sum % 65_541) as usize);
}

/// A window whose bucket is empty is not a hash hit (match.c:216-218 jumps
/// to `null_hash` before `hash_hits++`).
#[test]
fn an_empty_bucket_is_not_a_hash_hit() {
    let index = index_over(&[b'a'; 4096]);
    let mut table = UpstreamHashTable::new(&index, false);
    let foreign = vec![b'z'; index.block_length()];
    let basis_bucket = upstream_bucket(index.block(0).rolling().value(), table.tablesize);
    let foreign_bucket =
        upstream_bucket(RollingDigest::from_bytes(&foreign).value(), table.tablesize);
    assert_ne!(
        basis_bucket, foreign_bucket,
        "the fixture needs distinct buckets"
    );
    probe_bytes(&mut table, &index, &foreign);
    assert_eq!(table.counters(), ProbeCounters::default());
}

/// A matching window is one hash hit and no false alarm: the walk stops at
/// the first strong match (match.c:271-345).
#[test]
fn a_strong_match_is_one_hit_without_false_alarms() {
    let index = index_over(&[b'a'; 4096]);
    let mut table = UpstreamHashTable::new(&index, false);
    probe_bytes(&mut table, &index, &vec![b'a'; index.block_length()]);
    assert_eq!(
        table.counters(),
        ProbeCounters {
            hash_hits: 1,
            false_alarms: 0
        }
    );
}

/// A window with the basis's weak sum but other content is a false alarm
/// for every chain candidate it is compared against (match.c:268-271). The
/// short final block has another weak sum, so it is never compared.
/// Raising bytes 0 and 3 and lowering 1 and 2 by one keeps both halves of
/// the rolling sum while changing the strong sum.
#[test]
fn a_weak_collision_is_a_false_alarm_per_candidate() {
    let index = index_over(&[b'a'; 4096]);
    let mut table = UpstreamHashTable::new(&index, false);
    let mut window = vec![b'a'; index.block_length()];
    window[0] += 1;
    window[3] += 1;
    window[1] -= 1;
    window[2] -= 1;
    assert_eq!(
        RollingDigest::from_bytes(&window).value(),
        index.block(0).rolling().value(),
        "the fixture must collide on the weak sum"
    );
    probe_bytes(&mut table, &index, &window);
    assert_eq!(
        table.counters(),
        ProbeCounters {
            hash_hits: 1,
            false_alarms: index.full_block_count() as u64
        }
    );
}

/// The walk stops after `MAX_CHAIN_LEN` same-sum candidates, so no further
/// false alarms are counted (match.c:250).
#[test]
fn the_chain_walk_is_capped() {
    let index = index_over(&vec![b'a'; 4 << 20]);
    assert!(
        index.full_block_count() > MAX_CHAIN_LEN as usize,
        "the fixture needs a long chain"
    );
    let mut table = UpstreamHashTable::new(&index, false);
    let mut window = vec![b'a'; index.block_length()];
    window[0] += 1;
    window[3] += 1;
    window[1] -= 1;
    window[2] -= 1;
    probe_bytes(&mut table, &index, &window);
    assert_eq!(table.counters().false_alarms, u64::from(MAX_CHAIN_LEN));
}

/// Under `--inplace` a block behind the cursor that never matched at its
/// own offset leaves the chain for good (match.c:233-237).
#[test]
fn inplace_unlinks_blocks_behind_the_cursor() {
    let index = index_over(&[b'a'; 4096]);
    let block_len = index.block_length() as u64;
    let mut table = UpstreamHashTable::new(&index, true);
    let window = vec![b'a'; index.block_length()];
    let past_all_blocks = block_len * index.block_count() as u64;
    table.probe_window(
        &index,
        RollingDigest::from_bytes(&window),
        past_all_blocks,
        &window,
        &[],
    );
    table.probe_window(&index, RollingDigest::from_bytes(&window), 0, &window, &[]);
    assert_eq!(
        table.counters().hash_hits,
        1,
        "the second probe finds an emptied bucket"
    );
}
