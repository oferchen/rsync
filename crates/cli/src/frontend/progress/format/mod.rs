//! Formatting utilities for progress, file-list, rate, and event output.

mod event;
mod list;
mod progress;
mod rate;
mod remaining;
mod size;

pub(crate) use self::event::{describe_event_kind, event_matches_name_level, is_progress_event};
pub(crate) use self::list::{format_list_permissions, format_list_timestamp, list_only_event};
pub(crate) use self::progress::{
    format_progress_elapsed, format_progress_percent, format_stat_categories,
};
pub(crate) use self::rate::{
    format_human_rate, format_progress_rate, format_progress_rate_decimal,
    format_progress_rate_from_value, format_progress_rate_human, format_summary_rate,
    format_verbose_rate_human,
};
pub(crate) use self::remaining::RemainingTimeEstimator;
pub(crate) use self::size::{
    LIST_SIZE_WIDTH, format_decimal_bytes, format_human_bytes, format_list_size,
    format_progress_bytes, format_size,
};
