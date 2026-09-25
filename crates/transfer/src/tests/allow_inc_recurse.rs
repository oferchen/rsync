//! Regression coverage for `compute_allow_inc_recurse` and the upstream
//! option predicate `ServerConfig::allows_inc_recurse` it builds on.
//!
//! Pins the receiver-side restriction that gates INC_RECURSE so the upstream
//! testsuite `hardlinks` test no longer deadlocks against a source tree that
//! exceeds upstream's `MIN_FILECNT_LOOKAHEAD` window.
//!
//! upstream: compat.c:161-179 set_allow_inc_recurse,
//! sender.c:231-235 send_extra_file_list throttle.

use crate::{ServerConfig, ServerRole, compute_allow_inc_recurse};

fn config(role: ServerRole, recursive: bool, qsort: bool) -> ServerConfig {
    let mut config = ServerConfig {
        role,
        qsort,
        ..ServerConfig::default()
    };
    config.flags.recursive = recursive;
    config
}

#[test]
fn generator_with_recursion_advertises_inc_recurse() {
    assert!(compute_allow_inc_recurse(&config(
        ServerRole::Generator,
        true,
        false
    )));
}

#[test]
fn generator_without_recursion_does_not_advertise() {
    assert!(!compute_allow_inc_recurse(&config(
        ServerRole::Generator,
        false,
        false
    )));
}

#[test]
fn generator_with_qsort_does_not_advertise() {
    assert!(!compute_allow_inc_recurse(&config(
        ServerRole::Generator,
        true,
        true
    )));
}

/// Receiver MUST never advertise INC_RECURSE. Measured: dropping the role term
/// deadlocks the upstream testsuite `hardlinks` cell on a source tree of 1024
/// entries while 961 still passes, so the boundary is upstream's
/// MIN_FILECNT_LOOKAHEAD of 1000. See `compute_allow_inc_recurse` for the full
/// A/B table and for why the receiver-side blocking site is deliberately left
/// unnamed.
#[test]
fn receiver_never_advertises_inc_recurse() {
    assert!(!compute_allow_inc_recurse(&config(
        ServerRole::Receiver,
        true,
        false
    )));
    assert!(!compute_allow_inc_recurse(&config(
        ServerRole::Receiver,
        true,
        true
    )));
    assert!(!compute_allow_inc_recurse(&config(
        ServerRole::Receiver,
        false,
        false
    )));
}

/// A plain recursive receiver is allowed by its options even though oc never
/// advertises from it: a peer-set CF_INC_RECURSE must stay honoured on a pull,
/// or every inc-recursive pull would abort with RERR_SYNTAX.
#[test]
fn plain_recursive_receiver_options_allow_inc_recurse() {
    let mut receiver = config(ServerRole::Receiver, true, false);
    assert!(receiver.allows_inc_recurse());
    // upstream: compat.c:683-688 - `--delete` and `--delete-delay` are
    // during-deletes and stay compatible with inc-recursion.
    receiver.flags.delete = true;
    receiver.deletion.late_delete = true;
    assert!(receiver.allows_inc_recurse());
}

/// Each of upstream's four receiver clauses (compat.c:174-176) needs the full
/// file list before the transfer walk, so each alone must refuse inc-recursion.
#[test]
fn receiver_options_needing_the_whole_list_refuse_inc_recurse() {
    type Set = fn(&mut ServerConfig);
    let clauses: [(&str, Set); 4] = [
        ("--delete-before", |c| c.deletion.delete_before = true),
        ("--delete-after", |c| c.deletion.delete_after = true),
        ("--delay-updates", |c| c.write.delay_updates = true),
        ("--prune-empty-dirs", |c| c.flags.prune_empty_dirs = true),
    ];
    for (name, set) in clauses {
        let mut receiver = config(ServerRole::Receiver, true, false);
        set(&mut receiver);
        assert!(!receiver.allows_inc_recurse(), "{name} must refuse");

        // upstream: the clause is `!am_sender && (...)`, so a sender keeps it.
        let mut sender = config(ServerRole::Generator, true, false);
        set(&mut sender);
        assert!(sender.allows_inc_recurse(), "{name} must not bind a sender");
    }
}

/// Drives the real client entry point against a scripted peer that sets
/// CF_INC_RECURSE unasked, so the wiring from `ServerConfig` into protocol
/// setup is under test, not just the predicate. upstream: compat.c:780-785.
mod unasked_inc_recurse_from_peer {
    use super::config;
    use crate::handshake::HandshakeResult;
    use crate::{ServerConfig, ServerRole, run_server_with_handshake};
    use protocol::{CompatibilityFlags, ProtocolVersion};

    const MESSAGE: &str = "Incompatible options specified for inc-recursive connection.";

    fn run(set: fn(&mut ServerConfig), peer_flags: CompatibilityFlags) -> std::io::Error {
        let mut receiver = config(ServerRole::Receiver, true, false);
        receiver.connection.client_mode = true;
        receiver.args = vec![std::ffi::OsString::from(".")];
        set(&mut receiver);
        let handshake = HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        };
        // The peer's compat-flags varint and nothing else: past the check the
        // client hits EOF, which is a different failure.
        let mut wire = Vec::new();
        protocol::write_varint(&mut wire, peer_flags.bits() as i32).unwrap();
        let mut stdin = &wire[..];
        run_server_with_handshake(
            receiver,
            handshake,
            &mut stdin,
            Vec::new(),
            None,
            None,
            None,
        )
        .expect_err("a scripted peer cannot complete a transfer")
    }

    #[test]
    fn whole_list_receiver_refuses_a_peer_set_inc_recurse() {
        type Set = fn(&mut ServerConfig);
        let clauses: [(&str, Set); 4] = [
            ("--delete-before", |c| c.deletion.delete_before = true),
            ("--delete-after", |c| c.deletion.delete_after = true),
            ("--delay-updates", |c| c.write.delay_updates = true),
            ("--prune-empty-dirs", |c| c.flags.prune_empty_dirs = true),
        ];
        let inc = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::VARINT_FLIST_FLAGS;
        for (name, set) in clauses {
            let err = run(set, inc);
            assert_eq!(err.to_string(), MESSAGE, "{name}");
            assert_eq!(crate::error::rerr_for_io_error(&err), 1, "{name}");

            // Opposed control: the same receiver with no bit on the wire gets
            // past the check.
            let err = run(set, CompatibilityFlags::VARINT_FLIST_FLAGS);
            assert_ne!(err.to_string(), MESSAGE, "{name} without the bit");
        }
    }

    /// Opposed control: a plain `--delete` receiver keeps accepting it
    /// (compat.c:683-688 makes it a during-delete).
    #[test]
    fn plain_delete_receiver_accepts_a_peer_set_inc_recurse() {
        let err = run(
            |c| c.flags.delete = true,
            CompatibilityFlags::INC_RECURSE | CompatibilityFlags::VARINT_FLIST_FLAGS,
        );
        assert_ne!(err.to_string(), MESSAGE);
    }
}
