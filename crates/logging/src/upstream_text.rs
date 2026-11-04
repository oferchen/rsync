//! Upstream's shared text renderers for modes, counts and times.
//!
//! Each function reproduces one upstream helper byte for byte, so every oc
//! surface that prints the same field (the `--list-only` listing, daemon log
//! lines, the log-file prefix) renders it from one place.

/// `timestring()`'s result buffer: 20 bytes, so at most 19 characters survive
/// `snprintf()` (upstream: util1.c:1725 `static char buffers[4][20]`).
const TIMESTRING_MAX_LEN: usize = 19;

/// Renders `mode` as a 10-character `ls`-style string: the type character,
/// then the nine permission characters with setuid, setgid and sticky folded
/// in.
///
/// upstream: lib/permstring.c `permstring()`.
#[must_use]
pub fn permstring(mode: u32) -> String {
    const PERM_MAP: &[u8; 9] = b"rwxrwxrwx";
    let mut perms = *b"----------";
    for (i, slot) in perms[1..].iter_mut().enumerate() {
        if mode & (0o400 >> i) != 0 {
            *slot = PERM_MAP[i];
        }
    }
    if mode & 0o4000 != 0 {
        perms[3] = if mode & 0o100 != 0 { b's' } else { b'S' };
    }
    if mode & 0o2000 != 0 {
        perms[6] = if mode & 0o010 != 0 { b's' } else { b'S' };
    }
    if mode & 0o1000 != 0 {
        perms[9] = if mode & 0o001 != 0 { b't' } else { b'T' };
    }
    perms[0] = match mode & 0o170_000 {
        0o040_000 => b'd',
        0o120_000 => b'l',
        0o060_000 => b'b',
        0o020_000 => b'c',
        0o140_000 => b's',
        0o010_000 => b'p',
        _ => b'-',
    };
    perms.iter().map(|&b| char::from(b)).collect()
}

/// Renders `num` with a comma between every group of three digits.
///
/// upstream: lib/compat.c `do_big_num()` with `human_flag == 1` - the
/// `human_num()` and `comma_num()` rendering at the default `-h` level.
#[must_use]
pub fn comma_num(num: u64) -> String {
    let digits = num.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// Renders `unix_secs` as upstream `timestring()` does for a zone `offset`
/// seconds east of UTC.
///
/// Pre-epoch times render normally, a year that does not fit `%4d` keeps its
/// extra digits and loses the tail to the 19-character buffer, and a time
/// `localtime_r()` cannot represent renders `(time out of range)`.
///
/// upstream: util1.c:1722-1739 `timestring()`.
#[must_use]
pub fn timestring_at_offset(unix_secs: i64, offset: i64) -> String {
    // A saturated sum lies far outside the int tm_year range checked below.
    let local_secs = unix_secs.saturating_add(offset);
    let (year, month, day) = civil_from_days(local_secs.div_euclid(86_400));
    // upstream: util1.c:1729-1731 - a NULL localtime_r() renders the literal;
    // localtime_r() fails exactly when tm_year (year - 1900) overflows an int.
    let Ok(tm_year) = i32::try_from(year - 1900) else {
        return "(time out of range)".to_owned();
    };
    let day_seconds = local_secs.rem_euclid(86_400);
    let hours = day_seconds / 3_600;
    let minutes = (day_seconds % 3_600) / 60;
    let seconds = day_seconds % 60;
    // upstream: util1.c:1733-1735 - `(int)tm->tm_year + 1900` wraps in C int
    // arithmetic, `%4d` space-pads, and snprintf() truncates to the buffer.
    let year = tm_year.wrapping_add(1900);
    let mut rendered = format!("{year:4}/{month:02}/{day:02} {hours:02}:{minutes:02}:{seconds:02}");
    rendered.truncate(TIMESTRING_MAX_LEN);
    rendered
}

/// Converts a day count (days since 1970-01-01) to a civil `(year, month, day)`.
///
/// Algorithm from Howard Hinnant's date library (public domain).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// upstream: lib/permstring.c - the type character comes from the file
    /// type, and the special bits print lowercase only over an execute bit.
    #[test]
    fn permstring_matches_upstream() {
        assert_eq!(permstring(0o100_644), "-rw-r--r--");
        assert_eq!(permstring(0o040_755), "drwxr-xr-x");
        assert_eq!(permstring(0o120_777), "lrwxrwxrwx");
        assert_eq!(permstring(0o060_660), "brw-rw----");
        assert_eq!(permstring(0o020_620), "crw--w----");
        assert_eq!(permstring(0o140_755), "srwxr-xr-x");
        assert_eq!(permstring(0o010_644), "prw-r--r--");
        assert_eq!(permstring(0o104_755), "-rwsr-xr-x");
        assert_eq!(permstring(0o104_644), "-rwSr--r--");
        assert_eq!(permstring(0o102_750), "-rwxr-s---");
        assert_eq!(permstring(0o041_777), "drwxrwxrwt");
        assert_eq!(permstring(0o041_776), "drwxrwxrwT");
        assert_eq!(permstring(0o644), "-rw-r--r--");
    }

    /// upstream: lib/compat.c `do_big_num()` - a comma before every group of
    /// three digits counted from the right, none for a short number.
    #[test]
    fn comma_num_groups_thousands() {
        assert_eq!(comma_num(0), "0");
        assert_eq!(comma_num(999), "999");
        assert_eq!(comma_num(1_000), "1,000");
        assert_eq!(comma_num(4_096), "4,096");
        assert_eq!(comma_num(123_456_789), "123,456,789");
        assert_eq!(comma_num(u64::MAX), "18,446,744,073,709,551,615");
    }

    /// upstream: util1.c:1722-1739 `timestring()` over the whole `time_t`
    /// range, each value measured against rsync 3.5.1 under `TZ=UTC`: `%4d`
    /// space-pads short and negative years, a longer year pushes the tail out
    /// of the 20-byte buffer, `tm_year + 1900` wraps in `int`, and only an
    /// unrepresentable `tm_year` renders the out-of-range literal.
    #[test]
    fn timestring_matches_upstream_across_the_time_t_range() {
        let cases: [(i64, &str); 12] = [
            (-40_000_000_000, " 702/06/15 00:53:20"),
            (-62_135_596_800, "   1/01/01 00:00:00"),
            (-30_610_224_001, " 999/12/31 23:59:59"),
            (-2_208_988_800, "1900/01/01 00:00:00"),
            (-1, "1969/12/31 23:59:59"),
            (0, "1970/01/01 00:00:00"),
            (253_402_300_799, "9999/12/31 23:59:59"),
            (253_402_300_800, "10000/01/01 00:00:0"),
            (67_767_976_233_316_799, "2147483647/12/29 11"),
            (67_768_036_191_676_799, "-2147481749/12/31 2"),
            (67_768_036_191_676_800, "(time out of range)"),
            (i64::MAX, "(time out of range)"),
        ];
        for (secs, expected) in cases {
            assert_eq!(timestring_at_offset(secs, 0), expected, "t={secs}");
        }
    }

    /// The offset is applied before the range check, as `localtime_r()`
    /// converts to local time before filling `tm_year`.
    #[test]
    fn timestring_applies_the_offset_before_the_range_check() {
        assert_eq!(timestring_at_offset(-1, 3_600), "1970/01/01 00:59:59");
        assert_eq!(timestring_at_offset(i64::MAX, 1), "(time out of range)");
        assert_eq!(timestring_at_offset(i64::MIN, -1), "(time out of range)");
    }
}
