//! Regression: a remote pull's file-list banner must follow upstream
//! `recv_file_list()` (flist.c:3119-3122) - `receiving incremental file list`
//! only when INC_RECURSE was negotiated, otherwise `receiving file list ...
//! done` under `xfer_dirs` (flist.c:172), and only for a client-side receiver
//! (`!am_server`) with the FLIST info category at level >= 1.
//!
//! The banner is written straight to the client's stdout at file-list-receive
//! time (see `setup_transfer`), ahead of the per-file names, rather than through
//! the deferred `info_log!` event buffer that the CLI only drains after the
//! summary stats - which previously left the banner printing dead last on every
//! ssh/russh/daemon pull.

use logging::VerbosityConfig;
use protocol::{CompatibilityFlags, ProtocolVersion};

use super::super::ReceiverContext;
use super::support::test_handshake;
use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::flist_banner::FlistBanner;
use crate::role::ServerRole;

/// `inc_recurse` stands for the negotiated `CF_INC_RECURSE` bit; compat flags
/// are always exchanged, as on every protocol 30+ session.
fn ctx(client_mode: bool, recursive: bool, inc_recurse: bool) -> ReceiverContext {
    let mut handshake = test_handshake();
    handshake.compat_flags = Some(if inc_recurse {
        CompatibilityFlags::INC_RECURSE
    } else {
        CompatibilityFlags::EMPTY
    });
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

/// A recursive client-side pull that negotiated INC_RECURSE at `-v` (FLIST
/// level 1) announces the incremental file list.
#[test]
fn negotiated_inc_recurse_announces() {
    logging::init(VerbosityConfig::from_verbose_level(1));
    assert_eq!(
        ctx(true, true, true).flist_banner(),
        FlistBanner::Incremental
    );
}

/// A recursive pull without INC_RECURSE (`--no-inc-recursive`, a receiver-side
/// `--delete-after`, protocol < 30) prints `receiving file list ... done`.
#[test]
fn recursive_without_inc_recurse_prints_progress_banner() {
    logging::init(VerbosityConfig::from_verbose_level(1));
    assert_eq!(ctx(true, true, false).flist_banner(), FlistBanner::Progress);
}

/// `--info=flist0` (FLIST level 0) suppresses the banner even for a recursive
/// client pull, mirroring the upstream `INFO_GTE(FLIST, 1)` gate.
#[test]
fn flist0_suppresses_even_when_recursive() {
    logging::init(VerbosityConfig::from_verbose_level(0));
    assert_eq!(ctx(true, true, true).flist_banner(), FlistBanner::None);
}

/// A non-recursive single-file `-v` pull has neither inc_recurse nor
/// `xfer_dirs`, so neither banner is printed.
#[test]
fn non_recursive_prints_no_banner() {
    logging::init(VerbosityConfig::from_verbose_level(1));
    assert_eq!(ctx(true, false, false).flist_banner(), FlistBanner::None);
}

/// A server-mode receiver (the remote end of a push) never prints the banner:
/// upstream gates it on `!am_server`, and the client's SENDER prints its own
/// banner instead.
#[test]
fn server_mode_receiver_never_announces() {
    logging::init(VerbosityConfig::from_verbose_level(1));
    assert_eq!(ctx(false, true, true).flist_banner(), FlistBanner::None);
}
