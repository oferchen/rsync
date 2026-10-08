//! Regression coverage for the server's INC_RECURSE grant and the upstream
//! option predicate `ServerConfig::allows_inc_recurse` it rests on.
//!
//! upstream: compat.c:161-179 set_allow_inc_recurse, compat.c:724 (the server
//! writes `CF_INC_RECURSE` whenever `allow_inc_recurse` survives).

use crate::{ServerConfig, ServerRole};

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
fn recursion_without_qsort_allows_inc_recurse_in_either_role() {
    for role in [ServerRole::Generator, ServerRole::Receiver] {
        assert!(config(role, true, false).allows_inc_recurse(), "{role:?}");
        assert!(!config(role, false, false).allows_inc_recurse(), "{role:?}");
        assert!(!config(role, true, true).allows_inc_recurse(), "{role:?}");
    }
}

/// A push into an oc server makes it the receiver. Upstream's server grants
/// INC_RECURSE there exactly as it does on a pull: set_allow_inc_recurse() has
/// no role term beyond the receiver delete/delay/prune clauses, so a client
/// that offered `i` gets `CF_INC_RECURSE` back.
mod server_receiver_grant {
    use super::config;
    use crate::handshake::HandshakeResult;
    use crate::{ServerRole, run_server_with_handshake};
    use protocol::{CompatibilityFlags, ProtocolVersion};

    /// Runs a server receiver against a client that sent `flag_string` and
    /// returns the compat flags it wrote. They are the first bytes a server
    /// writes (compat.c:724-727); the empty input ends the session right after.
    fn written_compat_flags(flag_string: &str) -> CompatibilityFlags {
        let mut server = config(ServerRole::Receiver, true, false);
        server.flag_string = flag_string.to_owned();
        server.args = vec![std::ffi::OsString::from(".")];
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
        let mut wire = Vec::new();
        let mut stdin: &[u8] = &[];
        let _ =
            run_server_with_handshake(server, handshake, &mut stdin, &mut wire, None, None, None);
        CompatibilityFlags::decode_from_slice(&wire)
            .expect("compat flags on the wire")
            .0
    }

    #[test]
    fn server_receiver_grants_inc_recurse_the_client_offered() {
        let flags = written_compat_flags("-re.iLsfxCIvu");
        assert!(flags.contains(CompatibilityFlags::INC_RECURSE), "{flags:?}");
    }

    /// Opposed control: without the client's `i` the server withholds it
    /// (compat.c:178-179).
    #[test]
    fn server_receiver_withholds_inc_recurse_the_client_did_not_offer() {
        let flags = written_compat_flags("-re.LsfxCIvu");
        assert!(
            !flags.contains(CompatibilityFlags::INC_RECURSE),
            "{flags:?}"
        );
    }
}

/// A plain recursive receiver is allowed by its options: a peer-set
/// CF_INC_RECURSE must stay honoured on a pull,
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
