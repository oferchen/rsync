//! `--debug=ICONV` producer emissions for charset setup.
//!
//! Matches upstream rsync's `rsync.c::setup_iconv` `DEBUG_GTE(ICONV, N)`
//! output byte-for-byte so wire-comparable diagnostics align across
//! implementations.
//!
//! # Upstream Reference
//!
//! - `rsync.c:87-147` `setup_iconv` - opens `ic_chck`, `ic_send`, `ic_recv`.
//! - `rsync.c:99-110` (`DEBUG_GTE(ICONV, 2)`) - `ic_chck` message-charset
//!   probe; emits one of two shapes depending on whether `iconv_open`
//!   succeeded.
//! - `rsync.c:142-145` (`DEBUG_GTE(ICONV, 1)`) - peer charset announcement
//!   after `ic_send`/`ic_recv` are both opened.
//! - `options.c:306` - `DEBUG_WORD(ICONV, W_CLI|W_SRV, ...)` flag table
//!   entry, capping emissions at level 2.

use logging::debug_log;

/// Process identity for ICONV emissions.
///
/// `setup_iconv` runs while `am_starting_up == 1`, so upstream's
/// `who_am_i()` returns `"client"` or `"server"` (see
/// `rsync.c:823-830`). The sender/receiver/generator labels do not apply
/// because the iconv contexts are opened before those roles are assigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconvRole {
    /// Local CLI process (upstream: `!am_server`).
    Client,
    /// Remote helper invoked over SSH or daemon (upstream: `am_server`).
    Server,
}

impl IconvRole {
    /// Returns the upstream `who_am_i()` token for this role.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
        }
    }
}

impl std::fmt::Display for IconvRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Charset placeholder for an unset/locale-default charset.
///
/// Upstream renders `*charset ? charset : "[LOCALE]"` (`rsync.c:144`); a
/// `None` charset means the iconv setup walked the
/// `default_charset()` branch (locale-derived UTF-8).
const LOCALE_PLACEHOLDER: &str = "[LOCALE]";

/// Traces the peer charset announcement (level 1).
///
/// upstream: `rsync.c:142-145` - `"[%s] charset: %s\n"`. Emitted once
/// per process after `ic_send` and `ic_recv` are both opened. Pass
/// `charset = None` to render the upstream `[LOCALE]` placeholder for
/// the locale-default branch.
#[inline]
pub fn trace_peer_charset(role: IconvRole, charset: Option<&str>) {
    let label = charset.filter(|s| !s.is_empty()).unwrap_or(LOCALE_PLACEHOLDER);
    debug_log!(Iconv, 1, "[{}] charset: {}", role, label);
}

/// Traces a successful message-charset probe (level 2).
///
/// upstream: `rsync.c:106-108` - `"msg checking charset: %s\n"`. Emitted
/// when `iconv_open(defset, defset)` opens `ic_chck` for `isprint()`
/// validation, only when `!am_server && !allow_8bit_chars`.
#[inline]
pub fn trace_msg_checking_charset(defset: &str) {
    debug_log!(Iconv, 2, "msg checking charset: {}", defset);
}

/// Traces a failed message-charset probe (level 2).
///
/// upstream: `rsync.c:101-104` -
/// `"msg checking via isprint() (iconv_open(\"%s\", \"%s\") errno: %d)\n"`.
/// Emitted when `iconv_open(defset, defset)` returns `(iconv_t)-1`,
/// indicating that the locale charset is unknown to the system iconv
/// and the process will fall back to `isprint()` validation.
#[inline]
pub fn trace_msg_checking_via_isprint(defset: &str, errno: i32) {
    debug_log!(
        Iconv,
        2,
        "msg checking via isprint() (iconv_open(\"{}\", \"{}\") errno: {})",
        defset,
        defset,
        errno
    );
}

#[cfg(test)]
mod tests {
    //! Pinning tests for ICONV emission shapes. Strings match upstream
    //! `rsync.c::setup_iconv` byte-for-byte.

    use super::*;
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    fn init_at(level: u8) {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.iconv = level;
        init(cfg);
        let _ = drain_events();
    }

    fn iconv_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Iconv,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn role_renders_upstream_tokens() {
        // upstream: rsync.c:823-830 - who_am_i() returns "client" or
        // "server" during am_starting_up.
        assert_eq!(IconvRole::Client.as_str(), "client");
        assert_eq!(IconvRole::Server.as_str(), "server");
        assert_eq!(format!("{}", IconvRole::Client), "client");
        assert_eq!(format!("{}", IconvRole::Server), "server");
    }

    #[test]
    fn level1_peer_charset_named() {
        // upstream: rsync.c:142-145 - "[%s] charset: %s\n".
        init_at(1);
        trace_peer_charset(IconvRole::Client, Some("UTF-8"));
        let m = iconv_messages();
        assert!(
            m.iter().any(|s| s == "[client] charset: UTF-8"),
            "missing peer charset: {m:?}"
        );
    }

    #[test]
    fn level1_peer_charset_locale_placeholder_when_empty() {
        // upstream: rsync.c:144 - "*charset ? charset : \"[LOCALE]\"".
        init_at(1);
        trace_peer_charset(IconvRole::Server, None);
        trace_peer_charset(IconvRole::Server, Some(""));
        let m = iconv_messages();
        let locale = m
            .iter()
            .filter(|s| s.as_str() == "[server] charset: [LOCALE]")
            .count();
        assert_eq!(locale, 2, "locale placeholder must render for None and empty: {m:?}");
    }

    #[test]
    fn level2_msg_checking_success() {
        // upstream: rsync.c:106-108 - "msg checking charset: %s\n".
        init_at(2);
        trace_msg_checking_charset("UTF-8");
        let m = iconv_messages();
        assert!(
            m.iter().any(|s| s == "msg checking charset: UTF-8"),
            "missing checking-charset: {m:?}"
        );
    }

    #[test]
    fn level2_msg_checking_failure() {
        // upstream: rsync.c:101-104 - "msg checking via isprint() (iconv_open(\"%s\", \"%s\") errno: %d)\n".
        init_at(2);
        trace_msg_checking_via_isprint("BOGUS-CHARSET", 22);
        let m = iconv_messages();
        assert!(
            m.iter().any(|s| {
                s == "msg checking via isprint() (iconv_open(\"BOGUS-CHARSET\", \"BOGUS-CHARSET\") errno: 22)"
            }),
            "missing isprint fallback: {m:?}"
        );
    }

    #[test]
    fn level1_gates_level2_emissions() {
        // upstream: DEBUG_GTE(ICONV, 2) gates the message-checking probe.
        init_at(1);
        trace_msg_checking_charset("UTF-8");
        trace_msg_checking_via_isprint("BOGUS", 0);
        assert!(
            iconv_messages().is_empty(),
            "level-2 emissions must be gated at level 1"
        );
    }

    #[test]
    fn level0_suppresses_all_iconv_emissions() {
        // upstream: with DEBUG_ICONV at level 0, both DEBUG_GTE(ICONV, 1)
        // and DEBUG_GTE(ICONV, 2) evaluate to false.
        init_at(0);
        trace_peer_charset(IconvRole::Client, Some("UTF-8"));
        trace_msg_checking_charset("UTF-8");
        trace_msg_checking_via_isprint("BOGUS", 0);
        assert!(
            iconv_messages().is_empty(),
            "all ICONV emissions must be gated at level 0"
        );
    }
}
