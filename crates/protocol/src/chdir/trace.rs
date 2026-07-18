//! `--debug=CHDIR` producer emissions for the current-directory change path.
//!
//! Mirrors upstream rsync 3.4.4's `util1.c::change_dir` `DEBUG_GTE(CHDIR, 1)`
//! output byte-for-byte so wire-comparable diagnostics align across
//! implementations. Upstream funnels every `chdir()` syscall through
//! `change_dir`, which prints `"[%s] change_dir(%s)\n"` (`util1.c:1168-1169`)
//! after the syscall succeeds. The `%s` placeholders render
//! `who_am_i()` (`rsync.c:823-830`) and the cleaned absolute `curr_dir`.
//!
//! # Upstream Reference
//!
//! - `util1.c:1113-1172` `change_dir` - wraps `chdir`, maintains `curr_dir`,
//!   cleans the path via `clean_fname`, and fires the emission below.
//! - `util1.c:1168-1169` `DEBUG_GTE(CHDIR, 1)` -
//!   `"[%s] change_dir(%s)\n"` once per successful syscall (`set_path_only`
//!   variants skip emission, matching upstream's `!set_path_only` guard).
//! - `rsync.c:823-830` `who_am_i()` - role prefix string used by the
//!   emission.
//! - `options.c:293` `DEBUG_WORD(CHDIR, W_CLI|W_SRV, ...)` - flag table
//!   entry, capping useful emissions at level 1.

use logging::debug_log;

/// Process role used as the `[<role>]` prefix in CHDIR emissions.
///
/// Mirrors upstream's `who_am_i()` vocabulary (`rsync.c:823-830`) without
/// pulling in heavier process-wide globals. Callers pass the role
/// explicitly so the helper stays a thin formatter.
///
/// During `am_starting_up`, upstream returns `"client"` / `"server"`
/// depending on `am_server`. After role assignment it returns
/// `"sender"`, `"generator"`, `"receiver"`, or the capitalised
/// `"Receiver"` for the pre-forked receiver path. The enum covers all
/// six tokens so callsites can match upstream verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChdirRole {
    /// Local CLI process before role assignment (upstream: `!am_server`
    /// while `am_starting_up`).
    Client,
    /// Server-side process before role assignment (upstream: `am_server`
    /// while `am_starting_up`).
    Server,
    /// Sender role (upstream: `am_sender`).
    Sender,
    /// Generator role (upstream: `am_generator`).
    Generator,
    /// Receiver role after the receiver/generator fork
    /// (upstream: `am_receiver`).
    Receiver,
    /// Pre-forked receiver path (upstream catch-all when no `am_*` role
    /// is yet set; `who_am_i` returns the capitalised `"Receiver"`).
    PreForkReceiver,
}

impl ChdirRole {
    /// Returns the upstream `who_am_i()` token for this role.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
            Self::Sender => "sender",
            Self::Generator => "generator",
            Self::Receiver => "receiver",
            Self::PreForkReceiver => "Receiver",
        }
    }
}

impl std::fmt::Display for ChdirRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Traces a successful `chdir()` syscall (level 1).
///
/// upstream: `util1.c:1168-1169` - `"[%s] change_dir(%s)\n"`. Emitted by
/// `change_dir` after the wrapped `chdir(2)` succeeds and `curr_dir` has
/// been refreshed with the cleaned absolute path. Callers should pass the
/// post-syscall, fully-resolved absolute path that matches what upstream
/// stores in `curr_dir[]` (e.g., the chroot target after `chdir("/")`,
/// or the dest-path argument after `change_dir(dest_path, CD_NORMAL)`).
///
/// Upstream gates the emission with `!set_path_only`; callers should only
/// invoke this helper when the directory change actually executed a
/// syscall, matching upstream's behaviour.
#[inline]
pub fn trace_change_dir(role: ChdirRole, curr_dir: &str) {
    debug_log!(Chdir, 1, "[{}] change_dir({})", role, curr_dir);
}

#[cfg(test)]
mod tests {
    //! Pinning tests for CHDIR emission shape. Strings match upstream
    //! `util1.c:1168-1169` byte-for-byte once the role token from
    //! `who_am_i()` is taken into account.

    use super::*;
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    /// Initialises logging with the requested CHDIR level and clears the
    /// pending event buffer so assertions can focus on emissions produced
    /// by the test body.
    fn init_chdir(level: u8) {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.chdir = level;
        init(cfg);
        let _ = drain_events();
    }

    /// Collects CHDIR-flagged debug messages emitted since the last drain.
    fn chdir_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Chdir,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    /// Role display strings match upstream `who_am_i()` vocabulary.
    ///
    /// upstream: rsync.c:823-830 - `who_am_i()` returns one of
    /// `"client"`, `"server"`, `"sender"`, `"generator"`, `"receiver"`,
    /// or the capitalised `"Receiver"` for the pre-fork branch.
    #[test]
    fn role_display_matches_who_am_i() {
        assert_eq!(ChdirRole::Client.as_str(), "client");
        assert_eq!(ChdirRole::Server.as_str(), "server");
        assert_eq!(ChdirRole::Sender.as_str(), "sender");
        assert_eq!(ChdirRole::Generator.as_str(), "generator");
        assert_eq!(ChdirRole::Receiver.as_str(), "receiver");
        assert_eq!(ChdirRole::PreForkReceiver.as_str(), "Receiver");

        assert_eq!(format!("{}", ChdirRole::Client), "client");
        assert_eq!(format!("{}", ChdirRole::PreForkReceiver), "Receiver");
    }

    /// Pins the level 1 emission for the client role to upstream format.
    ///
    /// upstream: util1.c:1168-1169 - `"[%s] change_dir(%s)\n"`.
    #[test]
    fn change_dir_matches_upstream_format_client() {
        init_chdir(1);
        trace_change_dir(ChdirRole::Client, "/tmp/src");
        let msgs = chdir_messages();
        assert!(
            msgs.iter().any(|m| m == "[client] change_dir(/tmp/src)"),
            "expected upstream-format CHDIR,1 client emission, got {msgs:?}"
        );
    }

    /// Pins the level 1 emission for the server/daemon-chroot role.
    ///
    /// upstream: util1.c:1168-1169 emitted from `clientserver.c:987`
    /// `change_dir(module_chdir, CD_NORMAL)` after the daemon chroot
    /// jail is established.
    #[test]
    fn change_dir_matches_upstream_format_daemon_chroot() {
        init_chdir(1);
        trace_change_dir(ChdirRole::PreForkReceiver, "/");
        let msgs = chdir_messages();
        assert!(
            msgs.iter().any(|m| m == "[Receiver] change_dir(/)"),
            "expected upstream-format CHDIR,1 daemon-chroot emission, got {msgs:?}"
        );
    }

    /// Pins the level 1 emission for every other upstream role token.
    #[test]
    fn change_dir_matches_upstream_format_all_roles() {
        init_chdir(1);
        trace_change_dir(ChdirRole::Server, "/var/srv");
        trace_change_dir(ChdirRole::Sender, "/data");
        trace_change_dir(ChdirRole::Generator, "/dst");
        trace_change_dir(ChdirRole::Receiver, "/dst");
        let msgs = chdir_messages();
        for expected in [
            "[server] change_dir(/var/srv)",
            "[sender] change_dir(/data)",
            "[generator] change_dir(/dst)",
            "[receiver] change_dir(/dst)",
        ] {
            assert!(
                msgs.iter().any(|m| m == expected),
                "missing {expected}: got {msgs:?}"
            );
        }
    }

    /// Paths with spaces and special characters render verbatim because
    /// upstream's `rprintf(FINFO, "[%s] change_dir(%s)\n", ...)` does not
    /// quote the path argument.
    #[test]
    fn change_dir_renders_paths_verbatim() {
        init_chdir(1);
        trace_change_dir(ChdirRole::Receiver, "/path with spaces/sub");
        trace_change_dir(ChdirRole::Receiver, ".");
        let msgs = chdir_messages();
        assert!(
            msgs.iter()
                .any(|m| m == "[receiver] change_dir(/path with spaces/sub)"),
            "missing space-bearing path: {msgs:?}"
        );
        assert!(
            msgs.iter().any(|m| m == "[receiver] change_dir(.)"),
            "missing dot path: {msgs:?}"
        );
    }

    /// CHDIR emissions must not fire when the flag is disabled.
    ///
    /// upstream: with `DEBUG_CHDIR` level 0, `DEBUG_GTE(CHDIR, 1)`
    /// evaluates to false and the `rprintf` line is skipped.
    #[test]
    fn level_zero_suppresses_emission() {
        init_chdir(0);
        trace_change_dir(ChdirRole::Receiver, "/anything");
        assert!(
            chdir_messages().is_empty(),
            "level 0 must suppress emission"
        );
    }

    /// Level 1 fires exactly one line per call (no duplicate emissions).
    #[test]
    fn level_one_emits_exactly_once_per_call() {
        init_chdir(1);
        trace_change_dir(ChdirRole::Sender, "/data");
        let msgs = chdir_messages();
        assert_eq!(
            msgs.len(),
            1,
            "expected one CHDIR line per call, got {msgs:?}"
        );
        assert_eq!(msgs[0], "[sender] change_dir(/data)");
    }
}
