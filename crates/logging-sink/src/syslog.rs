//! Syslog backend for daemon-mode logging.
//!
//! Routes daemon diagnostics through the safe `syslog` crate, which speaks the
//! BSD/RFC 3164 wire protocol over a Unix socket (`/dev/log`, falling back to
//! `/var/run/syslog` on macOS). No `libc::openlog`/`syslog`/`closelog` FFI is
//! involved, so the crate continues to satisfy `#![deny(unsafe_code)]`.
//!
//! The implementation mirrors upstream rsync's `log.c` behaviour: when daemon
//! mode is active, diagnostics are sent to syslog with the configured facility
//! and tag.
//!
//! upstream: log.c - `logit()` calls `syslog(priority, "%s", buf)` when
//! `logfile_was_closed` is false and the daemon is running.

use std::fmt;
use std::process;
use std::sync::{Mutex, OnceLock};

use syslog::{Facility, Formatter3164, LoggerBackend};

/// Type alias for the concrete `syslog` crate logger we hold open.
type BsdLogger = syslog::Logger<LoggerBackend, Formatter3164>;

/// Process-wide handle to the currently open syslog connection.
///
/// `SyslogConfig::open` populates this slot; dropping the returned
/// [`SyslogGuard`] clears it, releasing the underlying socket. While the slot
/// is populated, [`syslog_message`] routes diagnostics through it.
fn logger_slot() -> &'static Mutex<Option<BsdLogger>> {
    static SLOT: OnceLock<Mutex<Option<BsdLogger>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Syslog facility codes matching the POSIX syslog(3) constants.
///
/// Each variant corresponds to a `LOG_*` facility from `<syslog.h>`. The
/// daemon configuration maps string names (e.g., `"daemon"`, `"local0"`) to
/// these constants via [`SyslogFacility::from_name`].
///
/// upstream: `loadparm.c` - `lp_syslog_facility()` returns an integer facility
/// code parsed from the `syslog facility` configuration directive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyslogFacility {
    /// Kernel messages (LOG_KERN).
    Kern,
    /// User-level messages (LOG_USER).
    User,
    /// Mail system (LOG_MAIL).
    Mail,
    /// System daemons (LOG_DAEMON) - the default for rsync daemon mode.
    Daemon,
    /// Security/authorization messages (LOG_AUTH).
    Auth,
    /// Messages generated internally by syslogd (LOG_SYSLOG).
    Syslog,
    /// Line printer subsystem (LOG_LPR).
    Lpr,
    /// Network news subsystem (LOG_NEWS).
    News,
    /// UUCP subsystem (LOG_UUCP).
    Uucp,
    /// Clock daemon (LOG_CRON).
    Cron,
    /// Reserved for local use (LOG_LOCAL0).
    Local0,
    /// Reserved for local use (LOG_LOCAL1).
    Local1,
    /// Reserved for local use (LOG_LOCAL2).
    Local2,
    /// Reserved for local use (LOG_LOCAL3).
    Local3,
    /// Reserved for local use (LOG_LOCAL4).
    Local4,
    /// Reserved for local use (LOG_LOCAL5).
    Local5,
    /// Reserved for local use (LOG_LOCAL6).
    Local6,
    /// Reserved for local use (LOG_LOCAL7).
    Local7,
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
        // upstream: loadparm.c - facility_names[] lookup table
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

    /// Maps to the corresponding [`syslog::Facility`] used on the wire.
    const fn to_wire(self) -> Facility {
        match self {
            Self::Kern => Facility::LOG_KERN,
            Self::User => Facility::LOG_USER,
            Self::Mail => Facility::LOG_MAIL,
            Self::Daemon => Facility::LOG_DAEMON,
            Self::Auth => Facility::LOG_AUTH,
            Self::Syslog => Facility::LOG_SYSLOG,
            Self::Lpr => Facility::LOG_LPR,
            Self::News => Facility::LOG_NEWS,
            Self::Uucp => Facility::LOG_UUCP,
            Self::Cron => Facility::LOG_CRON,
            Self::Local0 => Facility::LOG_LOCAL0,
            Self::Local1 => Facility::LOG_LOCAL1,
            Self::Local2 => Facility::LOG_LOCAL2,
            Self::Local3 => Facility::LOG_LOCAL3,
            Self::Local4 => Facility::LOG_LOCAL4,
            Self::Local5 => Facility::LOG_LOCAL5,
            Self::Local6 => Facility::LOG_LOCAL6,
            Self::Local7 => Facility::LOG_LOCAL7,
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
/// Encapsulates the facility and tag (ident) that the daemon advertises to
/// syslog. Constructing a [`SyslogConfig`] does not itself open the syslog
/// connection; call [`open`](SyslogConfig::open) to begin routing messages.
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
    /// Returns a [`SyslogGuard`] that closes the connection when dropped. The
    /// daemon should hold the guard for its lifetime; while it is alive,
    /// [`syslog_message`] routes diagnostics through the configured facility.
    ///
    /// Connection failures (e.g. no syslog daemon running) are swallowed so
    /// that callers never panic - this matches upstream rsync's behaviour of
    /// logging to syslog opportunistically. Subsequent [`syslog_message`]
    /// calls become no-ops until the next successful `open`.
    pub fn open(&self) -> SyslogGuard {
        let tag = if self.tag.is_empty() {
            DEFAULT_SYSLOG_TAG.to_string()
        } else {
            self.tag.clone()
        };

        let formatter = Formatter3164 {
            facility: self.facility.to_wire(),
            hostname: None,
            process: tag,
            pid: process::id(),
        };

        // upstream: log.c - openlog(tag, LOG_PID, facility)
        // The syslog crate's `unix(formatter)` connects to /dev/log on Linux
        // and falls back to /var/run/syslog on macOS, mirroring openlog(3).
        let logger = syslog::unix(formatter).ok();

        if let Ok(mut slot) = logger_slot().lock() {
            *slot = logger;
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
pub enum SyslogPriority {
    /// System is unusable (LOG_EMERG).
    Emergency,
    /// Action must be taken immediately (LOG_ALERT).
    Alert,
    /// Critical conditions (LOG_CRIT).
    Critical,
    /// Error conditions (LOG_ERR).
    Error,
    /// Warning conditions (LOG_WARNING).
    Warning,
    /// Normal but significant condition (LOG_NOTICE).
    Notice,
    /// Informational messages (LOG_INFO).
    Info,
    /// Debug-level messages (LOG_DEBUG).
    Debug,
}

/// Sends a message to syslog with the given priority.
///
/// The message is sent using the facility configured by the most recent
/// successful [`SyslogConfig::open`] call. If syslog has not been opened, or
/// if the previous `open` failed to connect, the call is silently dropped.
///
/// upstream: log.c - `syslog(priority, "%s", buf)`. The safe `syslog` crate
/// formats the message according to RFC 3164 before writing to the socket, so
/// `%` characters in `message` are not interpreted as format specifiers.
pub fn syslog_message(priority: SyslogPriority, message: &str) {
    let Ok(mut slot) = logger_slot().lock() else {
        return;
    };
    let Some(logger) = slot.as_mut() else {
        return;
    };
    // The syslog crate returns an error when the socket write fails; we ignore
    // the result so transient logging failures never bring down the daemon.
    let _ = match priority {
        SyslogPriority::Emergency => logger.emerg(message),
        SyslogPriority::Alert => logger.alert(message),
        SyslogPriority::Critical => logger.crit(message),
        SyslogPriority::Error => logger.err(message),
        SyslogPriority::Warning => logger.warning(message),
        SyslogPriority::Notice => logger.notice(message),
        SyslogPriority::Info => logger.info(message),
        SyslogPriority::Debug => logger.debug(message),
    };
}

/// RAII guard that closes the syslog connection when dropped.
///
/// Created by [`SyslogConfig::open`]. While this guard is alive, calls to
/// [`syslog_message`] route to the configured syslog facility. Dropping the
/// guard releases the underlying Unix socket.
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
/// // guard dropped here, syslog connection released
/// # }
/// ```
#[derive(Debug)]
pub struct SyslogGuard {
    _private: (),
}

impl Drop for SyslogGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = logger_slot().lock() {
            *slot = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn to_wire_covers_all_variants() {
        // Spot-check every variant maps to a syslog::Facility without panicking.
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
        for facility in facilities {
            let _wire = facility.to_wire();
        }
    }

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
        // The syslog crate's RFC 3164 formatter handles arbitrary content,
        // so embedded NULs no longer abort the call.
        syslog_message(SyslogPriority::Info, "before\0after");
    }

    #[test]
    fn syslog_message_without_open_is_noop() {
        // Drop any previously opened logger so we hit the cold path.
        if let Ok(mut slot) = logger_slot().lock() {
            *slot = None;
        }
        syslog_message(SyslogPriority::Info, "no logger configured");
    }

    #[test]
    fn priority_covers_all_severity_levels() {
        // Sanity check: each variant is constructible and Copy/Clone works.
        let priorities = [
            SyslogPriority::Emergency,
            SyslogPriority::Alert,
            SyslogPriority::Critical,
            SyslogPriority::Error,
            SyslogPriority::Warning,
            SyslogPriority::Notice,
            SyslogPriority::Info,
            SyslogPriority::Debug,
        ];
        for priority in priorities {
            let copy = priority;
            assert_eq!(priority, copy);
        }
    }

    #[test]
    fn guard_debug_format() {
        let config = SyslogConfig::default();
        let guard = config.open();
        let debug = format!("{guard:?}");
        assert!(debug.contains("SyslogGuard"));
    }
}
