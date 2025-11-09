use std::ffi::OsStr;
use std::time::{Duration, SystemTime};

use core::message::{Message, Role};
use core::rsync_error;
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

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

pub(crate) fn parse_stop_at_argument(value: &OsStr) -> Result<SystemTime, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    match parse_stop_at_internal(trimmed) {
        Ok(deadline) => Ok(deadline),
        Err(StopAtError::InvalidFormat) | Err(StopAtError::LocalOffset) => {
            Err(invalid_stop_at(display))
        }
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

#[derive(Debug)]
enum StopAtError {
    InvalidFormat,
    LocalOffset,
    NotInFuture,
}

#[derive(Debug)]
enum BuildError {
    InvalidDate,
    LocalOffset,
}

impl From<BuildError> for StopAtError {
    fn from(error: BuildError) -> Self {
        match error {
            BuildError::InvalidDate => StopAtError::InvalidFormat,
            BuildError::LocalOffset => StopAtError::LocalOffset,
        }
    }
}

fn parse_stop_at_internal(input: &str) -> Result<SystemTime, StopAtError> {
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

    let now = OffsetDateTime::now_local().map_err(|_| StopAtError::LocalOffset)?;
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
        build_offset_datetime(tm_year, tm_mon, tm_mday, tm_hour, tm_min, now.offset())?;
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

        match build_offset_datetime(tm_year, tm_mon, tm_mday, tm_hour, tm_min, datetime.offset()) {
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
            Err(BuildError::LocalOffset) => return Err(StopAtError::LocalOffset),
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

fn build_offset_datetime(
    tm_year: i32,
    tm_mon: i32,
    tm_mday: i32,
    tm_hour: i32,
    tm_min: i32,
    initial_offset: UtcOffset,
) -> Result<OffsetDateTime, BuildError> {
    let year = tm_year + 1900;
    let month = Month::try_from((tm_mon + 1) as u8).map_err(|_| BuildError::InvalidDate)?;
    let day = u8::try_from(tm_mday).map_err(|_| BuildError::InvalidDate)?;
    let date = Date::from_calendar_date(year, month, day).map_err(|_| BuildError::InvalidDate)?;
    let time =
        Time::from_hms(tm_hour as u8, tm_min as u8, 0).map_err(|_| BuildError::InvalidDate)?;
    let primitive = PrimitiveDateTime::new(date, time);
    let mut datetime = primitive.assume_offset(initial_offset);
    let mut attempts = 0u8;

    loop {
        match UtcOffset::local_offset_at(datetime) {
            Ok(offset) if offset == datetime.offset() => return Ok(datetime),
            Ok(offset) => {
                datetime = primitive.assume_offset(offset);
            }
            Err(_) => return Err(BuildError::LocalOffset),
        }

        attempts += 1;
        if attempts >= 3 {
            return Err(BuildError::LocalOffset);
        }
    }
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
