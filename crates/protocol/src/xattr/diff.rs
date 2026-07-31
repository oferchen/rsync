//! Extended-attribute set comparison for itemized change reporting.
//!
//! Ports upstream rsync's `xattrs.c:xattr_diff()` so the generator can flag an
//! up-to-date file whose extended attributes differ from the sender's
//! (`ITEM_REPORT_XATTR`, the `x` column of `--itemize-changes`).

use std::cmp::Ordering;

use crate::xattr::XattrList;
use crate::xattr::wire::checksum_matches;

/// Returns `true` when the sender's extended-attribute list differs from the
/// receiver's on-disk attributes.
///
/// Mirrors upstream `xattrs.c:xattr_diff()`. `sender` is the flist xattr list as
/// received: values longer than [`MAX_FULL_DATUM`](crate::xattr::MAX_FULL_DATUM)
/// carry a checksum rather than the full datum (the abbreviation protocol).
/// `receiver` holds the destination's current attributes with full values, as
/// produced by `metadata::read_xattrs_for_wire`. Both lists must be sorted by
/// name (the receiver sorts on read; the sender sorts before transmit).
///
/// The comparison walks the two sorted lists in lockstep. A differing entry
/// count means the sets differ. For a shared name, an abbreviated sender value
/// is compared by checksum against the receiver's full datum, while every value
/// the sender carries in full is compared byte-for-byte - upstream's split at
/// `MAX_FULL_DATUM` (`xattrs.c:584-594`), keyed on the sender entry's own
/// abbreviation state rather than on its length.
///
/// Upstream can key that split on the length alone because `rsync_xal_get()`
/// abbreviates on *both* sides (`xattrs.c:274-284`), so a long value is always
/// checksum-vs-checksum there. oc keeps full data on the receiver side and
/// abbreviates only on the wire, so a long value reaches this function
/// abbreviated when it arrived over the wire and in full when both lists were
/// read from local disk (the local-copy generator). Testing
/// [`XattrEntry::is_abbreviated`](crate::xattr::XattrEntry::is_abbreviated)
/// covers both: it is exactly `datum_len > MAX_FULL_DATUM` for a wire-received
/// list, and it routes a locally-read long value to the byte-for-byte branch
/// instead of hashing its plaintext against the peer's datum, which could never
/// match.
///
/// The `find_all` bookkeeping upstream uses to mark entries for on-demand
/// request does not affect the returned "do they differ" answer, so this stops
/// at the first mismatch.
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
            if s.is_abbreviated() {
                // upstream: xattrs.c:584-587 - an abbreviated value compares by
                // checksum.
                s.datum_len() == r.datum_len()
                    && checksum_matches(s.datum(), r.datum(), checksum_seed)
            } else {
                // upstream: xattrs.c:591-594 - a value carried in full compares
                // byte-for-byte.
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

/// Marks every abbreviated sender entry the receiver cannot satisfy locally as
/// [`XattrState::Todo`](crate::xattr::XattrState::Todo), returning whether any
/// were marked.
///
/// This is the `find_all` arm of upstream `xattrs.c:xattr_diff()`
/// (`xattrs.c:547-616`). The generator calls `xattr_diff(file, sxp, 1)` before
/// itemizing (`generator.c:569`,`generator.c:575`); for every abbreviated value
/// (`datum_len > MAX_FULL_DATUM`) whose digest does not match the receiver's
/// on-disk copy it flips `snd_rxa->datum[0]` from `XSTATE_ABBREV` to
/// `XSTATE_TODO` (`xattrs.c:589-590`). `send_xattr_request()` then transmits
/// exactly those `num`s to the sender, which replies with the full values.
///
/// `sender` is the flist xattr list as received - large values carry only a
/// digest. `receiver` holds the basis (`fnamecmp`) file's current attributes
/// with full values (read via `metadata::read_xattrs_for_wire`); pass an empty
/// list when there is no basis, which lands every abbreviated entry in TODO
/// exactly as upstream's `rec_cnt == 0` path does for a brand-new file
/// (`generator.c:575` calls `xattr_diff(file, NULL, 1)`).
///
/// Both lists must be sorted by name. Only abbreviated entries are ever marked:
/// a value carried in full arrived complete in the flist and needs no request,
/// mirroring upstream which only reaches the `datum[0]` assignment inside the
/// `snd_rxa->datum_len > MAX_FULL_DATUM` branch.
#[must_use]
pub fn mark_xattr_requests(
    sender: &mut XattrList,
    receiver: &XattrList,
    checksum_seed: i32,
) -> bool {
    let rec = receiver.entries().to_vec();
    let mut ri = 0usize;
    let mut any = false;

    for s in sender.entries_mut() {
        // upstream: xattrs.c:581 strcmp(snd_rxa->name, rec_rxa->name). Walk the
        // sorted receiver forward past every name that sorts before this sender
        // name; what remains either matches or the receiver lacks the name.
        while ri < rec.len() && rec[ri].name() < s.name() {
            ri += 1;
        }
        let matched = ri < rec.len() && rec[ri].name() == s.name();

        // upstream: xattrs.c:584 - the abbreviation split keys on the sender
        // entry carrying only a digest, not on length alone (oc keeps a locally
        // read long value in full, which must not be hashed here).
        if s.is_abbreviated() {
            // upstream: xattrs.c:585-587 - an abbreviated value is "same" only
            // when the names match, the datum lengths agree, and the receiver's
            // locally computed digest equals the sender's `datum + 1`.
            let same = matched
                && s.datum_len() == rec[ri].datum_len()
                && checksum_matches(s.datum(), rec[ri].datum(), checksum_seed);
            // upstream: xattrs.c:589-590 - `if (!same && find_all && datum[0]
            // == XSTATE_ABBREV) datum[0] = XSTATE_TODO`.
            if !same {
                s.mark_todo();
                any = true;
            }
        }

        if matched {
            ri += 1;
        }
    }

    any
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xattr::wire::compute_xattr_checksum;
    use crate::xattr::{MAX_FULL_DATUM, XattrEntry, XattrList, XattrState};

    fn list(entries: Vec<XattrEntry>) -> XattrList {
        XattrList::with_entries(entries)
    }

    fn full(name: &str, value: &[u8]) -> XattrEntry {
        XattrEntry::new(name.as_bytes().to_vec(), value.to_vec())
    }

    fn abbrev(name: &str, value: &[u8]) -> XattrEntry {
        let checksum = compute_xattr_checksum(value, 0).to_vec();
        XattrEntry::abbreviated(name.as_bytes().to_vec(), checksum, value.len())
    }

    // upstream: xattrs.c:575 - generator.c:575 calls xattr_diff(file, NULL, 1)
    // for a brand-new file, so rec_cnt == 0 and every abbreviated sender entry
    // is not-same and lands in XSTATE_TODO. Without this the receiver never
    // requests the value and a large xattr on a new file is silently dropped.
    #[test]
    fn no_basis_marks_every_abbreviated_entry_todo() {
        let big = vec![7u8; MAX_FULL_DATUM + 40];
        let mut sender = list(vec![abbrev("user.big", &big), full("user.small", b"x")]);
        let any = mark_xattr_requests(&mut sender, &list(vec![]), 0);
        assert!(any);
        assert_eq!(sender.entries()[0].state(), XattrState::Todo);
        // A short value ships in full in the flist, so it is never requested.
        assert_eq!(sender.entries()[1].state(), XattrState::Done);
    }

    // upstream: xattrs.c:585-590 - an abbreviated value whose digest matches the
    // basis stays XSTATE_ABBREV (resolved locally from fnamecmp), never TODO.
    #[test]
    fn matching_basis_leaves_abbreviated_entry_unrequested() {
        let big = vec![7u8; MAX_FULL_DATUM + 40];
        let mut sender = list(vec![abbrev("user.big", &big)]);
        let receiver = list(vec![full("user.big", &big)]);
        let any = mark_xattr_requests(&mut sender, &receiver, 0);
        assert!(!any);
        assert_eq!(sender.entries()[0].state(), XattrState::Abbrev);
    }

    // upstream: xattrs.c:585-590 - a digest mismatch against the basis marks the
    // entry TODO so the sender is asked for the current value.
    #[test]
    fn mismatched_basis_marks_abbreviated_entry_todo() {
        let sender_val = vec![7u8; MAX_FULL_DATUM + 40];
        let basis_val = vec![9u8; MAX_FULL_DATUM + 40];
        let mut sender = list(vec![abbrev("user.big", &sender_val)]);
        let receiver = list(vec![full("user.big", &basis_val)]);
        let any = mark_xattr_requests(&mut sender, &receiver, 0);
        assert!(any);
        assert_eq!(sender.entries()[0].state(), XattrState::Todo);
    }

    // upstream: xattrs.c:581 - a name the basis lacks (cmp > 0 / rec exhausted)
    // is not-same, so an abbreviated value with no basis counterpart is TODO
    // while an unrelated basis name is skipped without disturbing the walk.
    #[test]
    fn partial_basis_marks_only_the_missing_abbreviated_entry() {
        let a = vec![1u8; MAX_FULL_DATUM + 8];
        let b = vec![2u8; MAX_FULL_DATUM + 8];
        let mut sender = list(vec![abbrev("user.a", &a), abbrev("user.b", &b)]);
        // Basis holds only user.a (identical); user.b is absent.
        let receiver = list(vec![full("user.a", &a)]);
        let any = mark_xattr_requests(&mut sender, &receiver, 0);
        assert!(any);
        assert_eq!(sender.entries()[0].state(), XattrState::Abbrev);
        assert_eq!(sender.entries()[1].state(), XattrState::Todo);
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

    /// The local-copy generator reads both sides from disk, so a value over
    /// `MAX_FULL_DATUM` arrives here in full on the sender side. Keying the
    /// checksum branch off the length alone would hash-compare the plaintext
    /// against the destination's identical plaintext and always report a
    /// difference, lighting the itemize `x` column on every large xattr.
    #[test]
    fn identical_unabbreviated_large_values_do_not_differ() {
        let big = vec![7u8; MAX_FULL_DATUM + 40];
        let sender = list(vec![full("user.big", &big)]);
        let receiver = list(vec![full("user.big", &big)]);
        assert!(!xattr_diff(&sender, &receiver, 0));
    }

    /// The byte-for-byte branch must still catch a real difference in a large
    /// value that both sides carry in full.
    #[test]
    fn differing_unabbreviated_large_values_differ() {
        let sender = list(vec![full("user.big", &[7u8; MAX_FULL_DATUM + 40])]);
        let receiver = list(vec![full("user.big", &[9u8; MAX_FULL_DATUM + 40])]);
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
