//! Server argument parsing - compact flag strings and value conversions.

use std::ffi::{OsStr, OsString};
use std::time::SystemTime;

use super::flags::is_known_server_long_flag;

/// Parses the flag string and positional arguments from server-mode argument list.
///
/// Extracts the compact flag string (first arg starting with `-` that is not
/// a known long flag) and positional arguments (everything after the flag string
/// and optional `.` separator).
pub(super) fn parse_server_flag_string_and_args(args: &[OsString]) -> (String, Vec<OsString>) {
    let mut flag_string = String::new();
    let mut positional_args = Vec::new();
    let mut found_flags = false;

    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        let arg_str = arg.to_string_lossy();

        if is_known_server_long_flag(&arg_str) {
            // upstream: options.c:2886-2890 - `--partial-dir` is emitted as
            // TWO separate argv entries by server_options(), so the value
            // that immediately follows must also be skipped here. Without
            // this, the partial-dir VALUE leaks into the positional list and
            // becomes a destination-path argument.
            if arg_str == "--partial-dir" {
                idx += 2;
                continue;
            }
            idx += 1;
            continue;
        }

        if !found_flags && arg_str.starts_with('-') {
            flag_string = arg_str.into_owned();
            found_flags = true;
            idx += 1;
            continue;
        }

        // upstream uses "." as a placeholder separator between flags and paths
        if found_flags && arg_str == "." {
            idx += 1;
            continue;
        }

        if found_flags {
            positional_args.push(arg.clone());
        }
        idx += 1;
    }

    (flag_string, positional_args)
}

/// Parses a `--checksum-seed=NUM` value from the server argument list.
///
/// upstream: options.c - `--checksum-seed=NUM` parsed in `server_options()`.
pub(super) fn parse_server_checksum_seed(value: &str) -> Result<u32, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("--checksum-seed value must not be empty".to_owned());
    }
    trimmed.parse::<u32>().map_err(|_| {
        format!(
            "invalid --checksum-seed value '{value}': must be 0..{}",
            u32::MAX
        )
    })
}

/// Parses a `--min-size=SIZE` or `--max-size=SIZE` value from the server argument list.
///
/// Delegates to the shared size parser used by the client-side CLI.
/// upstream: options.c - `--min-size` / `--max-size` in `server_options()`.
pub(super) fn parse_server_size_limit(value: &str, flag: &str) -> Result<u64, String> {
    let os_value = OsStr::new(value);
    super::super::execution::parse_size_limit_argument(os_value, flag)
        .map_err(|msg| msg.to_string())
}

/// Parses a `--stop-at=WHEN` value from the server argument list.
///
/// Delegates to the shared stop-at parser.
/// upstream: options.c - `--stop-at` in `server_options()`.
pub(super) fn parse_server_stop_at(value: &str) -> Result<SystemTime, String> {
    let os_value = OsStr::new(value);
    super::super::execution::parse_stop_at_argument(os_value).map_err(|msg| msg.to_string())
}

/// Parses a `--stop-after=MINS` value from the server argument list.
///
/// Converts minutes to an absolute deadline (now + minutes).
/// upstream: options.c - `--stop-after` / `--time-limit` in `server_options()`.
pub(super) fn parse_server_stop_after(value: &str) -> Result<SystemTime, String> {
    let os_value = OsStr::new(value);
    super::super::execution::parse_stop_after_argument(os_value).map_err(|msg| msg.to_string())
}
