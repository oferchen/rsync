//! NXT-7 regression test for the sender-side file-list symlink leak,
//! ported from the upstream `sender-flist-symlink-leak.test`.
//!
//! # Why this matters
//!
//! When a daemon module runs with `use chroot = no`, a local attacker can
//! plant a symlink inside the module that points outside the module root
//! (e.g. `module/cd -> /outside`). A client that pulls the symlinked
//! subdir must not be able to learn anything about `/outside`: the sender
//! enumerates the file list before any content transfer, so if the flist
//! generator followed the symlink it would ship the names, sizes, modes and
//! mtimes of every file under `/outside` to the client - a metadata leak -
//! even though the eventual content copy fails with EXDEV.
//!
//! The invariant: during recursive file-list generation the sender records
//! a symlink-to-directory as a single symlink entry and never descends into
//! its target, so no path outside the transfer root ever reaches the wire.
//! This holds because the default stat is an `lstat` (`fs::symlink_metadata`)
//! - a symlink reports `is_dir() == false`, so `should_recurse` stays false
//! in `walk.rs`.
//!
//! # Upstream Reference
//!
//! - `testsuite/sender-flist-symlink-leak.test` - the end-to-end daemon
//!   regression this test encodes as a fast in-process check.
//! - `flist.c:send_file_list()` / `readlink_stat()` - default lstat keeps
//!   symlinks unfollowed during enumeration.
//! - `util1.c:change_dir()` - the secure branch the upstream test exercises
//!   to reject a chdir that follows an attacker-controlled symlink.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use tempfile::TempDir;

use protocol::flist::FileType;
use protocol::ProtocolVersion;
use transfer::{
    GeneratorContext, HandshakeResult, ServerConfig, ServerRole, TransferPhase, TransferPipeline,
};

/// Builds a recursive sender (generator) config, the shape produced once a
/// daemon has accepted a pull request and is enumerating a module.
fn recursive_generator_config(source_root: &Path) -> ServerConfig {
    let flag_string = "-r.iLsfxCIvu".to_owned();
    let mut config = ServerConfig::from_flag_string_and_args(
        ServerRole::Generator,
        flag_string,
        vec![source_root.to_path_buf().into_os_string()],
    )
    .expect("server config");
    config.connection.client_mode = true;
    config
}

fn test_handshake() -> HandshakeResult {
    HandshakeResult {
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: true,
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    }
}

fn test_pipeline() -> TransferPipeline {
    let mut pipeline = TransferPipeline::new(ServerRole::Generator);
    pipeline
        .advance_to(TransferPhase::FilterExchange)
        .expect("advance to FilterExchange");
    pipeline
        .advance_to(TransferPhase::FileListTransfer)
        .expect("advance to FileListTransfer");
    pipeline
}

/// A symlink-to-directory escaping the transfer root must be enumerated as
/// a symlink entry only - never followed into the outside tree - so the
/// metadata of files under the symlink target never reaches the client.
#[test]
fn sender_flist_does_not_leak_symlink_target_outside_root() {
    let scratch = TempDir::new().expect("tempdir");

    // The transfer root the client is allowed to see.
    let module = scratch.path().join("module");
    let sub = module.join("sub");
    fs::create_dir_all(&sub).expect("mkdir module/sub");
    fs::write(module.join("real.txt"), b"in-module payload").expect("write real.txt");
    fs::write(sub.join("nested.txt"), b"nested payload").expect("write nested.txt");

    // The outside tree the attacker wants the daemon to enumerate. The
    // distinctive marker name makes a leak trivial to detect.
    let outside = scratch.path().join("outside");
    fs::create_dir_all(&outside).expect("mkdir outside");
    fs::write(
        outside.join("leak_marker.txt"),
        b"OUTSIDE_PROTECTED_FILE_USED_AS_LEAK_DETECTOR",
    )
    .expect("write leak_marker.txt");

    // The trap: a symlink inside the module pointing at the outside tree.
    symlink(&outside, module.join("cd")).expect("plant cd symlink");

    let config = recursive_generator_config(&module);
    let handshake = test_handshake();
    let pipeline = test_pipeline();
    let mut ctx = GeneratorContext::new(&handshake, config, pipeline);

    ctx.build_file_list(&[module.clone()])
        .expect("build_file_list");

    let file_list = ctx.file_list();
    let names: Vec<&str> = file_list.iter().map(|e| e.name()).collect();

    // Core invariant: nothing under the symlink target was enumerated.
    assert!(
        names.iter().all(|n| !n.contains("leak_marker")),
        "sender flist leaked an outside file into the wire list: {names:?}"
    );
    assert!(
        names.iter().all(|n| !n.contains("outside")),
        "sender flist leaked an outside path into the wire list: {names:?}"
    );

    // The legitimate in-module files must still be present.
    assert!(
        names.iter().any(|n| n.ends_with("real.txt")),
        "expected in-module real.txt to be enumerated: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.ends_with("nested.txt")),
        "expected in-module sub/nested.txt to be enumerated: {names:?}"
    );

    // The trap itself must appear as a symlink entry (recorded, not
    // descended), carrying its literal target rather than the target's
    // contents - exactly upstream's unfollowed-symlink behaviour.
    let cd = file_list
        .iter()
        .find(|e| e.name().ends_with("cd"))
        .expect("the cd symlink entry must be present in the file list");
    assert_eq!(
        cd.file_type(),
        FileType::Symlink,
        "cd must be recorded as a symlink, not a followed directory"
    );
    assert_eq!(
        cd.link_target().map(|t| t.as_path()),
        Some(outside.as_path()),
        "the symlink entry must carry its literal target, proving the \
         sender stored the link instead of walking into it"
    );
}
