//! Progress and verbose output helpers extracted from the CLI front-end.

mod format;
mod live;
mod mode;
mod render;

#[allow(unused_imports)]
pub(crate) use self::format::{
    VerboseRateDisplay, describe_event_kind, event_matches_name_level, format_decimal_bytes,
    format_human_bytes, format_human_rate, format_list_permissions, format_list_size,
    format_list_timestamp, format_progress_bytes, format_progress_elapsed, format_progress_percent,
    format_progress_rate, format_progress_rate_decimal, format_progress_rate_human, format_size,
    format_stat_categories, format_summary_rate, format_verbose_rate, format_verbose_rate_decimal,
    format_verbose_rate_human, is_progress_event, list_only_event,
};
pub(crate) use self::live::LiveProgress;
pub use self::mode::{NameOutputLevel, ProgressSetting};  // Changed to pub for test_utils
pub(crate) use self::mode::ProgressMode;
pub(crate) use self::render::emit_transfer_summary;
