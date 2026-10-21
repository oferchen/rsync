//! Block-match token validation shared by every receive path.
//!
//! A block-match token names a basis block by index. The index arrives from
//! the peer, so it is checked against the `sum_head` count before any basis
//! byte is addressed, on the write paths and the discard path alike.
//!
//! # Upstream Reference
//!
//! - `receiver.c:584-593` - `receive_data()` bounds the index by `sum.count`
//!   and derives the block's offset and length from the same head.
use std::io;

use signature::SignatureLayout;

/// The basis span a block-match token names, as `(offset, length)`.
///
/// `layout` is the block geometry of the file's `sum_head`, `None` for a
/// whole-file transfer, which describes no blocks. The index is checked
/// before the caller consults its basis map, so a match token against a
/// whole-file head is refused here with upstream's `Invalid block index`
/// rather than as a missing basis.
///
/// upstream: `receiver.c:584-593`
///
/// ```text
/// if (i < 0 || i >= sum.count) { ...Invalid block index... }
/// offset2 = i * (OFF_T)sum.blength;
/// len = sum.blength;
/// if (i == (int)sum.count-1 && sum.remainder != 0)
///         len = sum.remainder;
/// ```
pub(crate) fn block_span(
    layout: Option<SignatureLayout>,
    index: usize,
) -> io::Result<(u64, usize)> {
    match layout {
        Some(layout) if (index as u64) < layout.block_count() => {
            let block_len = layout.block_length().get();
            let is_last = index as u64 == layout.block_count() - 1;
            let len = if is_last && layout.remainder() != 0 {
                layout.remainder()
            } else {
                block_len
            };
            Ok((index as u64 * u64::from(block_len), len as usize))
        }
        _ => Err(invalid_block_index(
            index,
            layout.map_or(0, SignatureLayout::block_count),
        )),
    }
}

/// Refuses a block-match index the `sum_head` does not describe.
///
/// The discard path maps no basis but still reads the token, so it calls this
/// directly with the echoed head's count.
///
/// upstream: `receiver.c:585-589` - `rprintf(FERROR, "Invalid block index %d
/// (count=%ld) [%s]\n", ...); exit_cleanup(RERR_PROTOCOL);`
pub(crate) fn check_block_index(index: usize, count: u64) -> io::Result<()> {
    if (index as u64) < count {
        Ok(())
    } else {
        Err(invalid_block_index(index, count))
    }
}

/// The `RERR_PROTOCOL`-tagged refusal, in upstream's wording.
fn invalid_block_index(index: usize, count: u64) -> io::Error {
    protocol::protocol_violation(format!(
        "Invalid block index {index} (count={count}) [receiver]"
    ))
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU8, NonZeroU32};

    use protocol::ProtocolViolation;

    use super::*;

    /// Three 700-byte blocks and a 200-byte tail: the shape upstream's
    /// `sum_sizes_sqroot()` gives a 2300-byte file.
    fn layout() -> SignatureLayout {
        SignatureLayout::from_raw_parts(
            NonZeroU32::new(700).unwrap(),
            200,
            4,
            NonZeroU8::new(16).unwrap(),
        )
    }

    fn assert_refused(result: io::Result<(u64, usize)>, index: usize, count: u64) {
        let err = result.expect_err("an index past the head must be refused");
        assert_eq!(
            err.to_string(),
            format!("Invalid block index {index} (count={count}) [receiver]")
        );
        assert!(
            err.get_ref()
                .and_then(|inner| inner.downcast_ref::<ProtocolViolation>())
                .is_some(),
            "must map to RERR_PROTOCOL (2), not a per-file or stream error"
        );
    }

    #[test]
    fn every_in_range_index_maps_to_its_basis_span() {
        let layout = Some(layout());
        assert_eq!(block_span(layout, 0).unwrap(), (0, 700));
        assert_eq!(block_span(layout, 2).unwrap(), (1400, 700));
        assert_eq!(
            block_span(layout, 3).unwrap(),
            (2100, 200),
            "the tail block"
        );
    }

    #[test]
    fn the_first_index_past_the_head_is_refused_with_upstreams_text() {
        assert_refused(block_span(Some(layout()), 4), 4, 4);
        assert_refused(block_span(Some(layout()), usize::MAX), usize::MAX, 4);
    }

    /// A whole-file head describes no blocks, so even index 0 is out of range.
    /// upstream checks the index before the basis map, so this - not the
    /// no-basis diagnostic - is what a match token against it produces.
    #[test]
    fn a_whole_file_head_refuses_every_index() {
        assert_refused(block_span(None, 0), 0, 0);
    }

    #[test]
    fn the_discard_check_uses_the_echoed_count() {
        assert!(check_block_index(3, 4).is_ok());
        let err = check_block_index(4, 4).expect_err("past the echoed count");
        assert_eq!(
            err.to_string(),
            "Invalid block index 4 (count=4) [receiver]"
        );
    }

    /// An exact multiple of the block length has no tail: the last block is
    /// full-sized.
    #[test]
    fn a_zero_remainder_leaves_the_last_block_full() {
        let layout = SignatureLayout::from_raw_parts(
            NonZeroU32::new(512).unwrap(),
            0,
            2,
            NonZeroU8::new(16).unwrap(),
        );
        assert_eq!(block_span(Some(layout), 1).unwrap(), (512, 512));
    }
}
