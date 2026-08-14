//! `--debug=NSTR` producer emissions for the algorithm-negotiation strings.
//!
//! Mirrors upstream rsync 3.4.4's `compat.c` and `checksum.c`
//! `DEBUG_GTE(NSTR, N)` output byte-for-byte so wire-comparable diagnostics
//! line up across implementations. NSTR ("name string") covers the
//! checksum, compression, and daemon-auth-checksum vstring exchanges that
//! run during `negotiate_the_strings()` and `negotiate_daemon_auth()`.
//!
//! # Upstream Reference
//!
//! - `compat.c:373-378` (`recv_negotiate_str`, `DEBUG_GTE(NSTR, am_server?3:2)`)
//!   - `"Client %s list (on server): %s\n"` when `am_server`
//!   - `"Server %s list (on client): %s\n"` otherwise
//! - `compat.c:521-525` (`send_negotiate_str`, `DEBUG_GTE(NSTR, am_server?3:2)`)
//!   - `"Server %s list (on server): %s\n"` when `am_server`
//!   - `"Client %s list (on client): %s\n"` otherwise
//! - `compat.c:213-219` (`parse_compress_choice`, `DEBUG_GTE(NSTR, am_server?3:1)`)
//!   - `"%s%s compress: %s (level %d)\n"` where the second `%s` is
//!     `" negotiated"` iff `valid_compressions.negotiated_nni` is set
//! - `checksum.c:206-211` (`parse_checksum_choice`, `DEBUG_GTE(NSTR, am_server?3:1)`)
//!   - `"%s%s checksum: %s\n"` with the same `negotiated` conditional
//! - `compat.c:843-844` (`output_daemon_greeting`, `am_client && DEBUG_GTE(NSTR, 2)`)
//!   - `"Client %s list (on client): %s\n"` for the daemon-auth-checksum
//!     greeting echo
//! - `compat.c:865-868` (`negotiate_daemon_auth`, `DEBUG_GTE(NSTR, 1)`)
//!   - `"Client negotiated %s: %s\n"` after the daemon-auth checksum is
//!     selected from the server-advertised list

use logging::debug_log;

/// Process role used to render the `Client` / `Server` prefix in NSTR
/// emissions.
///
/// Mirrors upstream's `am_server` flag at the point each emission fires.
/// `negotiate_the_strings()` runs after role assignment in
/// `setup_protocol()` (`compat.c:572-580`), so the two values exactly
/// match upstream's branch on `am_server`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NstrSide {
    /// Local CLI process (upstream: `!am_server`).
    Client,
    /// Remote helper invoked over SSH or daemon (upstream: `am_server`).
    Server,
}

impl NstrSide {
    /// Returns the capitalised label used as the leading `%s` in the
    /// NSTR emissions ("Client" or "Server").
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Client => "Client",
            Self::Server => "Server",
        }
    }

    /// Returns the lowercase token used inside the `(on <local>)` clause.
    #[must_use]
    pub const fn local_token(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
        }
    }

    /// Returns the opposing side; used to render the `<remote> ... (on
    /// <local>)` shape in [`trace_recv_list`].
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Client => Self::Server,
            Self::Server => Self::Client,
        }
    }

    /// Returns the upstream level threshold for `send_negotiate_str` and
    /// `recv_negotiate_str`: `am_server ? 3 : 2`.
    #[must_use]
    pub const fn list_level(self) -> u8 {
        match self {
            Self::Client => 2,
            Self::Server => 3,
        }
    }

    /// Returns the upstream level threshold for `parse_checksum_choice`,
    /// `parse_compress_choice`, and `negotiate_daemon_auth`:
    /// `am_server ? 3 : 1`.
    #[must_use]
    pub const fn summary_level(self) -> u8 {
        match self {
            Self::Client => 1,
            Self::Server => 3,
        }
    }
}

/// Algorithm category negotiated via NSTR exchanges.
///
/// Renders the `%s` placeholder in upstream's
/// `"%s %s list (on %s): %s\n"` patterns. Upstream stores this in
/// `name_num_obj.type` (`compat.c:62`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NstrCategory {
    /// `valid_checksums.type = "checksum"` (`checksum.c:65`).
    Checksum,
    /// `valid_compressions.type = "compress"` (`compat.c:78`).
    Compress,
    /// `valid_auth_checksums.type = "auth"` (`checksum.c:71`).
    Auth,
}

impl NstrCategory {
    /// Returns the upstream `name_num_obj.type` string for this category.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Checksum => "checksum",
            Self::Compress => "compress",
            Self::Auth => "auth",
        }
    }
}

/// Upstream sentinel for `do_compression_level` when the user did not
/// pass `--compress-level=N`.
///
/// upstream: `rsync.h:1151` `#define CLVL_NOT_SPECIFIED INT_MIN`.
/// This is the raw wire value before resolution; it is NOT what upstream
/// prints. `parse_compress_choice(1)` calls `init_compression_level()`
/// (`token.c:55`) first, which substitutes the codec `def_level` for this
/// sentinel, so the `(level %d)` clause always shows a resolved level.
/// Callers must resolve the level (see
/// [`compress::algorithm::CompressionAlgorithm::resolve_debug_level`]) before
/// handing it to [`trace_compress_summary`], which prints it verbatim.
pub const CLVL_NOT_SPECIFIED: i32 = i32::MIN;

/// Traces a `send_negotiate_str` list emission.
///
/// upstream: `compat.c:521-525` -
/// `"Server <type> list (on server): <list>\n"` when `am_server`,
/// `"Client <type> list (on client): <list>\n"` otherwise.
///
/// The level is `am_server ? 3 : 2` ([`NstrSide::list_level`]).
#[inline]
pub fn trace_send_list(side: NstrSide, category: NstrCategory, list: &str) {
    debug_log!(
        Nstr,
        side.list_level(),
        "{} {} list (on {}): {}",
        side.label(),
        category.as_str(),
        side.local_token(),
        list
    );
}

/// Traces a `recv_negotiate_str` list emission.
///
/// upstream: `compat.c:373-378` -
/// `"Client <type> list (on server): <list>\n"` when `am_server`,
/// `"Server <type> list (on client): <list>\n"` otherwise.
///
/// The level is `am_server ? 3 : 2` ([`NstrSide::list_level`]).
#[inline]
pub fn trace_recv_list(side: NstrSide, category: NstrCategory, list: &str) {
    debug_log!(
        Nstr,
        side.list_level(),
        "{} {} list (on {}): {}",
        side.opposite().label(),
        category.as_str(),
        side.local_token(),
        list
    );
}

/// Traces the `parse_checksum_choice` summary emission.
///
/// upstream: `checksum.c:206-211` - `"%s%s checksum: %s\n"`. The second
/// `%s` renders `" negotiated"` iff the algorithm came out of the
/// `negotiate_the_strings()` vstring exchange. When the caller forced the
/// algorithm via `--checksum-choice`, upstream leaves the qualifier
/// blank because `valid_checksums.negotiated_nni` stays NULL
/// (`compat.c:175-187`).
///
/// The level is `am_server ? 3 : 1` ([`NstrSide::summary_level`]).
#[inline]
pub fn trace_checksum_summary(side: NstrSide, negotiated: bool, name: &str) {
    debug_log!(
        Nstr,
        side.summary_level(),
        "{}{} checksum: {}",
        side.label(),
        if negotiated { " negotiated" } else { "" },
        name
    );
}

/// Traces the `parse_compress_choice` summary emission.
///
/// upstream: `compat.c:213-219` - `"%s%s compress: %s (level %d)\n"`.
/// The `(level %d)` clause always renders. Upstream calls
/// `init_compression_level()` (`token.c:55`) inside `parse_compress_choice(1)`
/// before this print, resolving `do_compression_level` from
/// `CLVL_NOT_SPECIFIED` to the codec `def_level` (6 for zlib, 3 for zstd, 0
/// for lz4). This renderer prints `compress_level` verbatim, so callers must
/// resolve it first (see
/// [`compress::algorithm::CompressionAlgorithm::resolve_debug_level`]); passing
/// the raw sentinel would emit `(level -2147483648)`, which upstream never does.
///
/// Upstream gates the emission on
/// `do_compression != CPRES_NONE || level != CLVL_NOT_SPECIFIED`; the
/// caller is responsible for that guard.
///
/// The level is `am_server ? 3 : 1` ([`NstrSide::summary_level`]).
#[inline]
pub fn trace_compress_summary(side: NstrSide, negotiated: bool, name: &str, compress_level: i32) {
    debug_log!(
        Nstr,
        side.summary_level(),
        "{}{} compress: {} (level {})",
        side.label(),
        if negotiated { " negotiated" } else { "" },
        name,
        compress_level
    );
}

/// Traces the `output_daemon_greeting` echo on the client side.
///
/// upstream: `compat.c:843-844` -
/// `"Client <type> list (on client): <list>\n"` printed by the client
/// after it builds its `@RSYNCD: %d.%d %s\n` banner. Gated by
/// `am_client && DEBUG_GTE(NSTR, 2)` - the server never emits this
/// shape because `output_daemon_greeting` is called from both sides but
/// the trace is client-only.
///
/// The category is fixed to [`NstrCategory::Auth`] because upstream
/// passes `valid_auth_checksums.type`.
#[inline]
pub fn trace_daemon_greeting_auth_list(list: &str) {
    debug_log!(
        Nstr,
        2,
        "{} {} list (on {}): {}",
        NstrSide::Client.label(),
        NstrCategory::Auth.as_str(),
        NstrSide::Client.local_token(),
        list
    );
}

/// Traces the `negotiate_daemon_auth` selected-algorithm emission.
///
/// upstream: `compat.c:865-868` - `"Client negotiated <type>: <name>\n"`
/// after the client picks an algorithm from the server-advertised auth
/// list. Always emitted as level 1 because upstream's branch is
/// `DEBUG_GTE(NSTR, 1)` without the `am_server ? 3 : ...` ternary.
///
/// The category is fixed to [`NstrCategory::Auth`] because upstream
/// passes `valid_auth_checksums.type`.
#[inline]
pub fn trace_daemon_auth_negotiated(name: &str) {
    debug_log!(
        Nstr,
        1,
        "{} negotiated {}: {}",
        NstrSide::Client.label(),
        NstrCategory::Auth.as_str(),
        name
    );
}

#[cfg(test)]
mod tests {
    //! Pinning tests for NSTR emission shapes. Strings match upstream
    //! `compat.c` and `checksum.c` byte-for-byte.

    use super::*;
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    fn init_at(level: u8) {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.nstr = level;
        init(cfg);
        let _ = drain_events();
    }

    fn nstr_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Nstr,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn side_renders_upstream_labels() {
        // upstream: compat.c:215 - `am_server ? "Server" : "Client"`.
        assert_eq!(NstrSide::Client.label(), "Client");
        assert_eq!(NstrSide::Server.label(), "Server");
        assert_eq!(NstrSide::Client.local_token(), "client");
        assert_eq!(NstrSide::Server.local_token(), "server");
        assert_eq!(NstrSide::Client.opposite(), NstrSide::Server);
        assert_eq!(NstrSide::Server.opposite(), NstrSide::Client);
    }

    #[test]
    fn category_renders_upstream_type_strings() {
        // upstream: checksum.c:65/71, compat.c:78 - the `type` field of
        // each `name_num_obj` (used as `%s` in the list/summary
        // emissions).
        assert_eq!(NstrCategory::Checksum.as_str(), "checksum");
        assert_eq!(NstrCategory::Compress.as_str(), "compress");
        assert_eq!(NstrCategory::Auth.as_str(), "auth");
    }

    #[test]
    fn upstream_level_table() {
        // upstream: compat.c:373/521 - `am_server ? 3 : 2`.
        assert_eq!(NstrSide::Client.list_level(), 2);
        assert_eq!(NstrSide::Server.list_level(), 3);
        // upstream: checksum.c:206, compat.c:213 - `am_server ? 3 : 1`.
        assert_eq!(NstrSide::Client.summary_level(), 1);
        assert_eq!(NstrSide::Server.summary_level(), 3);
    }

    #[test]
    fn clvl_not_specified_matches_upstream_int_min() {
        // upstream: rsync.h:1151 - `#define CLVL_NOT_SPECIFIED INT_MIN`.
        assert_eq!(CLVL_NOT_SPECIFIED, i32::MIN);
    }

    #[test]
    fn send_list_client_emits_upstream_format() {
        // upstream: compat.c:525 - "Client <type> list (on client): <list>".
        init_at(2);
        trace_send_list(NstrSide::Client, NstrCategory::Checksum, "xxh3 md5 md4");
        let m = nstr_messages();
        assert!(
            m.iter()
                .any(|s| s == "Client checksum list (on client): xxh3 md5 md4"),
            "missing send-list client: {m:?}"
        );
    }

    #[test]
    fn send_list_server_emits_upstream_format() {
        // upstream: compat.c:523 - "Server <type> list (on server): <list>".
        init_at(3);
        trace_send_list(NstrSide::Server, NstrCategory::Compress, "zstd zlibx zlib");
        let m = nstr_messages();
        assert!(
            m.iter()
                .any(|s| s == "Server compress list (on server): zstd zlibx zlib"),
            "missing send-list server: {m:?}"
        );
    }

    #[test]
    fn recv_list_client_emits_upstream_format() {
        // upstream: compat.c:377 - "Server <type> list (on client): <list>".
        init_at(2);
        trace_recv_list(NstrSide::Client, NstrCategory::Checksum, "md5 md4");
        let m = nstr_messages();
        assert!(
            m.iter()
                .any(|s| s == "Server checksum list (on client): md5 md4"),
            "missing recv-list client: {m:?}"
        );
    }

    #[test]
    fn recv_list_server_emits_upstream_format() {
        // upstream: compat.c:375 - "Client <type> list (on server): <list>".
        init_at(3);
        trace_recv_list(NstrSide::Server, NstrCategory::Compress, "zlib");
        let m = nstr_messages();
        assert!(
            m.iter()
                .any(|s| s == "Client compress list (on server): zlib"),
            "missing recv-list server: {m:?}"
        );
    }

    #[test]
    fn checksum_summary_negotiated_includes_qualifier() {
        // upstream: checksum.c:207-210 - second %s = " negotiated"
        // when valid_checksums.negotiated_nni is non-NULL.
        init_at(1);
        trace_checksum_summary(NstrSide::Client, true, "xxh3");
        let m = nstr_messages();
        assert!(
            m.iter().any(|s| s == "Client negotiated checksum: xxh3"),
            "missing negotiated checksum summary: {m:?}"
        );
    }

    #[test]
    fn checksum_summary_forced_omits_qualifier() {
        // upstream: checksum.c:209 - when negotiated_nni is NULL (user
        // forced via --checksum-choice), second %s renders blank.
        init_at(1);
        trace_checksum_summary(NstrSide::Client, false, "md5");
        let m = nstr_messages();
        assert!(
            m.iter().any(|s| s == "Client checksum: md5"),
            "missing forced checksum summary: {m:?}"
        );
        // Must NOT contain the negotiated wording.
        assert!(
            !m.iter().any(|s| s.contains("negotiated")),
            "forced summary must not contain ' negotiated': {m:?}"
        );
    }

    #[test]
    fn compress_summary_renders_level_clause() {
        // upstream: compat.c:215-218 - "(level %d)" always renders.
        init_at(1);
        trace_compress_summary(NstrSide::Client, true, "zstd", 3);
        let m = nstr_messages();
        assert!(
            m.iter()
                .any(|s| s == "Client negotiated compress: zstd (level 3)"),
            "missing compress summary with level: {m:?}"
        );
    }

    #[test]
    fn compress_summary_renders_level_verbatim() {
        // This renderer is level-transparent: it prints whatever `compress_level`
        // it is handed. Callers resolve CLVL_NOT_SPECIFIED to the codec def_level
        // via init_compression_level (token.c:55) before calling, so a resolved
        // zlib level of 6 renders as-is. upstream: compat.c:216-219.
        init_at(1);
        trace_compress_summary(NstrSide::Client, true, "zlib", 6);
        let m = nstr_messages();
        assert!(
            m.iter()
                .any(|s| s == "Client negotiated compress: zlib (level 6)"),
            "renderer must print the resolved level verbatim: {m:?}"
        );
    }

    #[test]
    fn compress_summary_forced_omits_qualifier() {
        // upstream: compat.c:217 - second %s blank when negotiated_nni
        // is NULL.
        init_at(1);
        trace_compress_summary(NstrSide::Server, false, "zlib", 6);
        let m = nstr_messages();
        // server side gates at level 3, so level 1 init suppresses.
        assert!(
            m.is_empty(),
            "server summary must gate at level 3, got: {m:?}"
        );
        init_at(3);
        trace_compress_summary(NstrSide::Server, false, "zlib", 6);
        let m = nstr_messages();
        assert!(
            m.iter().any(|s| s == "Server compress: zlib (level 6)"),
            "missing forced compress summary: {m:?}"
        );
    }

    #[test]
    fn daemon_greeting_auth_list_emits_upstream_format() {
        // upstream: compat.c:843-844 - "Client auth list (on client): <list>".
        init_at(2);
        trace_daemon_greeting_auth_list("sha512 sha256 sha1 md5 md4");
        let m = nstr_messages();
        assert!(
            m.iter()
                .any(|s| s == "Client auth list (on client): sha512 sha256 sha1 md5 md4"),
            "missing daemon-greeting auth list: {m:?}"
        );
    }

    #[test]
    fn daemon_auth_negotiated_emits_upstream_format() {
        // upstream: compat.c:866-867 - "Client negotiated auth: <name>".
        init_at(1);
        trace_daemon_auth_negotiated("md5");
        let m = nstr_messages();
        assert!(
            m.iter().any(|s| s == "Client negotiated auth: md5"),
            "missing daemon-auth negotiated: {m:?}"
        );
    }

    #[test]
    fn list_emissions_gate_at_level_two_client() {
        // upstream: compat.c:373/521 - `DEBUG_GTE(NSTR, 2)` for client.
        init_at(1);
        trace_send_list(NstrSide::Client, NstrCategory::Checksum, "md5");
        trace_recv_list(NstrSide::Client, NstrCategory::Checksum, "md5");
        assert!(
            nstr_messages().is_empty(),
            "client list emissions must gate at level 2"
        );
        init_at(2);
        trace_send_list(NstrSide::Client, NstrCategory::Checksum, "md5");
        trace_recv_list(NstrSide::Client, NstrCategory::Checksum, "md5");
        assert_eq!(
            nstr_messages().len(),
            2,
            "two list emissions must fire at level 2"
        );
    }

    #[test]
    fn list_emissions_gate_at_level_three_server() {
        // upstream: compat.c:373/521 - `DEBUG_GTE(NSTR, 3)` for server.
        init_at(2);
        trace_send_list(NstrSide::Server, NstrCategory::Checksum, "md5");
        trace_recv_list(NstrSide::Server, NstrCategory::Checksum, "md5");
        assert!(
            nstr_messages().is_empty(),
            "server list emissions must gate at level 3"
        );
        init_at(3);
        trace_send_list(NstrSide::Server, NstrCategory::Checksum, "md5");
        trace_recv_list(NstrSide::Server, NstrCategory::Checksum, "md5");
        assert_eq!(
            nstr_messages().len(),
            2,
            "two server list emissions must fire at level 3"
        );
    }

    #[test]
    fn summary_emissions_gate_at_level_one_client() {
        // upstream: checksum.c:206, compat.c:213 - `DEBUG_GTE(NSTR, 1)`
        // for client.
        init_at(0);
        trace_checksum_summary(NstrSide::Client, true, "md5");
        trace_compress_summary(NstrSide::Client, true, "zlib", 6);
        assert!(
            nstr_messages().is_empty(),
            "summary emissions must gate at level 1"
        );
        init_at(1);
        trace_checksum_summary(NstrSide::Client, true, "md5");
        trace_compress_summary(NstrSide::Client, true, "zlib", 6);
        assert_eq!(
            nstr_messages().len(),
            2,
            "client summary emissions must fire at level 1"
        );
    }

    #[test]
    fn daemon_emissions_gate_correctly() {
        // upstream: compat.c:843 gates the auth list at level 2;
        // compat.c:865 gates the negotiated auth at level 1.
        init_at(0);
        trace_daemon_greeting_auth_list("md5 md4");
        trace_daemon_auth_negotiated("md5");
        assert!(
            nstr_messages().is_empty(),
            "daemon emissions must gate at level 0"
        );
        init_at(1);
        trace_daemon_greeting_auth_list("md5 md4");
        trace_daemon_auth_negotiated("md5");
        let m = nstr_messages();
        assert_eq!(
            m.len(),
            1,
            "only the negotiated-auth emission must fire at level 1, got: {m:?}"
        );
        assert!(
            m.iter().any(|s| s == "Client negotiated auth: md5"),
            "level 1 must include the negotiated-auth line: {m:?}"
        );
        init_at(2);
        trace_daemon_greeting_auth_list("md5 md4");
        trace_daemon_auth_negotiated("md5");
        assert_eq!(
            nstr_messages().len(),
            2,
            "both daemon emissions must fire at level 2"
        );
    }

    #[test]
    fn level_zero_suppresses_all_nstr_emissions() {
        init_at(0);
        trace_send_list(NstrSide::Client, NstrCategory::Checksum, "md5");
        trace_recv_list(NstrSide::Client, NstrCategory::Compress, "zlib");
        trace_checksum_summary(NstrSide::Client, true, "md5");
        trace_compress_summary(NstrSide::Server, true, "zlib", 6);
        trace_daemon_greeting_auth_list("md5");
        trace_daemon_auth_negotiated("md5");
        assert!(
            nstr_messages().is_empty(),
            "all NSTR emissions must be suppressed at level 0"
        );
    }
}
