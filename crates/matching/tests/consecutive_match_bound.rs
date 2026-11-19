//! Why `OC_CONSECUTIVE_MATCH` may halve the strong sum: with the successor
//! rule the expected number of falsely trusted blocks per file never exceeds
//! upstream's.
//!
//! Upstream's model (`generator.c:733` `sum_sizes_sqroot`, which picks
//! `s2length` from it): every source offset is tried against every basis block,
//! `len * nb` trials, and a trial is falsely accepted when the 32-bit rolling
//! sum and the `s2`-byte strong sum both collide, so
//! `E_up = len * nb * 2^-(32+8*s2)`.
//!
//! The consecutive-match receiver sends `h = effective_s2length(s2)` bytes, so
//! one trial collides with `p = 2^-(32+8*h)`. The sender trusts a block only
//! when the next window matches its successor (zsync `next_match`). A trusted
//! false run is then either two chance collisions, the second on one fixed
//! block (`len * nb * p^2`), or a genuine run edge followed by one chance
//! collision on the successor, at most two edges per genuine run and at most
//! `nb` runs (`2 * nb * p`). If the next window could match ANY block the
//! first term would gain a factor `nb` and the second `nb` too.
//!
//! The cells below use the real layout code and the real `effective_s2length`,
//! so a change to either that breaks the bound fails here.

use protocol::{CompatibilityFlags, ProtocolVersion, effective_s2length};
use signature::{SignatureLayoutParams, calculate_signature_layout};
use std::num::{NonZeroU8, NonZeroU32};

/// `log2(2^a + 2^b)` without overflowing the exponent range.
fn log2_sum(a: f64, b: f64) -> f64 {
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    hi + (1.0 + (lo - hi).exp2()).log2()
}

struct Cell {
    len: u64,
    block_len: u32,
    s2: u8,
    h: u8,
    /// `log2(E_successor) - log2(E_upstream)`.
    successor_margin: f64,
    /// `log2(E_any_block) - log2(E_upstream)`.
    any_block_margin: f64,
}

/// Every file length from 2^10 to 2^44 in 1/8-power steps, against the block
/// length upstream derives and every forced power of two from 64 to 128 KiB,
/// for 16-byte and 8-byte negotiated digests. Layouts upstream refuses (too
/// many blocks) and single-block files (which never gate) are skipped.
fn cells() -> Vec<Cell> {
    let flags = CompatibilityFlags::CONSECUTIVE_MATCH;
    let mut out = Vec::new();
    for digest_len in [16u8, 8] {
        for eighth in 80..=352u32 {
            let len = (f64::from(eighth) / 8.0).exp2() as u64;
            let forced = std::iter::once(None).chain((6..=17).map(|k| NonZeroU32::new(1u32 << k)));
            for block in forced {
                let params = SignatureLayoutParams::new(
                    len,
                    block,
                    ProtocolVersion::NEWEST,
                    NonZeroU8::new(2).unwrap(),
                )
                .with_transfer_digest_length(NonZeroU8::new(digest_len).unwrap());
                let Ok(layout) = calculate_signature_layout(params) else {
                    continue;
                };
                let nb = layout.block_count();
                if nb < 2 {
                    continue;
                }
                let s2 = layout.strong_sum_length().get();
                let h = effective_s2length(flags, s2);
                let (l, n) = ((len as f64).log2(), (nb as f64).log2());
                let up = l + n - f64::from(32 + 8 * u32::from(s2));
                let ph = f64::from(32 + 8 * u32::from(h));
                let successor = log2_sum(l + n - 2.0 * ph, 1.0 + n - ph);
                let any_block = log2_sum(l + 2.0 * n - 2.0 * ph, 1.0 + 2.0 * n - ph);
                out.push(Cell {
                    len,
                    block_len: layout.block_length().get(),
                    s2,
                    h,
                    successor_margin: successor - up,
                    any_block_margin: any_block - up,
                });
            }
        }
    }
    out
}

#[test]
fn successor_rule_never_exceeds_upstreams_false_match_bound() {
    let cells = cells();
    assert!(cells.len() > 5_000, "only {} cells checked", cells.len());
    assert!(
        cells.iter().any(|c| c.h < c.s2),
        "no cell halved the strong sum, so nothing was tested"
    );
    let worst = cells
        .iter()
        .max_by(|a, b| a.successor_margin.total_cmp(&b.successor_margin))
        .unwrap();
    assert!(
        worst.successor_margin <= 0.0,
        "successor rule exceeds upstream by 2^{:.2} at len={} block={} s2={} h={}",
        worst.successor_margin,
        worst.len,
        worst.block_len,
        worst.s2,
        worst.h
    );
}

/// The rule the scan used before: a run could continue on any matching
/// block. This cell also shows the comparison above can fail.
#[test]
fn any_block_rule_exceeds_upstreams_bound_by_far() {
    let cells = cells();
    let worst = cells
        .iter()
        .max_by(|a, b| a.any_block_margin.total_cmp(&b.any_block_margin))
        .unwrap();
    assert!(
        worst.any_block_margin > 20.0,
        "any-block rule worst margin only 2^{:.2}",
        worst.any_block_margin
    );
}
