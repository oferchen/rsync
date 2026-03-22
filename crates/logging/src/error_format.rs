//! Upstream-compatible error and warning formatting.
//!
//! Upstream rsync emits diagnostics in a fixed format that external tools and
//! scripts depend on for parsing:
//!
//! ```text
//! rsync error: <text> (code N) at <file>:<line> [<role>=<version>]
//! rsync warning: <text> (code N) at <file>:<line> [<role>=<version>]
//! ```
//!
//! Reference: upstream `log.c:rwrite()`, `errcode.h` (rsync 3.4.1).
//!
//! The [`rsync_error_fmt!`] and [`rsync_warning_fmt!`] macros produce strings
//! in this format. They capture `file!()` and `line!()` at the call site, strip
//! the repo prefix to yield a `crates/`-relative path, and embed the caller's
//! `CARGO_PKG_VERSION` as the version.

/// Strips the repository prefix from a compile-time file path.
///
/// Given a `CARGO_MANIFEST_DIR` like `/home/user/project/crates/logging` and a
/// `file!()` path like `/home/user/project/crates/logging/src/lib.rs`, this
/// returns `logging/src/lib.rs` - the path relative to the `crates/` directory.
///
/// If the path does not contain `crates/`, the full `file!()` value is returned
/// unchanged.
#[must_use]
pub fn strip_repo_prefix<'a>(manifest_dir: &str, file_path: &'a str) -> &'a str {
    // Find the `crates/` boundary in the manifest dir to determine where the
    // crate-relative portion starts. The manifest dir for a crate at
    // `<repo>/crates/logging` ends with `crates/logging`, so we locate
    // `crates/` and use the offset after it as the repo prefix length.
    if let Some(crates_pos) = find_crates_prefix(manifest_dir) {
        let repo_prefix = &manifest_dir[..crates_pos];
        // file_path typically starts with the same repo prefix
        if let Some(stripped) = file_path.strip_prefix(repo_prefix) {
            return stripped;
        }
    }

    // Fallback: try to find `crates/` directly in file_path
    if let Some(pos) = find_crates_prefix(file_path) {
        return &file_path[pos..];
    }

    file_path
}

/// Locates the byte offset of the `crates/` segment in a path.
fn find_crates_prefix(path: &str) -> Option<usize> {
    let needle = "crates/";
    path.find(needle)
}

/// Formats an upstream-compatible rsync error string.
///
/// This is the function backing [`rsync_error_fmt!`]. Prefer the macro, which
/// captures `file!()` and `line!()` automatically.
///
/// # Format
///
/// ```text
/// rsync error: <message> (code <exit_code>) at <rel_path>:<line> [<role>=<version>]
/// ```
#[must_use]
pub fn format_rsync_error(
    message: &str,
    exit_code: i32,
    manifest_dir: &str,
    file_path: &str,
    line: u32,
    role: &str,
    version: &str,
) -> String {
    let rel_path = strip_repo_prefix(manifest_dir, file_path);
    format!("rsync error: {message} (code {exit_code}) at {rel_path}:{line} [{role}={version}]")
}

/// Formats an upstream-compatible rsync warning string.
///
/// This is the function backing [`rsync_warning_fmt!`]. Prefer the macro, which
/// captures `file!()` and `line!()` automatically.
///
/// # Format
///
/// ```text
/// rsync warning: <message> (code <exit_code>) at <rel_path>:<line> [<role>=<version>]
/// ```
#[must_use]
pub fn format_rsync_warning(
    message: &str,
    exit_code: i32,
    manifest_dir: &str,
    file_path: &str,
    line: u32,
    role: &str,
    version: &str,
) -> String {
    let rel_path = strip_repo_prefix(manifest_dir, file_path);
    format!("rsync warning: {message} (code {exit_code}) at {rel_path}:{line} [{role}={version}]")
}

/// Produces an upstream-compatible rsync error string.
///
/// Captures `file!()` and `line!()` at the call site, strips the repo prefix
/// from the path (everything up to and including `crates/`), and formats the
/// result using the upstream pattern.
///
/// # Format
///
/// ```text
/// rsync error: <message> (code <exit_code>) at <rel_path>:<line> [<role>=<version>]
/// ```
///
/// # Arguments
///
/// - `$exit_code` - Numeric exit code matching upstream `errcode.h`
/// - `$role` - Role string: `"sender"`, `"receiver"`, `"generator"`, `"server"`,
///   `"client"`, or `"daemon"`
/// - `$msg` - Error message (literal or format string with arguments)
///
/// # Examples
///
/// ```
/// use logging::rsync_error_fmt;
///
/// let s = rsync_error_fmt!(23, "sender", "partial transfer");
/// assert!(s.starts_with("rsync error: partial transfer (code 23) at "));
/// assert!(s.contains("[sender="));
/// assert!(s.ends_with("]"));
/// ```
///
/// With format arguments:
///
/// ```
/// use logging::rsync_error_fmt;
///
/// let path = "/tmp/foo";
/// let s = rsync_error_fmt!(11, "receiver", "read error: {}", path);
/// assert!(s.contains("read error: /tmp/foo"));
/// assert!(s.contains("(code 11)"));
/// ```
#[macro_export]
macro_rules! rsync_error_fmt {
    ($exit_code:expr, $role:expr, $($msg:tt)+) => {
        $crate::error_format::format_rsync_error(
            &format!($($msg)+),
            $exit_code,
            env!("CARGO_MANIFEST_DIR"),
            file!(),
            line!(),
            $role,
            env!("CARGO_PKG_VERSION"),
        )
    };
}

/// Produces an upstream-compatible rsync warning string.
///
/// Behaves identically to [`rsync_error_fmt!`] but uses the `rsync warning:`
/// prefix instead of `rsync error:`.
///
/// # Format
///
/// ```text
/// rsync warning: <message> (code <exit_code>) at <rel_path>:<line> [<role>=<version>]
/// ```
///
/// # Examples
///
/// ```
/// use logging::rsync_warning_fmt;
///
/// let s = rsync_warning_fmt!(24, "sender", "some files vanished");
/// assert!(s.starts_with("rsync warning: some files vanished (code 24) at "));
/// assert!(s.contains("[sender="));
/// assert!(s.ends_with("]"));
/// ```
#[macro_export]
macro_rules! rsync_warning_fmt {
    ($exit_code:expr, $role:expr, $($msg:tt)+) => {
        $crate::error_format::format_rsync_warning(
            &format!($($msg)+),
            $exit_code,
            env!("CARGO_MANIFEST_DIR"),
            file!(),
            line!(),
            $role,
            env!("CARGO_PKG_VERSION"),
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- strip_repo_prefix tests --

    #[test]
    fn strips_prefix_when_crates_present() {
        let manifest = "/home/user/project/crates/logging";
        let file = "/home/user/project/crates/logging/src/lib.rs";
        assert_eq!(
            strip_repo_prefix(manifest, file),
            "crates/logging/src/lib.rs"
        );
    }

    #[test]
    fn strips_prefix_different_crate() {
        let manifest = "/repo/crates/protocol";
        let file = "/repo/crates/protocol/src/wire.rs";
        assert_eq!(
            strip_repo_prefix(manifest, file),
            "crates/protocol/src/wire.rs"
        );
    }

    #[test]
    fn returns_full_path_when_no_crates() {
        let manifest = "/tmp/standalone";
        let file = "/tmp/standalone/src/main.rs";
        assert_eq!(
            strip_repo_prefix(manifest, file),
            "/tmp/standalone/src/main.rs"
        );
    }

    #[test]
    fn handles_file_path_with_crates_directly() {
        let manifest = "/other/path";
        let file = "/some/repo/crates/engine/src/delta.rs";
        assert_eq!(
            strip_repo_prefix(manifest, file),
            "crates/engine/src/delta.rs"
        );
    }

    #[test]
    fn handles_windows_style_paths() {
        let manifest = "C:\\Users\\dev\\project\\crates\\logging";
        let file = "crates/logging/src/lib.rs";
        // file_path uses forward slashes via file!(), fallback to crates/ search
        assert_eq!(
            strip_repo_prefix(manifest, file),
            "crates/logging/src/lib.rs"
        );
    }

    // -- format_rsync_error tests --

    #[test]
    fn error_format_matches_upstream() {
        let result = format_rsync_error(
            "some files/attrs were not transferred",
            23,
            "/repo/crates/core",
            "/repo/crates/core/src/transfer.rs",
            42,
            "sender",
            "0.5.9",
        );
        assert_eq!(
            result,
            "rsync error: some files/attrs were not transferred (code 23) at crates/core/src/transfer.rs:42 [sender=0.5.9]"
        );
    }

    #[test]
    fn error_format_with_different_role() {
        let result = format_rsync_error(
            "read errors",
            11,
            "/repo/crates/engine",
            "/repo/crates/engine/src/delta.rs",
            100,
            "receiver",
            "0.5.9",
        );
        assert_eq!(
            result,
            "rsync error: read errors (code 11) at crates/engine/src/delta.rs:100 [receiver=0.5.9]"
        );
    }

    #[test]
    fn error_format_daemon_role() {
        let result = format_rsync_error(
            "auth failed",
            5,
            "/repo/crates/core",
            "/repo/crates/core/src/daemon.rs",
            7,
            "daemon",
            "0.5.9",
        );
        assert!(result.starts_with("rsync error: auth failed (code 5)"));
        assert!(result.ends_with("[daemon=0.5.9]"));
    }

    #[test]
    fn error_code_zero() {
        let result = format_rsync_error(
            "success",
            0,
            "/repo/crates/core",
            "/repo/crates/core/src/lib.rs",
            1,
            "client",
            "1.0.0",
        );
        assert!(result.contains("(code 0)"));
        assert!(result.contains("[client=1.0.0]"));
    }

    // -- format_rsync_warning tests --

    #[test]
    fn warning_format_matches_upstream() {
        let result = format_rsync_warning(
            "some files vanished before they could be transferred",
            24,
            "/repo/crates/core",
            "/repo/crates/core/src/transfer.rs",
            55,
            "sender",
            "0.5.9",
        );
        assert_eq!(
            result,
            "rsync warning: some files vanished before they could be transferred (code 24) at crates/core/src/transfer.rs:55 [sender=0.5.9]"
        );
    }

    #[test]
    fn warning_uses_warning_prefix() {
        let result = format_rsync_warning(
            "test",
            1,
            "/repo/crates/logging",
            "/repo/crates/logging/src/lib.rs",
            10,
            "server",
            "0.5.9",
        );
        assert!(result.starts_with("rsync warning: "));
        assert!(!result.starts_with("rsync error: "));
    }

    // -- macro integration tests --

    #[test]
    fn rsync_error_fmt_macro_captures_location() {
        let s = rsync_error_fmt!(23, "sender", "partial transfer");
        assert!(s.starts_with("rsync error: partial transfer (code 23) at "));
        assert!(s.contains("error_format.rs:"));
        assert!(s.contains("[sender="));
        assert!(s.ends_with("]"));
    }

    #[test]
    fn rsync_error_fmt_macro_with_format_args() {
        let path = "/tmp/foo";
        let s = rsync_error_fmt!(11, "receiver", "failed to read {}", path);
        assert!(s.contains("failed to read /tmp/foo"));
        assert!(s.contains("(code 11)"));
        assert!(s.contains("[receiver="));
    }

    #[test]
    fn rsync_warning_fmt_macro_captures_location() {
        let s = rsync_warning_fmt!(24, "sender", "files vanished");
        assert!(s.starts_with("rsync warning: files vanished (code 24) at "));
        assert!(s.contains("error_format.rs:"));
        assert!(s.contains("[sender="));
        assert!(s.ends_with("]"));
    }

    #[test]
    fn rsync_warning_fmt_macro_with_format_args() {
        let count = 3;
        let s = rsync_warning_fmt!(24, "generator", "{} files vanished", count);
        assert!(s.contains("3 files vanished"));
        assert!(s.contains("(code 24)"));
        assert!(s.contains("[generator="));
    }

    #[test]
    fn macro_version_matches_cargo_pkg_version() {
        let s = rsync_error_fmt!(1, "client", "test");
        let version = env!("CARGO_PKG_VERSION");
        let expected_trailer = format!("[client={version}]");
        assert!(
            s.ends_with(&expected_trailer),
            "expected trailer {expected_trailer}, got: {s}"
        );
    }

    #[test]
    fn macro_path_contains_crates_prefix() {
        let s = rsync_error_fmt!(1, "server", "test");
        // file!() for this test is inside crates/logging/src/error_format.rs
        assert!(
            s.contains("crates/"),
            "path should be crates/-relative, got: {s}"
        );
    }

    /// Verifies the full format matches the upstream pattern:
    /// `rsync error: <text> (code N) at <file>:<line> [<role>=<version>]`
    #[test]
    fn full_format_structure_matches_upstream_pattern() {
        let s = rsync_error_fmt!(23, "sender", "some files/attrs were not transferred");
        // Parse the structure
        assert!(s.starts_with("rsync error: "));
        let after_prefix = &s["rsync error: ".len()..];

        // Find "(code N)"
        let code_start = after_prefix.find("(code ").expect("missing (code N)");
        let code_end = after_prefix[code_start..].find(')').expect("missing )") + code_start;
        let code_section = &after_prefix[code_start..=code_end];
        assert_eq!(code_section, "(code 23)");

        // Find " at "
        let at_pos = after_prefix.find(" at ").expect("missing ' at '");
        assert!(at_pos > code_end - 1, "' at ' should follow (code N)");

        // Find "[role=version]"
        let bracket_start = after_prefix.find('[').expect("missing [");
        let bracket_end = after_prefix.find(']').expect("missing ]");
        let trailer = &after_prefix[bracket_start..=bracket_end];
        assert!(trailer.starts_with("[sender="));
        assert!(trailer.ends_with(']'));

        // Between " at " and "[" should be "file:line "
        let location = &after_prefix[at_pos + 4..bracket_start];
        assert!(location.contains(':'), "location should contain file:line");
    }
}
