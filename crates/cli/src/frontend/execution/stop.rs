//! Parsing for the `--stop-after` (duration) and `--stop-at` (deadline) flags.

use std::ffi::OsStr;
use std::time::{Duration, SystemTime};

use core::message::{Message, Role};
use core::rsync_error;
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

/// Parses `--stop-after=MINUTES` into an absolute deadline `MINUTES` from now.
///
/// The value is a positive integer (an optional leading `+` is allowed). Zero,
/// empty, and non-numeric input is rejected.
pub(crate) fn parse_stop_after_argument(value: &OsStr) -> Result<SystemTime, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    if trimmed.is_empty() {
        return Err(invalid_stop_after(display));
    }

    let normalized = trimmed.strip_prefix('+').unwrap_or(trimmed);
    if normalized.is_empty() || !normalized.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(invalid_stop_after(display));
    }

    let minutes: u64 = normalized
        .parse()
        .map_err(|_| invalid_stop_after(display))?;
    if minutes == 0 {
        return Err(invalid_stop_after(display));
    }

    let seconds = minutes
        .checked_mul(60)
        .ok_or_else(|| invalid_stop_after(display))?;
    let deadline = SystemTime::now()
        .checked_add(Duration::from_secs(seconds))
        .ok_or_else(|| invalid_stop_after(display))?;

    Ok(deadline)
}

/// Parses a `--stop-at` timestamp into an absolute deadline.
///
/// Accepts upstream's flexible `[YEAR-MON-DAY][THOUR:MIN]` grammar (see
/// `parse_stop_at_internal`), interpreted in the process's local timezone.
/// Rejects malformed input and any time that does not resolve to the future.
pub(crate) fn parse_stop_at_argument(value: &OsStr) -> Result<SystemTime, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    let now = OffsetDateTime::now_local().map_err(|_| local_offset_unavailable(display))?;
    match parse_stop_at_internal(trimmed, now) {
        Ok(deadline) => Ok(deadline),
        Err(StopAtError::InvalidFormat) => Err(invalid_stop_at(display)),
        Err(StopAtError::NotInFuture) => Err(stop_at_not_future(display)),
    }
}

fn invalid_stop_after(value: &str) -> Message {
    rsync_error!(1, format!("invalid --stop-after value: {value}")).with_role(Role::Client)
}

fn invalid_stop_at(value: &str) -> Message {
    rsync_error!(1, format!("invalid --stop-at format: {value}")).with_role(Role::Client)
}

fn stop_at_not_future(value: &str) -> Message {
    rsync_error!(1, format!("--stop-at time is not in the future: {value}")).with_role(Role::Client)
}

fn local_offset_unavailable(value: &str) -> Message {
    rsync_error!(
        1,
        format!("--stop-at could not determine local timezone: {value}")
    )
    .with_role(Role::Client)
}

#[derive(Debug)]
enum StopAtError {
    InvalidFormat,
    NotInFuture,
}

#[derive(Debug)]
enum BuildError {
    InvalidDate,
}

impl From<BuildError> for StopAtError {
    fn from(error: BuildError) -> Self {
        match error {
            BuildError::InvalidDate => StopAtError::InvalidFormat,
        }
    }
}

// upstream: options.c:1167 parse_time() - reference "now" comes from
// localtime(&now) and mktime() interprets the constructed tm as local time.
// We mirror that by capturing the local OffsetDateTime once at the call site
// and reusing its UTC offset for every candidate datetime.
fn parse_stop_at_internal(input: &str, now: OffsetDateTime) -> Result<SystemTime, StopAtError> {
    if input.is_empty() {
        return Err(StopAtError::InvalidFormat);
    }

    let bytes = input.as_bytes();
    let mut idx = 0usize;
    let len = bytes.len();
    let mut tm_year: i32 = -1;
    let mut tm_mon: i32 = -1;
    let mut tm_mday: i32 = -1;
    let mut tm_hour: i32 = -1;
    let mut tm_min: i32 = -1;
    let mut in_date: i32;

    if matches!(bytes[0], b'T' | b't' | b':') {
        in_date = if bytes[0] == b':' { 0 } else { -1 };
        idx += 1;
        if idx >= len {
            return Err(StopAtError::InvalidFormat);
        }
    } else {
        in_date = 1;
    }

    while idx < len {
        if !bytes[idx].is_ascii_digit() {
            return Err(StopAtError::InvalidFormat);
        }
        let mut n: i32 = 0;
        while idx < len && bytes[idx].is_ascii_digit() {
            n = n.checked_mul(10).ok_or(StopAtError::InvalidFormat)? + i32::from(bytes[idx] - b'0');
            idx += 1;
        }
        if idx < len && bytes[idx] == b':' {
            in_date = 0;
        }
        if in_date > 0 {
            if tm_year != -1 {
                return Err(StopAtError::InvalidFormat);
            }
            tm_year = tm_mon;
            tm_mon = tm_mday;
            tm_mday = n;
            if idx == len {
                break;
            }
            match bytes[idx] {
                b'T' | b't' => {
                    idx += 1;
                    if idx == len {
                        break;
                    }
                    in_date = -1;
                }
                b'-' | b'/' => {
                    idx += 1;
                }
                _ => return Err(StopAtError::InvalidFormat),
            }
            continue;
        }
        if tm_hour != -1 {
            return Err(StopAtError::InvalidFormat);
        }
        tm_hour = tm_min;
        tm_min = n;
        if idx == len {
            if in_date < 0 {
                return Err(StopAtError::InvalidFormat);
            }
            break;
        }
        if bytes[idx] != b':' {
            return Err(StopAtError::InvalidFormat);
        }
        idx += 1;
        in_date = 0;
    }

    let local_offset = now.offset();
    let original_tm_year = tm_year;
    let original_tm_mon = tm_mon;
    let original_tm_mday = tm_mday;
    let mut in_date_flag = if in_date > 0 { in_date } else { 0 };

    if tm_year < 0 {
        tm_year = now.year() - 1900;
        in_date_flag = in_date_flag.max(1);
    } else if tm_year < 100 {
        let today_year = now.year() - 1900;
        while tm_year < today_year {
            tm_year += 100;
        }
    } else {
        tm_year -= 1900;
    }

    if tm_mon < 0 {
        tm_mon = (now.month() as i32) - 1;
        in_date_flag = in_date_flag.max(2);
    } else {
        tm_mon -= 1;
    }

    if tm_mday < 0 {
        tm_mday = now.day() as i32;
        in_date_flag = in_date_flag.max(3);
    }

    let mut repeat_seconds: i64 = 0;
    if tm_min < 0 {
        tm_hour = 0;
        tm_min = 0;
    } else if tm_hour < 0 {
        if in_date_flag != 3 {
            return Err(StopAtError::InvalidFormat);
        }
        in_date_flag = 0;
        tm_hour = now.hour() as i32;
        repeat_seconds = 60 * 60;
    }

    if !(0..=23).contains(&tm_hour)
        || !(0..=59).contains(&tm_min)
        || !(0..12).contains(&tm_mon)
        || !(1..=31).contains(&tm_mday)
    {
        return Err(StopAtError::InvalidFormat);
    }

    let mut old_mday = tm_mday;
    let mut datetime =
        build_offset_datetime(tm_year, tm_mon, tm_mday, tm_hour, tm_min, local_offset)?;
    let no_date_specified = original_tm_year < 0 && original_tm_mon < 0 && original_tm_mday < 0;

    if no_date_specified && datetime <= now {
        return Err(StopAtError::NotInFuture);
    }

    while in_date_flag > 0 && (datetime <= now || tm_mday < old_mday) {
        match in_date_flag {
            3 => {
                tm_mday += 1;
                old_mday = tm_mday;
            }
            2 => {
                if tm_mday < old_mday {
                    tm_mday = old_mday;
                } else {
                    tm_mon += 1;
                    if tm_mon == 12 {
                        tm_mon = 0;
                        tm_year += 1;
                    }
                }
            }
            1 => {
                if tm_mday < old_mday {
                    if tm_mon != 1 || old_mday != 29 {
                        return Err(StopAtError::InvalidFormat);
                    }
                    tm_mon = 1;
                    tm_mday = 29;
                }
                tm_year += 1;
            }
            _ => unreachable!(),
        }

        match build_offset_datetime(tm_year, tm_mon, tm_mday, tm_hour, tm_min, local_offset) {
            Ok(new_dt) => {
                datetime = new_dt;
            }
            Err(BuildError::InvalidDate) => {
                if in_date_flag != 3 || tm_mday <= 28 {
                    return Err(StopAtError::InvalidFormat);
                }
                tm_mday = 1;
                old_mday = 1;
                in_date_flag = 2;
                continue;
            }
        }
    }

    if repeat_seconds > 0 {
        while datetime <= now {
            datetime = datetime
                .checked_add(time::Duration::seconds(repeat_seconds))
                .ok_or(StopAtError::InvalidFormat)?;
        }
    }

    if datetime <= now {
        return Err(StopAtError::NotInFuture);
    }

    offset_datetime_to_system_time(datetime)
}

// upstream: options.c parse_time uses mktime() with the process's current
// timezone for every candidate. We capture the offset once at "now" and reuse
// it - this matches upstream when no DST transition falls between now and the
// target datetime, which is the typical use case for --stop-at deadlines.
fn build_offset_datetime(
    tm_year: i32,
    tm_mon: i32,
    tm_mday: i32,
    tm_hour: i32,
    tm_min: i32,
    local_offset: UtcOffset,
) -> Result<OffsetDateTime, BuildError> {
    let year = tm_year + 1900;
    let month = Month::try_from((tm_mon + 1) as u8).map_err(|_| BuildError::InvalidDate)?;
    let day = u8::try_from(tm_mday).map_err(|_| BuildError::InvalidDate)?;
    let date = Date::from_calendar_date(year, month, day).map_err(|_| BuildError::InvalidDate)?;
    let time =
        Time::from_hms(tm_hour as u8, tm_min as u8, 0).map_err(|_| BuildError::InvalidDate)?;
    let primitive = PrimitiveDateTime::new(date, time);
    Ok(primitive.assume_offset(local_offset))
}

fn offset_datetime_to_system_time(datetime: OffsetDateTime) -> Result<SystemTime, StopAtError> {
    let seconds = datetime.unix_timestamp();
    if seconds < 0 {
        return Err(StopAtError::InvalidFormat);
    }
    let nanos = datetime.nanosecond();
    let base = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(seconds as u64))
        .ok_or(StopAtError::InvalidFormat)?;
    base.checked_add(Duration::from_nanos(u64::from(nanos)))
        .ok_or(StopAtError::InvalidFormat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// Fixed reference "now" for deterministic parser tests: 2026-06-15T12:00:00+02:00.
    /// Using a non-UTC offset proves the parser interprets inputs in the local zone.
    fn fixed_local_now() -> OffsetDateTime {
        let date = Date::from_calendar_date(2026, Month::June, 15).unwrap();
        let time = Time::from_hms(12, 0, 0).unwrap();
        let offset = UtcOffset::from_hms(2, 0, 0).unwrap();
        PrimitiveDateTime::new(date, time).assume_offset(offset)
    }

    fn parse_with_now(input: &str, now: OffsetDateTime) -> Result<SystemTime, StopAtError> {
        parse_stop_at_internal(input, now)
    }

    #[test]
    fn stop_after_valid_minutes() {
        let result = parse_stop_after_argument(&OsString::from("10"));
        assert!(result.is_ok());
        let deadline = result.unwrap();
        let duration = deadline.duration_since(SystemTime::now()).unwrap();
        // Should be approximately 10 minutes (600 seconds), allow small drift
        assert!(duration.as_secs() >= 598 && duration.as_secs() <= 602);
    }

    #[test]
    fn stop_after_with_plus_prefix() {
        let result = parse_stop_after_argument(&OsString::from("+15"));
        assert!(result.is_ok());
        let deadline = result.unwrap();
        let duration = deadline.duration_since(SystemTime::now()).unwrap();
        assert!(duration.as_secs() >= 898 && duration.as_secs() <= 902);
    }

    #[test]
    fn stop_after_rejects_zero() {
        let result = parse_stop_after_argument(&OsString::from("0"));
        assert!(result.is_err());
    }

    #[test]
    fn stop_after_rejects_empty() {
        let result = parse_stop_after_argument(&OsString::from(""));
        assert!(result.is_err());
    }

    #[test]
    fn stop_after_rejects_whitespace_only() {
        let result = parse_stop_after_argument(&OsString::from("   "));
        assert!(result.is_err());
    }

    #[test]
    fn stop_after_rejects_non_numeric() {
        let result = parse_stop_after_argument(&OsString::from("abc"));
        assert!(result.is_err());
    }

    #[test]
    fn stop_after_rejects_negative() {
        let result = parse_stop_after_argument(&OsString::from("-10"));
        assert!(result.is_err());
    }

    #[test]
    fn stop_after_rejects_decimal() {
        let result = parse_stop_after_argument(&OsString::from("10.5"));
        assert!(result.is_err());
    }

    #[test]
    fn stop_after_rejects_mixed() {
        let result = parse_stop_after_argument(&OsString::from("10abc"));
        assert!(result.is_err());
    }

    #[test]
    fn stop_after_handles_whitespace_padding() {
        let result = parse_stop_after_argument(&OsString::from("  5  "));
        assert!(result.is_ok());
    }

    #[test]
    fn stop_after_single_minute() {
        let result = parse_stop_after_argument(&OsString::from("1"));
        assert!(result.is_ok());
        let deadline = result.unwrap();
        let duration = deadline.duration_since(SystemTime::now()).unwrap();
        assert!(duration.as_secs() >= 58 && duration.as_secs() <= 62);
    }

    #[test]
    fn stop_after_large_value() {
        let result = parse_stop_after_argument(&OsString::from("1440")); // 24 hours
        assert!(result.is_ok());
        let deadline = result.unwrap();
        let duration = deadline.duration_since(SystemTime::now()).unwrap();
        // 24 hours = 86400 seconds
        assert!(duration.as_secs() >= 86398 && duration.as_secs() <= 86402);
    }

    #[test]
    fn stop_at_rejects_empty() {
        let result = parse_stop_at_argument(&OsString::from(""));
        assert!(result.is_err());
    }

    #[test]
    fn stop_at_rejects_whitespace_only() {
        let result = parse_stop_at_argument(&OsString::from("   "));
        assert!(result.is_err());
    }

    #[test]
    fn stop_at_rejects_invalid_format() {
        let result = parse_stop_at_argument(&OsString::from("invalid"));
        assert!(result.is_err());
    }

    #[test]
    fn stop_at_rejects_invalid_hour() {
        let result = parse_stop_at_argument(&OsString::from("25:00"));
        assert!(result.is_err());
    }

    #[test]
    fn stop_at_rejects_invalid_minute() {
        let result = parse_stop_at_argument(&OsString::from("12:60"));
        assert!(result.is_err());
    }

    #[test]
    fn stop_at_rejects_invalid_month() {
        let result = parse_stop_at_argument(&OsString::from("2025-13-01"));
        assert!(result.is_err());
    }

    #[test]
    fn stop_at_rejects_invalid_day() {
        let result = parse_stop_at_argument(&OsString::from("2025-01-32"));
        assert!(result.is_err());
    }

    #[test]
    fn stop_at_internal_empty_input() {
        let result = parse_with_now("", fixed_local_now());
        assert!(matches!(result, Err(StopAtError::InvalidFormat)));
    }

    #[test]
    fn stop_at_internal_only_t_prefix() {
        let result = parse_with_now("T", fixed_local_now());
        assert!(matches!(result, Err(StopAtError::InvalidFormat)));
    }

    #[test]
    fn stop_at_internal_only_colon_prefix() {
        let result = parse_with_now(":", fixed_local_now());
        assert!(matches!(result, Err(StopAtError::InvalidFormat)));
    }

    #[test]
    fn stop_at_internal_non_digit_start() {
        let result = parse_with_now("abc", fixed_local_now());
        assert!(matches!(result, Err(StopAtError::InvalidFormat)));
    }

    #[test]
    fn stop_at_internal_negative_hour() {
        // Hours must be 0-23
        let result = parse_with_now("T-1:00", fixed_local_now());
        assert!(matches!(result, Err(StopAtError::InvalidFormat)));
    }

    #[test]
    fn stop_at_internal_hour_24() {
        let result = parse_with_now("T24:00", fixed_local_now());
        assert!(matches!(result, Err(StopAtError::InvalidFormat)));
    }

    #[test]
    fn stop_at_internal_minute_60() {
        let result = parse_with_now("T12:60", fixed_local_now());
        assert!(matches!(result, Err(StopAtError::InvalidFormat)));
    }

    #[test]
    fn stop_at_internal_month_0() {
        let result = parse_with_now("2030-00-15", fixed_local_now());
        assert!(matches!(result, Err(StopAtError::InvalidFormat)));
    }

    #[test]
    fn stop_at_internal_month_13() {
        let result = parse_with_now("2030-13-15", fixed_local_now());
        assert!(matches!(result, Err(StopAtError::InvalidFormat)));
    }

    #[test]
    fn stop_at_internal_day_0() {
        let result = parse_with_now("2030-06-00", fixed_local_now());
        assert!(matches!(result, Err(StopAtError::InvalidFormat)));
    }

    #[test]
    fn stop_at_internal_day_32() {
        let result = parse_with_now("2030-06-32", fixed_local_now());
        assert!(matches!(result, Err(StopAtError::InvalidFormat)));
    }

    #[test]
    fn stop_at_internal_feb_30() {
        let result = parse_with_now("2030-02-30", fixed_local_now());
        assert!(matches!(result, Err(StopAtError::InvalidFormat)));
    }

    #[test]
    fn stop_at_error_debug() {
        let error = StopAtError::InvalidFormat;
        let debug = format!("{error:?}");
        assert!(debug.contains("InvalidFormat"));
    }

    #[test]
    fn stop_at_error_not_in_future_debug() {
        let error = StopAtError::NotInFuture;
        let debug = format!("{error:?}");
        assert!(debug.contains("NotInFuture"));
    }

    #[test]
    fn build_error_debug() {
        let error = BuildError::InvalidDate;
        let debug = format!("{error:?}");
        assert!(debug.contains("InvalidDate"));
    }

    #[test]
    fn build_error_converts_to_stop_at_error_invalid() {
        let build_error = BuildError::InvalidDate;
        let stop_error: StopAtError = build_error.into();
        assert!(matches!(stop_error, StopAtError::InvalidFormat));
    }

    #[test]
    fn offset_datetime_converts_epoch() {
        let epoch = OffsetDateTime::UNIX_EPOCH;
        let result = offset_datetime_to_system_time(epoch);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn offset_datetime_converts_future() {
        let date = Date::from_calendar_date(2030, Month::June, 15).unwrap();
        let time = Time::from_hms(12, 30, 0).unwrap();
        let datetime = PrimitiveDateTime::new(date, time).assume_utc();
        let result = offset_datetime_to_system_time(datetime);
        assert!(result.is_ok());
        assert!(result.unwrap() > SystemTime::now());
    }

    #[test]
    fn invalid_stop_after_message_contains_value() {
        let msg = invalid_stop_after("bad_value");
        let text = msg.to_string();
        assert!(text.contains("bad_value"));
        assert!(text.contains("--stop-after"));
    }

    #[test]
    fn invalid_stop_at_message_contains_value() {
        let msg = invalid_stop_at("bad_format");
        let text = msg.to_string();
        assert!(text.contains("bad_format"));
        assert!(text.contains("--stop-at"));
    }

    #[test]
    fn stop_at_not_future_message_contains_value() {
        let msg = stop_at_not_future("past_time");
        let text = msg.to_string();
        assert!(text.contains("past_time"));
        assert!(text.contains("not in the future"));
    }

    #[test]
    fn stop_at_internal_dash_separator() {
        let result = parse_with_now("2099-12-31T23:59", fixed_local_now()).expect("parses");
        assert!(result > SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn stop_at_internal_slash_separator() {
        let result = parse_with_now("2099/12/31T23:59", fixed_local_now()).expect("parses");
        assert!(result > SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn stop_at_internal_lowercase_t() {
        let result = parse_with_now("2099-12-31t23:59", fixed_local_now()).expect("parses");
        assert!(result > SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn stop_at_internal_two_digit_year_future() {
        // 99 should be interpreted as 2099 (or later century in the future)
        let result = parse_with_now("99-12-31", fixed_local_now()).expect("parses");
        assert!(result > SystemTime::UNIX_EPOCH);
    }

    /// upstream parity: `YYYY-MM-DDTHH:MM` is interpreted in the process's local
    /// timezone. With `now = 2026-06-15T12:00:00+02:00`, the input
    /// `2026-12-31T23:59` resolves to `2026-12-31T23:59:00+02:00` -> unix
    /// `1798754340`. UTC interpretation would have yielded `1798761540` (+7200s).
    #[test]
    fn stop_at_absolute_interpreted_in_local_zone() {
        let now = fixed_local_now();
        let parsed = parse_with_now("2026-12-31T23:59", now).expect("parses");
        let unix = parsed.duration_since(SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(unix.as_secs(), 1_798_754_340);
    }

    /// upstream parity: with a negative offset, the same wall-clock input maps
    /// to a different unix timestamp - proving the parser honours local TZ.
    #[test]
    fn stop_at_absolute_local_zone_negative_offset() {
        let date = Date::from_calendar_date(2026, Month::June, 15).unwrap();
        let time = Time::from_hms(12, 0, 0).unwrap();
        let offset = UtcOffset::from_hms(-5, 0, 0).unwrap();
        let now = PrimitiveDateTime::new(date, time).assume_offset(offset);

        let parsed = parse_with_now("2026-12-31T23:59", now).expect("parses");
        let unix = parsed.duration_since(SystemTime::UNIX_EPOCH).unwrap();
        // 2026-12-31T23:59:00-05:00 == unix 1798779540
        assert_eq!(unix.as_secs(), 1_798_779_540);
    }

    /// Edge: end-of-year boundary in upstream's local-time convention.
    #[test]
    fn stop_at_end_of_year_local_time() {
        let now = fixed_local_now();
        let parsed = parse_with_now("2025-12-31T23:59", now);
        // 2025-12-31 is in the past relative to the fixed 2026-06-15 "now", but
        // the year is explicit so upstream bumps the loop until NotInFuture.
        // Our parser surfaces NotInFuture in that case - the behavioural pin.
        assert!(matches!(parsed, Err(StopAtError::NotInFuture)));
    }

    /// Stop-after duration is timezone-irrelevant - duration is added to "now".
    #[test]
    fn stop_after_duration_is_timezone_independent() {
        let before = SystemTime::now();
        let deadline = parse_stop_after_argument(&OsString::from("90")).expect("parses");
        let delta = deadline.duration_since(before).unwrap();
        assert!(delta.as_secs() >= 5398 && delta.as_secs() <= 5402);
    }
}
