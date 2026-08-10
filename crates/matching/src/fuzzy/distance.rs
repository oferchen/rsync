//! Levenshtein-style fuzzy distance for basis-file selection.
//!
//! Faithful port of upstream rsync's `fuzzy_distance()` and
//! `find_filename_suffix()`. Lower distance means a closer name match; the
//! selection loop in [`super::search`] keeps the candidate with the lowest
//! distance, exactly mirroring `generator.c:find_fuzzy()`.
//!
//! upstream: util1.c:1588 `fuzzy_distance()`, util1.c:1528
//! `find_filename_suffix()`, generator.c:890-895 (combined name+suffix score).

/// One Levenshtein unit of edit distance. upstream: util1.c:1586 `#define UNIT (1 << 16)`.
pub(super) const UNIT: u32 = 1 << 16;

/// Distance sentinel returned when the length-difference heuristic proves the
/// edit distance must exceed the caller's upper limit. upstream: util1.c:1599
/// `return 0xFFFFU * UNIT + 1`.
const DIST_TOO_FAR: u32 = 0xFFFFu32 * UNIT + 1;

/// Computes the Levenshtein edit distance between two byte strings, weighted by
/// the ASCII distance between substituted characters.
///
/// This is a direct port of upstream `fuzzy_distance()`: `s1`/`s2` are compared
/// with each substitution costing `UNIT` plus the absolute byte difference, and
/// each insertion/deletion costing `UNIT` plus the inserted/deleted byte. All
/// arithmetic is `u32` with wraparound to match the C implementation exactly.
///
/// `upperlimit` is a pruning hint: when the length difference alone forces the
/// distance above it, [`DIST_TOO_FAR`] is returned without computing the matrix.
/// This never changes selection (a candidate pruned this way could not have won)
/// but preserves upstream's `--debug=FUZZY` distance output byte-for-byte.
///
/// upstream: util1.c:1588 `fuzzy_distance()`.
pub(super) fn fuzzy_distance(s1: &[u8], s2: &[u8], upperlimit: u32) -> u32 {
    let len1 = s1.len();
    let len2 = s2.len();

    // upstream: util1.c:1598 - prune using the length-difference lower bound.
    let len_diff = len1.abs_diff(len2) as u32;
    if len_diff.wrapping_mul(UNIT) > upperlimit {
        return DIST_TOO_FAR;
    }

    // upstream: util1.c:1601-1609 - one empty string: cost is the length in
    // UNITs plus the sum of the other string's bytes.
    if len1 == 0 || len2 == 0 {
        let s = if len1 == 0 { s2 } else { s1 };
        let mut cost: u32 = 0;
        for &b in s {
            cost = cost.wrapping_add(u32::from(b));
        }
        return (s.len() as u32).wrapping_mul(UNIT).wrapping_add(cost);
    }

    // upstream: util1.c:1611-1633 - single-row Levenshtein with ASCII weighting.
    let mut a = vec![0u32; len2];
    for (i2, slot) in a.iter_mut().enumerate() {
        *slot = ((i2 + 1) as u32).wrapping_mul(UNIT);
    }

    for (i1, &b1) in s1.iter().enumerate() {
        let mut diag = (i1 as u32).wrapping_mul(UNIT);
        let mut above = ((i1 + 1) as u32).wrapping_mul(UNIT);
        let c1 = u32::from(b1);
        for i2 in 0..len2 {
            let left = a[i2];
            let byte_diff = i32::from(b1) - i32::from(s2[i2]);
            // upstream: cost = 0 on a match, else UNIT + |byte_diff|.
            let cost = if byte_diff != 0 {
                UNIT.wrapping_add(byte_diff.unsigned_abs())
            } else {
                0
            };
            let diag_inc = diag.wrapping_add(cost);
            let left_inc = left.wrapping_add(UNIT).wrapping_add(c1);
            let above_inc = above.wrapping_add(UNIT).wrapping_add(u32::from(s2[i2]));
            let next = if left < above {
                left_inc.min(diag_inc)
            } else {
                above_inc.min(diag_inc)
            };
            a[i2] = next;
            above = next;
            diag = left;
        }
    }

    a[len2 - 1]
}

/// Distance below which the suffix bonus is still applied; at or above it the
/// candidate has already been disqualified. upstream: generator.c:893
/// `if (dist < 0xFFFF0000U)`.
const SUFFIX_APPLY_LIMIT: u32 = 0xFFFF_0000;

/// Combined name+suffix distance for one candidate, mirroring the per-candidate
/// scoring in upstream `find_fuzzy()`.
///
/// The base-name distance is computed first (pruned by `upperlimit`). Unless the
/// candidate was already disqualified, the suffix distance is added with a 10x
/// weight so that a shared significant suffix (e.g. `.tar.gz`) breaks ties toward
/// same-type files. `target_suffix` is precomputed by the caller since it is
/// constant across the scan.
///
/// upstream: generator.c:890-895.
pub(super) fn fuzzy_name_distance(
    candidate: &[u8],
    target: &[u8],
    target_suffix: &[u8],
    upperlimit: u32,
) -> u32 {
    let mut dist = fuzzy_distance(candidate, target, upperlimit);
    if dist < SUFFIX_APPLY_LIMIT {
        let candidate_suffix = find_filename_suffix(candidate);
        let suffix_dist = fuzzy_distance(candidate_suffix, target_suffix, SUFFIX_APPLY_LIMIT);
        dist = dist.wrapping_add(suffix_dist.wrapping_mul(10));
    }
    dist
}

/// Returns the most significant filename suffix, ignoring insignificant ones
/// such as a trailing `~`, `.bak`, `.old`, `.orig`, `.~1~`, and all-digit
/// suffixes. The returned slice includes the leading `.`.
///
/// Direct port of upstream `find_filename_suffix()`; the returned slice points
/// into `name`. An empty slice means no significant suffix was found.
///
/// upstream: util1.c:1528 `find_filename_suffix()`.
pub(super) fn find_filename_suffix(name: &[u8]) -> &[u8] {
    // upstream: util1.c:1534 - one or more leading dots aren't a suffix.
    let mut start = 0usize;
    let mut fn_len = name.len();
    while fn_len > 0 && name[start] == b'.' {
        start += 1;
        fn_len -= 1;
    }

    // upstream: util1.c:1537-1541 - ignore a trailing '~'.
    let had_tilde = fn_len > 1 && name[start + fn_len - 1] == b'~';
    if had_tilde {
        fn_len -= 1;
    }

    // upstream: util1.c:1543-1545 - assume no suffix.
    let mut suffix: &[u8] = &[];

    // upstream: util1.c:1548 - scan back through the significant suffixes.
    while fn_len > 1 {
        // upstream: util1.c:1549 - `while (*--s != '.' && s != fn) {}`.
        let mut s = start + fn_len;
        loop {
            s -= 1;
            if name[s] == b'.' || s == start {
                break;
            }
        }
        // upstream: util1.c:1550-1551 - reached the start with no dot.
        if s == start {
            break;
        }
        // upstream: util1.c:1552-1553.
        let s_off = s - start;
        let s_len = fn_len - s_off;
        fn_len = s_off;

        // upstream: util1.c:1554-1562 - skip insignificant suffixes. The
        // strcmp() compares against the NUL-terminated remainder of the full
        // name, so a trailing '~' left in the buffer defeats these matches.
        if s_len == 4 {
            let tail = &name[s + 1..];
            if tail == b"bak" || tail == b"old" {
                continue;
            }
        } else if s_len == 5 {
            if &name[s + 1..] == b"orig" {
                continue;
            }
        } else if s_len > 2 && had_tilde && name[s + 1] == b'~' && name[s + 2].is_ascii_digit() {
            continue;
        }

        // upstream: util1.c:1563-1566.
        suffix = &name[s..s + s_len];
        if s_len == 1 {
            break;
        }

        // upstream: util1.c:1567-1573 - an all-digit suffix is not significant;
        // keep scanning for an earlier one.
        let all_digits = name[s + 1..s + s_len].iter().all(u8::is_ascii_digit);
        if !all_digits {
            return suffix;
        }
    }

    suffix
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-traced upstream `fuzzy_distance()` values pin the port to the C
    /// implementation. Each expected value was derived by tracing util1.c:1588.
    #[test]
    fn distance_identical_strings_is_zero() {
        // No edits: matrix diagonal stays at 0.
        assert_eq!(fuzzy_distance(b"file.txt", b"file.txt", u32::MAX), 0);
    }

    #[test]
    fn distance_empty_candidate_sums_target_bytes() {
        // upstream: util1.c:1601-1609. len1 == 0 -> len*UNIT + sum(bytes).
        // "ab" = 97 + 98 = 195, len 2 -> 2*UNIT + 195.
        assert_eq!(fuzzy_distance(b"", b"ab", u32::MAX), 2 * UNIT + 195);
    }

    #[test]
    fn distance_empty_target_sums_candidate_bytes() {
        // len2 == 0 swaps to s1: "A" = 65, len 1 -> UNIT + 65.
        assert_eq!(fuzzy_distance(b"A", b"", u32::MAX), UNIT + 65);
    }

    #[test]
    fn distance_single_substitution_adds_unit_plus_ascii_gap() {
        // "b" vs "a": one substitution, |98-97| = 1 -> UNIT + 1.
        assert_eq!(fuzzy_distance(b"b", b"a", u32::MAX), UNIT + 1);
        // "c" vs "a": |99-97| = 2 -> UNIT + 2.
        assert_eq!(fuzzy_distance(b"c", b"a", u32::MAX), UNIT + 2);
    }

    #[test]
    fn distance_pure_insertion_costs_unit_plus_byte() {
        // "aX" vs "a": append 'X'(88) -> one insertion costs UNIT + 88.
        assert_eq!(
            fuzzy_distance(b"aX", b"a", u32::MAX),
            UNIT + u32::from(b'X')
        );
    }

    #[test]
    fn distance_length_diff_prunes_to_sentinel() {
        // len diff 3, upperlimit below 3*UNIT -> pruned sentinel.
        assert_eq!(fuzzy_distance(b"abcd", b"a", 2 * UNIT), DIST_TOO_FAR);
    }

    #[test]
    fn distance_one_edit_beats_two_edits() {
        let one = fuzzy_distance(b"report_2024", b"report_2023", u32::MAX);
        let two = fuzzy_distance(b"report_2044", b"report_2023", u32::MAX);
        assert!(
            one < two,
            "closer name must have smaller distance: {one} < {two}"
        );
    }

    #[test]
    fn suffix_simple_extension_includes_dot() {
        assert_eq!(find_filename_suffix(b"file.txt"), b".txt");
    }

    #[test]
    fn suffix_double_extension_is_last_significant() {
        // ".gz" is all-alpha and significant; scanning stops there.
        assert_eq!(find_filename_suffix(b"archive.tar.gz"), b".gz");
    }

    #[test]
    fn suffix_all_digit_is_skipped_for_earlier() {
        // ".2" is all-digit -> keep scanning -> ".tar".
        assert_eq!(find_filename_suffix(b"app.tar.2"), b".tar");
    }

    #[test]
    fn suffix_bak_is_ignored() {
        // ".bak" ignored -> ".txt".
        assert_eq!(find_filename_suffix(b"notes.txt.bak"), b".txt");
    }

    #[test]
    fn suffix_orig_is_ignored() {
        assert_eq!(find_filename_suffix(b"conf.ini.orig"), b".ini");
    }

    #[test]
    fn suffix_trailing_tilde_defeats_bak_match() {
        // The trailing '~' left in the buffer means the s_len==4 window is
        // ".bak" but the strcmp remainder is "bak~", so it is NOT ignored.
        assert_eq!(find_filename_suffix(b"file.bak~"), b".bak");
    }

    #[test]
    fn suffix_numbered_tilde_backup_is_ignored() {
        // ".~1~": trailing '~' trimmed, ".~1" matches the had_tilde rule.
        assert_eq!(find_filename_suffix(b"file.c.~1~"), b".c");
    }

    #[test]
    fn suffix_leading_dots_are_not_suffixes() {
        assert_eq!(find_filename_suffix(b".hidden"), b"");
    }

    #[test]
    fn suffix_no_extension_is_empty() {
        assert_eq!(find_filename_suffix(b"README"), b"");
    }

    #[test]
    fn name_distance_adds_weighted_suffix() {
        // Same base distance but a matching suffix must score lower (closer).
        let target = b"data.csv";
        let target_suffix = find_filename_suffix(target);
        let same_ext = fuzzy_name_distance(b"info.csv", target, target_suffix, u32::MAX);
        let diff_ext = fuzzy_name_distance(b"info.txt", target, target_suffix, u32::MAX);
        assert!(
            same_ext < diff_ext,
            "matching suffix must lower distance: {same_ext} < {diff_ext}"
        );
    }
}
