//! Drives the real session entry point to pin that `setup_protocol()` refuses
//! options the negotiated protocol cannot carry, on every role, before any
//! further byte is exchanged.
//!
//! Without the refusal a protocol < 30 `-A` or a protocol 28 `-m` transfer
//! started anyway and died mid-stream with RERR_STREAMIO (12), while upstream
//! refuses up front with RERR_PROTOCOL (2).
//!
//! upstream: compat.c:663-676 (protocol < 30), compat.c:684-713 (protocol < 29).
use crate::handshake::HandshakeResult;
use crate::{ServerConfig, ServerRole, run_server_with_handshake};
use engine::{ReferenceDirectory, ReferenceDirectoryKind};
use protocol::ProtocolVersion;

type Set = fn(&mut ServerConfig);

/// Each refused option, the newest protocol that still refuses it, and
/// upstream's exact refusal line for that protocol.
const REFUSALS: [(Set, u8, &str); 6] = [
    (
        |c| c.flags.acls = true,
        29,
        "--acls requires protocol 30 or higher (negotiated 29).",
    ),
    (
        |c| c.flags.xattrs = true,
        29,
        "--xattrs requires protocol 30 or higher (negotiated 29).",
    ),
    (
        |c| c.flags.fuzzy_level = 1,
        28,
        "--fuzzy requires protocol 29 or higher (negotiated 28).",
    ),
    (
        |c| {
            c.reference_directories = vec![basis("a")];
            c.write.inplace = true;
        },
        28,
        "--compare-dest/--copy-dest/--link-dest with --inplace requires protocol 29 or higher (negotiated 28).",
    ),
    (
        |c| c.reference_directories = vec![basis("a"), basis("b")],
        28,
        "Using more than one --compare-dest/--copy-dest/--link-dest option requires protocol 29 or higher (negotiated 28).",
    ),
    (
        |c| c.flags.prune_empty_dirs = true,
        28,
        "--prune-empty-dirs requires protocol 29 or higher (negotiated 28).",
    ),
];

fn basis(dir: &str) -> ReferenceDirectory {
    ReferenceDirectory::new(ReferenceDirectoryKind::Compare, dir)
}

/// Runs one session against a silent peer and returns the error together
/// with every byte this side wrote.
fn run(role: ServerRole, client_mode: bool, set: Set, protocol: u8) -> (std::io::Error, Vec<u8>) {
    let mut config = ServerConfig {
        role,
        ..ServerConfig::default()
    };
    config.connection.client_mode = client_mode;
    config.args = vec![std::ffi::OsString::from(".")];
    set(&mut config);
    let handshake = HandshakeResult {
        protocol: ProtocolVersion::try_from(protocol).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    };
    let mut written = Vec::new();
    let mut stdin: &[u8] = &[];
    let err = run_server_with_handshake(
        config,
        handshake,
        &mut stdin,
        &mut written,
        None,
        None,
        None,
    )
    .expect_err("a silent peer cannot complete a transfer");
    (err, written)
}

const ROLES: [(ServerRole, bool); 4] = [
    (ServerRole::Receiver, true),
    (ServerRole::Generator, true),
    (ServerRole::Receiver, false),
    (ServerRole::Generator, false),
];

#[test]
fn every_role_refuses_an_option_the_protocol_cannot_carry() {
    for (role, client_mode) in ROLES {
        for (set, protocol, message) in REFUSALS {
            let (err, written) = run(role, client_mode, set, protocol);
            let cell = format!("{role:?} client_mode={client_mode} {message}");
            assert_eq!(err.to_string(), message, "{cell}");
            assert_eq!(crate::error::rerr_for_io_error(&err), 2, "{cell}");
            // The refusal precedes the checksum-seed exchange, so a server
            // writes nothing at all (compat.c:663-713 runs before compat.c:822).
            assert!(written.is_empty(), "{cell}: wrote {written:?}");
        }
    }
}

/// Opposed control: one protocol higher, the same session gets past the check
/// and fails later on the silent peer instead.
#[test]
fn the_next_protocol_accepts_the_same_option() {
    for (role, client_mode) in ROLES {
        for (set, protocol, message) in REFUSALS {
            let (err, _) = run(role, client_mode, set, protocol + 1);
            assert_ne!(
                err.to_string(),
                message,
                "{role:?} client_mode={client_mode}"
            );
            assert_ne!(crate::error::rerr_for_io_error(&err), 2, "{message}");
        }
    }
}
