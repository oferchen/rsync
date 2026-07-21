//! Regression: the `receiving incremental file list` banner on a remote pull
//! must fire exactly like upstream `flist.c:2606-2607` - only for a client-side
//! receiver (`!am_server`), under recursion (upstream disables inc_recurse when
//! `!recurse`, compat.c:172-173), and when the FLIST info category is at level >= 1
//! (so `--info=flist0` suppresses it even at `-v`).
//!
//! The banner is written straight to the client's stdout at file-list-receive
//! time (see `setup_transfer`), ahead of the per-file names, rather than through
//! the deferred `info_log!` event buffer that the CLI only drains after the
//! summary stats - which previously left the banner printing dead last on every
//! ssh/russh/daemon pull.

use logging::VerbosityConfig;
use protocol::ProtocolVersion;

use super::super::ReceiverContext;
use super::support::test_handshake;
use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::role::ServerRole;

fn ctx(client_mode: bool, recursive: bool) -> ReceiverContext {
    let handshake = test_handshake();
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flags: ParsedServerFlags {
            verbose: true,
            recursive,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut c = ReceiverContext::new_for_test(&handshake, config);
    c.config.connection.client_mode = client_mode;
    c
}

/// The canonical case: a recursive client-side pull at `-v` (FLIST level 1)
/// announces the incremental file list.
#[test]
fn client_recursive_flist1_announces() {
    logging::init(VerbosityConfig::from_verbose_level(1));
    assert!(ctx(true, true).should_announce_incremental_flist());
}

/// `--info=flist0` (FLIST level 0) suppresses the banner even for a recursive
/// client pull, mirroring the upstream `INFO_GTE(FLIST, 1)` gate.
#[test]
fn flist0_suppresses_even_when_recursive() {
    logging::init(VerbosityConfig::from_verbose_level(0));
    assert!(!ctx(true, true).should_announce_incremental_flist());
}

/// A non-recursive single-file `-v` pull prints no banner: upstream leaves
/// inc_recurse off without `-r`, so `flist.c:2607` is not reached.
#[test]
fn non_recursive_prints_no_banner() {
    logging::init(VerbosityConfig::from_verbose_level(1));
    assert!(!ctx(true, false).should_announce_incremental_flist());
}

/// A server-mode receiver (the remote end of a push) never prints the banner:
/// upstream gates it on `!am_server`, and the client's SENDER prints its own
/// `sending incremental file list` instead.
#[test]
fn server_mode_receiver_never_announces() {
    logging::init(VerbosityConfig::from_verbose_level(1));
    assert!(!ctx(false, true).should_announce_incremental_flist());
}
