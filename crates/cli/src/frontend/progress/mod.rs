//! Progress and verbose output helpers extracted from the CLI front-end.

pub mod diagnostic;
mod format;
mod live;
mod mode;
mod render;

#[allow(unused_imports)] // REASON: convenience re-export; not all items used in every module
pub use self::diagnostic::{DiagnosticEvent, flush_diagnostics, render_diagnostic_events};
#[allow(unused_imports)] // REASON: convenience re-export; not all items used in every module
pub(crate) use self::format::{
    describe_event_kind, event_matches_name_level, format_decimal_bytes, format_human_bytes,
    format_human_rate, format_list_permissions, format_list_size, format_list_timestamp,
    format_progress_bytes, format_progress_elapsed, format_progress_percent, format_progress_rate,
    format_progress_rate_decimal, format_progress_rate_from_value, format_size,
    format_stat_categories, format_summary_rate, is_progress_event, list_only_event,
};
pub(crate) use self::live::{LiveProgress, ProgressOutputConfig};
pub(crate) use self::mode::ProgressMode;
pub use self::mode::{NameOutputLevel, ProgressSetting, StderrMode}; // Changed to pub for test_utils
#[cfg(test)]
pub(crate) use self::render::emit_list_only;
pub(crate) use self::render::emit_transfer_summary;
