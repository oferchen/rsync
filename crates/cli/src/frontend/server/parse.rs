//! Server argument parsing - compact flag strings and value conversions.

use std::ffi::{OsStr, OsString};
use std::time::SystemTime;

use super::flags::{
    compact_flag_string_expects_split_value, is_known_server_long_flag,
    is_two_arg_server_long_flag, split_at_end_of_options,
};

/// The lone `.` that stands for the remote working directory.
///
/// upstream: clientserver.c:303 - `server_options()` pushes it ahead of the
/// paths, and `main.c` `do_server_sender()` / `do_server_recv()` take `argv[0]`
/// as the directory to change into, so it is consumed there and never appears
/// as a transferred operand.
const CWD_PLACEHOLDER: &str = ".";

/// Parses the flag string and positional arguments from server-mode argument list.
///
/// Extracts the compact flag string (first arg starting with `-` that is not
/// a known long flag) and positional arguments (everything after the flag string
/// and optional `.` separator).
///
/// This mirrors popt's argument-arity model (upstream runs the same
/// `options.c` `long_options[]` popt parser on the server): a value-bearing
/// option's value is bound whether it travels inside the same token or in the
/// FOLLOWING argv token, so it never lands in `positional_args`. Two arms:
///
/// - Long `POPT_ARG_STRING` flags (`--copy-dest`, `--link-dest`,
///   `--compare-dest`, `--files-from`, `--backup-dir`, `--partial-dir`,
///   `--temp-dir`) that `server_options()` emits as two argv slots via
///   `safe_arg("", value)` - recognised by [`is_two_arg_server_long_flag`],
///   whose following value slot is skipped.
/// - The short `-e` (`POPT_ARG_STRING`, options.c:823) whose `.`-capability
///   value can split off the compact flag string when a remote shell
///   re-tokenizes the joined exec line - rebound by
///   [`compact_flag_string_expects_split_value`].
pub(super) fn parse_server_flag_string_and_args(args: &[OsString]) -> (String, Vec<OsString>) {
    // popt consumes a bare `--` and returns everything after it as operands,
    // never as flags (options.c:1491). Split first so the option scan below
    // cannot claim a path, and so the marker itself never reaches the path
    // list - upstream never delivers it to `link_stat()`, and neither may we.
    let (args, operands) = split_at_end_of_options(args);

    let mut flag_string = String::new();
    let mut positional_args = Vec::new();
    let mut found_flags = false;

    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        let arg_str = arg.to_string_lossy();

        // popt value-in-next-token arity: long `POPT_ARG_STRING` options that
        // server_options() emits as two argv slots (`--copy-dest`,
        // `--link-dest`, `--compare-dest`, `--files-from`, `--backup-dir`,
        // `--temp-dir`, `--partial-dir`). Checked BEFORE
        // `is_known_server_long_flag` so the following value slot is consumed
        // too, not just the flag name. Without this, `--copy-dest /alt . dest/`
        // lands `/alt` in positional_args[0] and `dest/` in positional_args[1],
        // so the receiver mkdir's the alt-dest basis (already exists) instead
        // of the actual destination root. The joined `--flag=value` form rides
        // in one token and is handled by `is_known_server_long_flag` below.
        if is_two_arg_server_long_flag(&arg_str) {
            idx += 2;
            continue;
        }

        if is_known_server_long_flag(&arg_str) {
            idx += 1;
            continue;
        }

        if !found_flags && arg_str.starts_with('-') {
            flag_string = arg_str.into_owned();
            found_flags = true;
            // popt value-in-next-token arity for the short `-e`
            // (`POPT_ARG_STRING`, options.c:823). When a remote shell
            // re-tokenized the joined `-...e.<caps>` exec line, `-e`'s
            // capability value became a separate argv token. Rebind it onto
            // the compact flag string so it is consumed as the `-e` value
            // (never a positional) and the downstream `ParsedServerFlags::parse`
            // reads the `.`-capability letters exactly as it does for the
            // never-split joined form - fixing both the path leak and the lost
            // compat negotiation in one place. See
            // `compact_flag_string_expects_split_value`.
            if compact_flag_string_expects_split_value(&flag_string) {
                if let Some(value) = args.get(idx + 1) {
                    flag_string.push_str(&value.to_string_lossy());
                    idx += 1;
                }
            }
            idx += 1;
            continue;
        }

        // upstream uses "." as a placeholder separator between flags and paths
        if found_flags && arg_str == CWD_PLACEHOLDER {
            idx += 1;
            continue;
        }

        if found_flags {
            positional_args.push(arg.clone());
        }
        idx += 1;
    }

    // Everything after the marker is a path, whatever it looks like - that is
    // the property `support/rrsync:558` relies on. The `.` placeholder is
    // filtered by the same rule the loop above applies, because rrsync emits it
    // AFTER the marker (`support/rrsync:655`) where upstream's own
    // `server_options()` emits it before.
    positional_args.extend(
        operands
            .iter()
            .filter(|operand| operand.as_os_str() != OsStr::new(CWD_PLACEHOLDER))
            .cloned(),
    );

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
