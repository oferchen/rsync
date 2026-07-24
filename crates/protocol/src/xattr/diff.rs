//! Extended-attribute set comparison for itemized change reporting.
//!
//! Ports upstream rsync's `xattrs.c:xattr_diff()` so the generator can flag an
//! up-to-date file whose extended attributes differ from the sender's
//! (`ITEM_REPORT_XATTR`, the `x` column of `--itemize-changes`).

use std::cmp::Ordering;

use crate::xattr::wire::checksum_matches;
use crate::xattr::{MAX_FULL_DATUM, XattrList};

/// Returns `true` when the sender's extended-attribute list differs from the
/// receiver's on-disk attributes.
///
/// Mirrors upstream `xattrs.c:xattr_diff()`. `sender` is the flist xattr list as
/// received: values longer than [`MAX_FULL_DATUM`] carry a checksum rather than
/// the full datum (the abbreviation protocol). `receiver` holds the
/// destination's current attributes with full values, as produced by
/// `metadata::read_xattrs_for_wire`. Both lists must be sorted by name (the
/// receiver sorts on read; the sender sorts before transmit).
///
/// The comparison walks the two sorted lists in lockstep. A differing entry
/// count means the sets differ. For a shared name, a value at or below
/// `MAX_FULL_DATUM` is compared byte-for-byte, while a larger value is compared
/// by checksum against the receiver's full datum - exactly upstream's split at
/// `MAX_FULL_DATUM` (`xattrs.c:584-594`). The `find_all` bookkeeping upstream
/// uses to mark entries for on-demand request does not affect the returned
/// "do they differ" answer, so this stops at the first mismatch.
#[must_use]
pub fn xattr_diff(sender: &XattrList, receiver: &XattrList, checksum_seed: i32) -> bool {
    let snd = sender.entries();
    let rec = receiver.entries();

    // upstream: xattrs.c:574-576 - a differing count means the lists differ.
    if snd.len() != rec.len() {
        return true;
    }

    let (mut si, mut ri) = (0usize, 0usize);
    while si < snd.len() {
        let s = &snd[si];
        // upstream: xattrs.c:581 - cmp < 0 means the sender has a name the
        // receiver lacks (rec exhausted counts as sender-smaller).
        let cmp = if ri < rec.len() {
            s.name().cmp(rec[ri].name())
        } else {
            Ordering::Less
        };

        let same = if cmp == Ordering::Equal {
            let r = &rec[ri];
            if s.datum_len() > MAX_FULL_DATUM {
                // upstream: xattrs.c:584-587 - large values compare by checksum.
                s.datum_len() == r.datum_len()
                    && checksum_matches(s.datum(), r.datum(), checksum_seed)
            } else {
                // upstream: xattrs.c:591-594 - small values compare byte-for-byte.
                s.datum_len() == r.datum_len() && s.datum() == r.datum()
            }
        } else {
            false
        };

        if !same {
            return true;
        }

        if cmp != Ordering::Greater {
            si += 1;
        }
        if cmp != Ordering::Less {
            ri += 1;
        }
    }

    // With equal counts and a full sender walk the receiver is also exhausted;
    // the check mirrors upstream's trailing `if (rec_cnt) xattrs_equal = 0`.
    ri < rec.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xattr::wire::compute_xattr_checksum;
    use crate::xattr::{XattrEntry, XattrList};

    fn list(entries: Vec<XattrEntry>) -> XattrList {
        XattrList::with_entries(entries)
    }

    fn full(name: &str, value: &[u8]) -> XattrEntry {
        XattrEntry::new(name.as_bytes().to_vec(), value.to_vec())
    }

    #[test]
    fn identical_small_values_do_not_differ() {
        let a = list(vec![full("user.a", b"x"), full("user.b", b"yy")]);
        let b = list(vec![full("user.a", b"x"), full("user.b", b"yy")]);
        assert!(!xattr_diff(&a, &b, 0));
    }

    #[test]
    fn differing_small_value_differs() {
        let a = list(vec![full("user.a", b"x")]);
        let b = list(vec![full("user.a", b"z")]);
        assert!(xattr_diff(&a, &b, 0));
    }

    #[test]
    fn differing_count_differs() {
        let a = list(vec![full("user.a", b"x"), full("user.b", b"y")]);
        let b = list(vec![full("user.a", b"x")]);
        assert!(xattr_diff(&a, &b, 0));
        assert!(xattr_diff(&b, &a, 0));
    }

    #[test]
    fn differing_name_same_count_differs() {
        let a = list(vec![full("user.a", b"x")]);
        let b = list(vec![full("user.c", b"x")]);
        assert!(xattr_diff(&a, &b, 0));
    }

    #[test]
    fn empty_lists_do_not_differ() {
        assert!(!xattr_diff(&list(vec![]), &list(vec![]), 0));
    }

    #[test]
    fn large_value_matching_checksum_does_not_differ() {
        // A value beyond MAX_FULL_DATUM: the sender carries only its checksum,
        // the receiver the full datum. Matching content must not report a diff.
        let big = vec![7u8; MAX_FULL_DATUM + 40];
        let checksum = compute_xattr_checksum(&big, 0).to_vec();
        let sender = list(vec![XattrEntry::abbreviated(
            b"user.big".to_vec(),
            checksum,
            big.len(),
        )]);
        let receiver = list(vec![full("user.big", &big)]);
        assert!(!xattr_diff(&sender, &receiver, 0));
    }

    #[test]
    fn large_value_differing_content_differs() {
        let sender_val = vec![7u8; MAX_FULL_DATUM + 40];
        let receiver_val = vec![9u8; MAX_FULL_DATUM + 40];
        let checksum = compute_xattr_checksum(&sender_val, 0).to_vec();
        let sender = list(vec![XattrEntry::abbreviated(
            b"user.big".to_vec(),
            checksum,
            sender_val.len(),
        )]);
        let receiver = list(vec![full("user.big", &receiver_val)]);
        assert!(xattr_diff(&sender, &receiver, 0));
    }

    #[test]
    fn large_value_differing_length_differs() {
        let sender_val = vec![7u8; MAX_FULL_DATUM + 40];
        let receiver_val = vec![7u8; MAX_FULL_DATUM + 41];
        let checksum = compute_xattr_checksum(&sender_val, 0).to_vec();
        let sender = list(vec![XattrEntry::abbreviated(
            b"user.big".to_vec(),
            checksum,
            sender_val.len(),
        )]);
        let receiver = list(vec![full("user.big", &receiver_val)]);
        assert!(xattr_diff(&sender, &receiver, 0));
    }
}
