// Syslog backend for daemon-mode logging.
//
// Uses libc `openlog`/`syslog`/`closelog` directly rather than pulling in a
// dedicated syslog crate, keeping the dependency graph minimal. The
// implementation mirrors upstream rsync's `log.c` behaviour: when daemon mode
// is active, diagnostics are routed to syslog(3) with the configured facility
// and tag.
//
// upstream: log.c — `logit()` calls `syslog(priority, "%s", buf)` when
// `logfile_was_closed` is false and the daemon is running.

use std::ffi::CString;
use std::fmt;
use std::sync::OnceLock;

/// Syslog facility codes matching the POSIX syslog(3) constants.
///
/// Each variant corresponds to a `LOG_*` facility from `<syslog.h>`.
/// The daemon configuration maps string names (e.g., `"daemon"`, `"local0"`)
/// to these constants via [`SyslogFacility::from_name`].
///
/// upstream: `loadparm.c` — `lp_syslog_facility()` returns an integer facility
/// code parsed from the `syslog facility` configuration directive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum SyslogFacility {
    /// Kernel messages (LOG_KERN).
    Kern = libc::LOG_KERN,
    /// User-level messages (LOG_USER).
    User = libc::LOG_USER,
    /// Mail system (LOG_MAIL).
    Mail = libc::LOG_MAIL,
    /// System daemons (LOG_DAEMON) — the default for rsync daemon mode.
    Daemon = libc::LOG_DAEMON,
    /// Security/authorization messages (LOG_AUTH).
    Auth = libc::LOG_AUTH,
    /// Messages generated internally by syslogd (LOG_SYSLOG).
    Syslog = libc::LOG_SYSLOG,
    /// Line printer subsystem (LOG_LPR).
    Lpr = libc::LOG_LPR,
    /// Network news subsystem (LOG_NEWS).
    News = libc::LOG_NEWS,
    /// UUCP subsystem (LOG_UUCP).
    Uucp = libc::LOG_UUCP,
    /// Clock daemon (LOG_CRON).
    Cron = libc::LOG_CRON,
    /// Reserved for local use (LOG_LOCAL0).
    Local0 = libc::LOG_LOCAL0,
    /// Reserved for local use (LOG_LOCAL1).
    Local1 = libc::LOG_LOCAL1,
    /// Reserved for local use (LOG_LOCAL2).
    Local2 = libc::LOG_LOCAL2,
    /// Reserved for local use (LOG_LOCAL3).
    Local3 = libc::LOG_LOCAL3,
    /// Reserved for local use (LOG_LOCAL4).
    Local4 = libc::LOG_LOCAL4,
    /// Reserved for local use (LOG_LOCAL5).
    Local5 = libc::LOG_LOCAL5,
    /// Reserved for local use (LOG_LOCAL6).
    Local6 = libc::LOG_LOCAL6,
    /// Reserved for local use (LOG_LOCAL7).
    Local7 = libc::LOG_LOCAL7,
}

impl SyslogFacility {
    /// Returns the default syslog facility for the daemon.
    ///
    /// Upstream rsync defaults to `LOG_DAEMON` when no `syslog facility`
    /// directive appears in `rsyncd.conf`.
    pub const fn default_daemon() -> Self {
        Self::Daemon
    }

    /// Parses a facility name string into the corresponding constant.
    ///
    /// Recognised names are case-insensitive and match the values accepted by
    /// upstream rsync's `syslog facility` configuration directive.
    ///
    /// Returns `None` for unrecognised names.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(unix)]
    /// # {
    /// use logging_sink::syslog::SyslogFacility;
    ///
    /// assert_eq!(
    ///     SyslogFacility::from_name("daemon"),
    ///     Some(SyslogFacility::Daemon)
    /// );
    /// assert_eq!(
    ///     SyslogFacility::from_name("LOCAL3"),
    ///     Some(SyslogFacility::Local3)
    /// );
    /// assert_eq!(SyslogFacility::from_name("unknown"), None);
    /// # }
    /// ```
    pub fn from_name(name: &str) -> Option<Self> {
        // upstream: loadparm.c — facility_names[] lookup table
        match name.to_ascii_lowercase().as_str() {
            "kern" => Some(Self::Kern),
            "user" => Some(Self::User),
            "mail" => Some(Self::Mail),
            "daemon" => Some(Self::Daemon),
            "auth" => Some(Self::Auth),
            "syslog" => Some(Self::Syslog),
            "lpr" => Some(Self::Lpr),
            "news" => Some(Self::News),
            "uucp" => Some(Self::Uucp),
            "cron" => Some(Self::Cron),
            "local0" => Some(Self::Local0),
            "local1" => Some(Self::Local1),
            "local2" => Some(Self::Local2),
            "local3" => Some(Self::Local3),
            "local4" => Some(Self::Local4),
            "local5" => Some(Self::Local5),
            "local6" => Some(Self::Local6),
            "local7" => Some(Self::Local7),
            _ => None,
        }
    }

    /// Returns the facility name as it would appear in `rsyncd.conf`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Kern => "kern",
            Self::User => "user",
            Self::Mail => "mail",
            Self::Daemon => "daemon",
            Self::Auth => "auth",
            Self::Syslog => "syslog",
            Self::Lpr => "lpr",
            Self::News => "news",
            Self::Uucp => "uucp",
            Self::Cron => "cron",
            Self::Local0 => "local0",
            Self::Local1 => "local1",
            Self::Local2 => "local2",
            Self::Local3 => "local3",
            Self::Local4 => "local4",
            Self::Local5 => "local5",
            Self::Local6 => "local6",
            Self::Local7 => "local7",
        }
    }
}

impl Default for SyslogFacility {
    fn default() -> Self {
        Self::default_daemon()
    }
}

impl fmt::Display for SyslogFacility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Default syslog tag for the daemon process.
///
/// Upstream rsync uses `"rsyncd"`; oc-rsync uses `"oc-rsyncd"` to avoid
/// colliding with upstream log entries on systems running both implementations.
pub const DEFAULT_SYSLOG_TAG: &str = "oc-rsyncd";

/// Configuration for syslog-based logging in daemon mode.
///
/// Encapsulates the facility and tag (ident) parameters passed to
/// [`openlog(3)`](libc::openlog). Constructing a [`SyslogConfig`] does not
/// itself open the syslog connection; call [`open`](SyslogConfig::open) to
/// begin routing messages.
///
/// # Examples
///
/// ```
/// # #[cfg(unix)]
/// # {
/// use logging_sink::syslog::{SyslogConfig, SyslogFacility};
///
/// let config = SyslogConfig::new(SyslogFacility::Local5, "my-daemon");
/// assert_eq!(config.facility(), SyslogFacility::Local5);
/// assert_eq!(config.tag(), "my-daemon");
/// # }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyslogConfig {
    facility: SyslogFacility,
    tag: String,
}

impl SyslogConfig {
    /// Creates a new syslog configuration with the given facility and tag.
    pub fn new(facility: SyslogFacility, tag: impl Into<String>) -> Self {
        Self {
            facility,
            tag: tag.into(),
        }
    }

    /// Returns the configured syslog facility.
    pub const fn facility(&self) -> SyslogFacility {
        self.facility
    }

    /// Returns the configured syslog tag (ident string).
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// Opens the syslog connection with the configured facility and tag.
    ///
    /// Returns a [`SyslogGuard`] that closes the connection when dropped.
    /// Only one syslog connection should be active at a time per process.
    ///
    /// # Safety
    ///
    /// This function calls `libc::openlog` which is not thread-safe with
    /// respect to concurrent `openlog`/`closelog` calls. The caller must
    /// ensure no other thread is opening or closing syslog simultaneously.
    /// In practice the daemon opens syslog once at startup before spawning
    /// worker threads, matching upstream rsync's single-threaded init pattern.
    pub fn open(&self) -> SyslogGuard {
        // The CString must outlive the openlog call because syslog(3) stores
        // the pointer internally. We leak the allocation intentionally and
        // store it in a static so closelog() can find a valid pointer. This
        // matches the process-lifetime semantics of syslog's ident parameter.
        static IDENT: OnceLock<CString> = OnceLock::new();
        let ident = IDENT.get_or_init(|| {
            CString::new(self.tag.as_str()).unwrap_or_else(|_| {
                CString::new(DEFAULT_SYSLOG_TAG).expect("default tag contains no NUL bytes")
            })
        });

        // upstream: log.c — openlog(tag, LOG_PID, facility)
        // LOG_PID includes the PID in each message, matching upstream behaviour.
        //
        // SAFETY: openlog is called once at daemon startup before worker threads
        // are spawned. The ident pointer is valid for the process lifetime because
        // it is stored in a static `OnceLock<CString>`.
        unsafe {
            libc::openlog(ident.as_ptr(), libc::LOG_PID, self.facility as libc::c_int);
        }

        SyslogGuard { _private: () }
    }
}

impl Default for SyslogConfig {
    fn default() -> Self {
        Self::new(SyslogFacility::default_daemon(), DEFAULT_SYSLOG_TAG)
    }
}

/// Syslog priority levels matching POSIX syslog(3) severity constants.
///
/// Used by [`syslog_message`] to set the severity of each log entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum SyslogPriority {
    /// System is unusable (LOG_EMERG).
    Emergency = libc::LOG_EMERG,
    /// Action must be taken immediately (LOG_ALERT).
    Alert = libc::LOG_ALERT,
    /// Critical conditions (LOG_CRIT).
    Critical = libc::LOG_CRIT,
    /// Error conditions (LOG_ERR).
    Error = libc::LOG_ERR,
    /// Warning conditions (LOG_WARNING).
    Warning = libc::LOG_WARNING,
    /// Normal but significant condition (LOG_NOTICE).
    Notice = libc::LOG_NOTICE,
    /// Informational messages (LOG_INFO).
    Info = libc::LOG_INFO,
    /// Debug-level messages (LOG_DEBUG).
    Debug = libc::LOG_DEBUG,
}

/// Sends a message to syslog(3) with the given priority.
///
/// The message is sent using the facility configured by the most recent
/// [`SyslogConfig::open`] call. The caller is responsible for ensuring that
/// syslog has been opened (via [`SyslogConfig::open`]) before calling this
/// function.
///
/// # Safety
///
/// This function calls `libc::syslog` which requires that `openlog` has been
/// called previously. The [`SyslogGuard`] returned by [`SyslogConfig::open`]
/// guarantees this invariant while it is alive.
pub fn syslog_message(priority: SyslogPriority, message: &str) {
    // syslog(3) interprets `%` as a format specifier. Using `%s` with the
    // message as an argument avoids accidental format string injection.
    // upstream: log.c — `syslog(priority, "%s", buf)`
    let c_message = match CString::new(message) {
        Ok(s) => s,
        Err(_) => return,
    };
    let format = c_str_literal(b"%s\0");

    // SAFETY: syslog is safe to call from multiple threads after openlog
    // has completed. The format string and message are valid C strings.
    unsafe {
        libc::syslog(priority as libc::c_int, format, c_message.as_ptr());
    }
}

/// Returns a pointer to a static C string literal.
///
/// The input must be a NUL-terminated byte slice.
const fn c_str_literal(bytes: &[u8]) -> *const libc::c_char {
    bytes.as_ptr().cast::<libc::c_char>()
}

/// RAII guard that closes the syslog connection when dropped.
///
/// Created by [`SyslogConfig::open`]. While this guard is alive, calls to
/// [`syslog_message`] will route to the configured syslog facility. Dropping
/// the guard calls `closelog(3)`.
///
/// # Examples
///
/// ```no_run
/// # #[cfg(unix)]
/// # {
/// use logging_sink::syslog::{SyslogConfig, SyslogFacility, SyslogPriority, syslog_message};
///
/// let config = SyslogConfig::new(SyslogFacility::Daemon, "oc-rsyncd");
/// let _guard = config.open();
///
/// syslog_message(SyslogPriority::Info, "daemon started");
/// // guard dropped here, closelog() called
/// # }
/// ```
#[derive(Debug)]
pub struct SyslogGuard {
    _private: (),
}

impl Drop for SyslogGuard {
    fn drop(&mut self) {
        // SAFETY: closelog is safe to call and has no preconditions beyond
        // openlog having been called previously, which is guaranteed by the
        // guard construction in SyslogConfig::open.
        unsafe {
            libc::closelog();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- SyslogFacility tests ---

    #[test]
    fn default_facility_is_daemon() {
        assert_eq!(SyslogFacility::default(), SyslogFacility::Daemon);
        assert_eq!(SyslogFacility::default_daemon(), SyslogFacility::Daemon);
    }

    #[test]
    fn from_name_recognises_all_standard_facilities() {
        let cases = [
            ("kern", SyslogFacility::Kern),
            ("user", SyslogFacility::User),
            ("mail", SyslogFacility::Mail),
            ("daemon", SyslogFacility::Daemon),
            ("auth", SyslogFacility::Auth),
            ("syslog", SyslogFacility::Syslog),
            ("lpr", SyslogFacility::Lpr),
            ("news", SyslogFacility::News),
            ("uucp", SyslogFacility::Uucp),
            ("cron", SyslogFacility::Cron),
            ("local0", SyslogFacility::Local0),
            ("local1", SyslogFacility::Local1),
            ("local2", SyslogFacility::Local2),
            ("local3", SyslogFacility::Local3),
            ("local4", SyslogFacility::Local4),
            ("local5", SyslogFacility::Local5),
            ("local6", SyslogFacility::Local6),
            ("local7", SyslogFacility::Local7),
        ];

        for (name, expected) in &cases {
            assert_eq!(
                SyslogFacility::from_name(name),
                Some(*expected),
                "failed for facility name '{name}'"
            );
        }
    }

    #[test]
    fn from_name_is_case_insensitive() {
        assert_eq!(
            SyslogFacility::from_name("DAEMON"),
            Some(SyslogFacility::Daemon)
        );
        assert_eq!(
            SyslogFacility::from_name("Daemon"),
            Some(SyslogFacility::Daemon)
        );
        assert_eq!(
            SyslogFacility::from_name("LOCAL7"),
            Some(SyslogFacility::Local7)
        );
        assert_eq!(
            SyslogFacility::from_name("Local0"),
            Some(SyslogFacility::Local0)
        );
    }

    #[test]
    fn from_name_rejects_unknown() {
        assert_eq!(SyslogFacility::from_name("unknown"), None);
        assert_eq!(SyslogFacility::from_name(""), None);
        assert_eq!(SyslogFacility::from_name("local8"), None);
        assert_eq!(SyslogFacility::from_name("LOG_DAEMON"), None);
    }

    #[test]
    fn as_str_round_trips_with_from_name() {
        let facilities = [
            SyslogFacility::Kern,
            SyslogFacility::User,
            SyslogFacility::Mail,
            SyslogFacility::Daemon,
            SyslogFacility::Auth,
            SyslogFacility::Syslog,
            SyslogFacility::Lpr,
            SyslogFacility::News,
            SyslogFacility::Uucp,
            SyslogFacility::Cron,
            SyslogFacility::Local0,
            SyslogFacility::Local1,
            SyslogFacility::Local2,
            SyslogFacility::Local3,
            SyslogFacility::Local4,
            SyslogFacility::Local5,
            SyslogFacility::Local6,
            SyslogFacility::Local7,
        ];

        for facility in &facilities {
            let name = facility.as_str();
            let parsed = SyslogFacility::from_name(name);
            assert_eq!(
                parsed,
                Some(*facility),
                "round-trip failed for {facility:?} (name={name})"
            );
        }
    }

    #[test]
    fn display_matches_as_str() {
        let facility = SyslogFacility::Local3;
        assert_eq!(format!("{facility}"), facility.as_str());
        assert_eq!(format!("{facility}"), "local3");
    }

    #[test]
    fn facility_values_match_libc_constants() {
        assert_eq!(SyslogFacility::Kern as i32, libc::LOG_KERN);
        assert_eq!(SyslogFacility::User as i32, libc::LOG_USER);
        assert_eq!(SyslogFacility::Daemon as i32, libc::LOG_DAEMON);
        assert_eq!(SyslogFacility::Local0 as i32, libc::LOG_LOCAL0);
        assert_eq!(SyslogFacility::Local7 as i32, libc::LOG_LOCAL7);
    }

    // --- SyslogConfig tests ---

    #[test]
    fn config_default_uses_daemon_facility_and_default_tag() {
        let config = SyslogConfig::default();
        assert_eq!(config.facility(), SyslogFacility::Daemon);
        assert_eq!(config.tag(), DEFAULT_SYSLOG_TAG);
    }

    #[test]
    fn config_new_stores_facility_and_tag() {
        let config = SyslogConfig::new(SyslogFacility::Local5, "my-daemon");
        assert_eq!(config.facility(), SyslogFacility::Local5);
        assert_eq!(config.tag(), "my-daemon");
    }

    #[test]
    fn config_accepts_string_tag() {
        let tag = String::from("custom-tag");
        let config = SyslogConfig::new(SyslogFacility::Auth, tag);
        assert_eq!(config.tag(), "custom-tag");
    }

    #[test]
    fn config_clone_preserves_values() {
        let config = SyslogConfig::new(SyslogFacility::Local2, "test-tag");
        let cloned = config.clone();
        assert_eq!(config, cloned);
    }

    #[test]
    fn config_debug_format() {
        let config = SyslogConfig::default();
        let debug = format!("{config:?}");
        assert!(debug.contains("SyslogConfig"));
        assert!(debug.contains("Daemon"));
        assert!(debug.contains(DEFAULT_SYSLOG_TAG));
    }

    // --- SyslogConfig::open tests ---

    #[test]
    fn open_does_not_panic_with_default_config() {
        let config = SyslogConfig::default();
        let _guard = config.open();
    }

    #[test]
    fn open_does_not_panic_with_custom_facility() {
        let config = SyslogConfig::new(SyslogFacility::Local7, "test-syslog");
        let _guard = config.open();
    }

    #[test]
    fn open_does_not_panic_with_empty_tag() {
        let config = SyslogConfig::new(SyslogFacility::Daemon, "");
        let _guard = config.open();
    }

    // --- SyslogPriority tests ---

    #[test]
    fn priority_values_match_libc_constants() {
        assert_eq!(SyslogPriority::Emergency as i32, libc::LOG_EMERG);
        assert_eq!(SyslogPriority::Alert as i32, libc::LOG_ALERT);
        assert_eq!(SyslogPriority::Critical as i32, libc::LOG_CRIT);
        assert_eq!(SyslogPriority::Error as i32, libc::LOG_ERR);
        assert_eq!(SyslogPriority::Warning as i32, libc::LOG_WARNING);
        assert_eq!(SyslogPriority::Notice as i32, libc::LOG_NOTICE);
        assert_eq!(SyslogPriority::Info as i32, libc::LOG_INFO);
        assert_eq!(SyslogPriority::Debug as i32, libc::LOG_DEBUG);
    }

    // --- syslog_message tests ---

    #[test]
    fn syslog_message_does_not_panic_after_open() {
        let config = SyslogConfig::default();
        let _guard = config.open();
        syslog_message(SyslogPriority::Info, "test message from oc-rsync tests");
    }

    #[test]
    fn syslog_message_handles_empty_string() {
        let config = SyslogConfig::default();
        let _guard = config.open();
        syslog_message(SyslogPriority::Debug, "");
    }

    #[test]
    fn syslog_message_handles_special_characters() {
        let config = SyslogConfig::default();
        let _guard = config.open();
        syslog_message(
            SyslogPriority::Warning,
            "path with spaces & symbols: /tmp/a b/c%d",
        );
    }

    #[test]
    fn syslog_message_handles_nul_bytes_gracefully() {
        let config = SyslogConfig::default();
        let _guard = config.open();
        // CString::new will fail on embedded NUL, syslog_message returns early
        syslog_message(SyslogPriority::Info, "before\0after");
    }

    // --- SyslogGuard tests ---

    #[test]
    fn guard_debug_format() {
        let config = SyslogConfig::default();
        let guard = config.open();
        let debug = format!("{guard:?}");
        assert!(debug.contains("SyslogGuard"));
    }
}
