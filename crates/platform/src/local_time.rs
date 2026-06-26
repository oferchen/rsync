//! Local timezone offset for a specific instant.
//!
//! Mirrors upstream rsync's `timestring()` (`util1.c`), which renders file
//! modtimes through `localtime_r`. The offset is computed *per instant*, so
//! daylight-saving time is correct for each file's modtime - a file modified
//! in July and one modified in January render with their own offsets.
//!
//! `localtime_r` is POSIX-thread-safe (it writes into a caller-owned `tm`),
//! so this works in oc-rsync's multi-threaded process. That is the crucial
//! difference from the `time` crate's `OffsetDateTime::now_local()`, which
//! refuses to read the local offset once any other thread exists.

/// Returns the local UTC offset, in seconds east of UTC, in effect at
/// `unix_secs` - accounting for DST at that instant, matching upstream
/// `localtime_r`-based `timestring()`.
///
/// Returns `0` (UTC) when the offset cannot be determined.
#[cfg(unix)]
#[allow(unsafe_code)]
#[must_use]
pub fn local_utc_offset_seconds(unix_secs: i64) -> i32 {
    let t = unix_secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` is POSIX-thread-safe; it writes the broken-down
    // local time into the caller-owned `tm` and returns its address (or NULL
    // on failure). We pass a valid `time_t` and an owned, zeroed `tm`.
    let result = unsafe { libc::localtime_r(&t, &mut tm) };
    if result.is_null() {
        return 0;
    }
    tm.tm_gmtoff as i32
}

/// Windows lacks `localtime_r`/`tm_gmtoff`; rendering the listing column in
/// the local timezone there is a follow-up. Returns `0` (UTC) for now.
#[cfg(not(unix))]
#[must_use]
pub fn local_utc_offset_seconds(_unix_secs: i64) -> i32 {
    0
}

#[cfg(all(test, unix))]
mod tests {
    use super::local_utc_offset_seconds;

    #[test]
    fn offset_is_a_valid_utc_offset() {
        // Whatever the test host's timezone, the offset must be a real one:
        // within [-12h, +14h] and a whole number of minutes.
        let offset = local_utc_offset_seconds(1_700_000_000);
        assert!(
            (-12 * 3600..=14 * 3600).contains(&offset),
            "offset {offset} out of range"
        );
        assert_eq!(offset % 60, 0, "offset {offset} is not minute-aligned");
    }

    #[test]
    fn offset_matches_localtime_r_per_instant() {
        // Summer and winter timestamps in a DST zone differ by an hour; in a
        // non-DST zone they are equal. Either way both must be valid offsets,
        // and their difference is 0 or +-3600 - never anything else.
        let summer = local_utc_offset_seconds(1_690_000_000); // 2023-07-22
        let winter = local_utc_offset_seconds(1_704_000_000); // 2023-12-31
        let delta = (summer - winter).abs();
        assert!(delta == 0 || delta == 3600, "unexpected DST delta {delta}");
    }
}
