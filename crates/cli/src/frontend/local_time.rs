//! Local-timezone conversion for rendered timestamps.
//!
//! Upstream rsync's `timestring()` renders file modtimes in local time via
//! `localtime_r`, computing the offset per instant (so DST is correct for each
//! file's modtime). We mirror that by asking [`platform::local_time`] for the
//! offset in effect at the given instant - `localtime_r` is thread-safe, so
//! unlike the `time` crate's `now_local()` it works in our multi-threaded
//! process with no startup capture or global state.

use std::time::SystemTime;

use time::{OffsetDateTime, UtcOffset};

/// Converts `time` to an [`OffsetDateTime`] in the local timezone in effect at
/// that instant, mirroring upstream `timestring()`. Falls back to UTC when the
/// offset is indeterminate (and on platforms without `localtime_r`).
pub(crate) fn to_local(time: SystemTime) -> OffsetDateTime {
    let datetime = OffsetDateTime::from(time);
    // `::platform` is the workspace crate (cli also has a local `mod platform`).
    let seconds = ::platform::local_time::local_utc_offset_seconds(datetime.unix_timestamp());
    UtcOffset::from_whole_seconds(seconds).map_or(datetime, |offset| datetime.to_offset(offset))
}
