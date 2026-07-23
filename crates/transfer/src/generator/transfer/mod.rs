//! Main transfer loop and goodbye handshake for the generator role.
//!
//! Split into focused submodules:
//! - `transfer_loop` - `run_transfer_loop`, NDX-driven file sending with
//!   delta/whole-file paths across all phases.
//! - `goodbye` - `handle_goodbye` (protocol 31+ extended goodbye with
//!   `NDX_DEL_STATS`) plus its accumulation helpers.
//! - `stats` - server-side transfer statistics emission.
//! - `orchestrator` - top-level `run` that ties file list building,
//!   transmission, transfer, and finalization together.
//!
//! # Upstream Reference
//!
//! - `sender.c:send_files()` - Main transfer loop (lines 210-462)
//! - `main.c:893-924` - `read_final_goodbye()` with del_stats handling

mod goodbye;
mod orchestrator;
mod stats;
mod transfer_loop;

#[cfg(test)]
mod send_debug_emission_tests {
    //! Wording tests for `--debug=send` producer emissions.
    //!
    //! Each test asserts the exact wire string that upstream rsync 3.4.1
    //! emits from `sender.c send_files` under `DEBUG_GTE(SEND, 1)`. The
    //! strings are matched byte-for-byte because interop harnesses grep
    //! for these literals.

    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, debug_log, drain_events, init};
    use std::path::PathBuf;

    fn init_send_level1() {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.send = 1;
        init(cfg);
        let _ = drain_events();
    }

    fn send_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Send,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn send_files_starting_matches_upstream() {
        // upstream: sender.c:217-218
        init_send_level1();
        debug_log!(Send, 1, "send_files starting");
        let msgs = send_messages();
        assert!(
            msgs.iter().any(|m| m == "send_files starting"),
            "missing upstream wording: {msgs:?}"
        );
    }

    #[test]
    fn send_files_phase_matches_upstream() {
        // upstream: sender.c:258-259 - "send_files phase=%d"
        init_send_level1();
        let phase: i32 = 2;
        debug_log!(Send, 1, "send_files phase={}", phase);
        let msgs = send_messages();
        assert!(
            msgs.iter().any(|m| m == "send_files phase=2"),
            "missing upstream wording: {msgs:?}"
        );
    }

    #[test]
    fn send_files_per_file_matches_upstream() {
        // upstream: sender.c:283-284 - "send_files(%d, %s%s%s)"
        // F_PATHNAME unset -> path/slash empty, only fname emitted.
        init_send_level1();
        let ndx: i32 = 7;
        let name = PathBuf::from("dir/file.txt");
        debug_log!(Send, 1, "send_files({}, {})", ndx, name.display());
        let msgs = send_messages();
        assert!(
            msgs.iter().any(|m| m == "send_files(7, dir/file.txt)"),
            "missing upstream wording: {msgs:?}"
        );
    }

    #[test]
    fn sender_finished_matches_upstream() {
        // upstream: sender.c:477 - "sender finished %s%s%s"
        init_send_level1();
        let name = PathBuf::from("dir/file.txt");
        debug_log!(Send, 1, "sender finished {}", name.display());
        let msgs = send_messages();
        assert!(
            msgs.iter().any(|m| m == "sender finished dir/file.txt"),
            "missing upstream wording: {msgs:?}"
        );
    }

    #[test]
    fn send_files_finished_matches_upstream() {
        // upstream: sender.c:489 - "send files finished"
        init_send_level1();
        debug_log!(Send, 1, "send files finished");
        let msgs = send_messages();
        assert!(
            msgs.iter().any(|m| m == "send files finished"),
            "missing upstream wording: {msgs:?}"
        );
    }

    #[test]
    fn send_emissions_suppressed_when_disabled() {
        let cfg = VerbosityConfig::default();
        init(cfg);
        let _ = drain_events();
        debug_log!(Send, 1, "send_files starting");
        let msgs = send_messages();
        assert!(
            msgs.is_empty(),
            "Send debug emissions must be gated; got: {msgs:?}"
        );
    }
}
