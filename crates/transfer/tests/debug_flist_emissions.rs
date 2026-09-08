//! `--debug=flist` emission parity through the live flist send/receive paths.
//!
//! Upstream rsync's `DEBUG_GTE(FLIST, n)` sites (flist.c, io.c, rsync.c,
//! generator.c, main.c, util1.c) were measured against the pinned 3.5.0 tree;
//! each emission has exactly one oc owner in `protocol::flist::trace`. These
//! tests drive the REAL paths - `GeneratorContext::build_file_list` +
//! `send_file_list` and `ReceiverContext::receive_file_list` over the bytes
//! the sender produced - with the verbosity installed through
//! `logging::apply_debug_flag`, the same funnel the CLI's `--debug=` parser
//! uses (cli/frontend/execution/drive/options.rs:265), and assert the EXACT
//! set of `DebugFlag::Flist` messages per level.
//!
//! Level anchors (measured, upstream 3.5.0 on the same fixture shape):
//!
//! - FLIST1 sender/receiver flist paths emit NOTHING (the only level-1 line,
//!   `delta-transmission %s` from generator.c:2763, belongs to transfer setup
//!   and is pinned by `receiver/transfer/phases.rs` tests).
//! - FLIST2 sender: one `[sender] make_file(%s,*,%d)` per walked entry
//!   (flist.c:1542; filter level 0 for the named source, 2 for recursed
//!   entries) and one `send_file_list done` (flist.c:2838).
//! - FLIST2 receiver: one `recv_file_name(%s)` per entry (flist.c:3012), one
//!   `received %d names` (flist.c:3019), one `recv_file_list done`
//!   (flist.c:3088).
//! - FLIST3 adds `[%s] flist_eof=1` (flist.c:2861/:3058) and the
//!   `output_flist()` dump (flist.c:3489).

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig};
use protocol::ProtocolVersion;
use transfer::{
    GeneratorContext, HandshakeResult, ReceiverContext, ServerConfig, ServerRole, TransferPhase,
    TransferPipeline,
};

/// Installs a default verbosity and applies one `--debug=` token through the
/// live parse funnel, then clears any events buffered by earlier tests on
/// this thread.
fn init_debug(token: &str) {
    logging::init(VerbosityConfig::default());
    logging::apply_debug_flag(token).expect("valid debug token");
    let _ = logging::drain_events();
}

/// Drains and returns the `DebugFlag::Flist` messages emitted on this thread.
fn flist_messages() -> Vec<String> {
    logging::drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Flist,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect()
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

fn pipeline_for(role: ServerRole) -> TransferPipeline {
    let mut pipeline = TransferPipeline::new(role);
    pipeline
        .advance_to(TransferPhase::FilterExchange)
        .expect("advance to FilterExchange");
    pipeline
        .advance_to(TransferPhase::FileListTransfer)
        .expect("advance to FileListTransfer");
    pipeline
}

/// The handover fixture shape: `src/{a.txt, sub/b.txt}`, returned with the
/// trailing-slash spelling (`src/`) so the transfer root is the directory's
/// CONTENTS and the dot entry is emitted, as in the measured cells.
fn build_fixture(scratch: &TempDir) -> PathBuf {
    let src = scratch.path().join("src");
    fs::create_dir_all(src.join("sub")).expect("mkdir src/sub");
    fs::write(src.join("a.txt"), b"hello\n").expect("write a.txt");
    fs::write(src.join("sub").join("b.txt"), b"world\n").expect("write b.txt");
    PathBuf::from(format!("{}/", src.display()))
}

fn sender_config(src: &Path) -> ServerConfig {
    ServerConfig::from_flag_string_and_args(
        ServerRole::Generator,
        "-r.iLsfxCIvu".to_owned(),
        vec![src.to_path_buf().into_os_string()],
    )
    .expect("sender config")
}

fn receiver_config(dest: &Path) -> ServerConfig {
    ServerConfig::from_flag_string_and_args(
        ServerRole::Receiver,
        "-r.iLsfxCIvu".to_owned(),
        vec![dest.to_path_buf().into_os_string()],
    )
    .expect("receiver config")
}

/// Builds and wire-encodes the fixture's file list through the live sender
/// path, returning the wire bytes.
fn send_fixture_flist(src: &Path) -> Vec<u8> {
    let handshake = test_handshake();
    let mut ctx = GeneratorContext::new(
        &handshake,
        sender_config(src),
        pipeline_for(ServerRole::Generator),
    );
    ctx.build_file_list(&[src.to_path_buf()])
        .expect("build_file_list");
    let mut wire = Vec::new();
    ctx.send_file_list(&mut wire).expect("send_file_list");
    wire
}

/// upstream cell: `--debug=flist1` push shows no sender-side flist lines
/// (the level-1 `delta-transmission` line belongs to the generator setup).
#[test]
fn flist1_sender_path_is_silent() {
    let scratch = TempDir::new().expect("tempdir");
    let src = build_fixture(&scratch);
    init_debug("flist1");
    let _wire = send_fixture_flist(&src);
    assert_eq!(
        flist_messages(),
        Vec::<String>::new(),
        "the sender flist path has no level-1 FLIST emissions"
    );
}

/// upstream cell: `--debug=flist2` push prints one make_file() line per
/// entry (level 0 for the named source, 2 for recursed entries) and
/// `send_file_list done` - nothing else.
#[test]
fn flist2_sender_emissions_exact() {
    let scratch = TempDir::new().expect("tempdir");
    let src = build_fixture(&scratch);
    init_debug("flist2");
    let _wire = send_fixture_flist(&src);

    let mut messages = flist_messages();
    // The walk order of sibling entries is filesystem-dependent; the SET is not.
    messages.sort();
    let mut expected = vec![
        "[sender] make_file(.,*,0)".to_owned(),
        "[sender] make_file(a.txt,*,2)".to_owned(),
        "[sender] make_file(sub,*,2)".to_owned(),
        "[sender] make_file(sub/b.txt,*,2)".to_owned(),
        "send_file_list done".to_owned(),
    ];
    expected.sort();
    assert_eq!(messages, expected);
}

/// upstream: flist.c:2835/2861 - at level 3 the sender additionally dumps the
/// flist and prints `[sender] flist_eof=1` (non-incremental list).
#[test]
fn flist3_sender_adds_dump_and_eof() {
    let scratch = TempDir::new().expect("tempdir");
    let src = build_fixture(&scratch);
    init_debug("flist3");
    let _wire = send_fixture_flist(&src);

    let messages = flist_messages();
    let count = |pred: &dyn Fn(&str) -> bool| messages.iter().filter(|m| pred(m)).count();
    assert_eq!(count(&|m| m == "[sender] flist_eof=1"), 1, "{messages:?}");
    assert_eq!(
        count(&|m| m.starts_with("[sender] flist start=0, used=4,")),
        1,
        "{messages:?}"
    );
    assert_eq!(
        count(&|m| m.starts_with("[sender] i=")),
        4,
        "one output_flist line per entry: {messages:?}"
    );
    // upstream: flist.c:3524 - the sender's root column is F_PATHNAME (the
    // source base, without the operand's trailing slash), and a directory
    // prints a trailing slash after its name.
    let base = src.display().to_string();
    let a_line = format!(
        "[sender] i=1 {} a.txt mode=0100644 len=6",
        base.trim_end_matches('/')
    );
    assert_eq!(
        count(&|m| m.starts_with(&a_line)),
        1,
        "expected {a_line:?} in {messages:?}"
    );
}

/// upstream cell: `--debug=flist1` pull shows no receiver flist-path lines.
#[test]
fn flist1_receiver_path_is_silent() {
    let scratch = TempDir::new().expect("tempdir");
    let src = build_fixture(&scratch);
    let wire = send_fixture_flist(&src);

    let dest = scratch.path().join("dest");
    fs::create_dir_all(&dest).expect("mkdir dest");
    init_debug("flist1");
    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new(
        &handshake,
        receiver_config(&dest),
        pipeline_for(ServerRole::Receiver),
    );
    ctx.receive_file_list(&mut wire.as_slice())
        .expect("receive_file_list");
    assert_eq!(
        flist_messages(),
        Vec::<String>::new(),
        "the receiver flist path has no level-1 FLIST emissions"
    );
}

/// upstream cell: `--debug=flist2` pull prints `recv_file_name(%s)` per
/// entry, then `received %d names`, then `recv_file_list done` - exactly.
#[test]
fn flist2_receiver_emissions_exact() {
    let scratch = TempDir::new().expect("tempdir");
    let src = build_fixture(&scratch);
    let wire = send_fixture_flist(&src);

    let dest = scratch.path().join("dest");
    fs::create_dir_all(&dest).expect("mkdir dest");
    init_debug("flist2");
    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new(
        &handshake,
        receiver_config(&dest),
        pipeline_for(ServerRole::Receiver),
    );
    let count = ctx
        .receive_file_list(&mut wire.as_slice())
        .expect("receive_file_list");
    assert_eq!(count, 4);

    // The wire carries the sender's sorted order: ".", files before dirs.
    assert_eq!(
        flist_messages(),
        vec![
            "recv_file_name(.)".to_owned(),
            "recv_file_name(a.txt)".to_owned(),
            "recv_file_name(sub)".to_owned(),
            "recv_file_name(sub/b.txt)".to_owned(),
            "received 4 names".to_owned(),
            "recv_file_list done".to_owned(),
        ]
    );
}

/// upstream: flist.c:3058/3085 - level 3 adds `[Receiver] flist_eof=1` (the
/// pre-forked receiver capitalizes, rsync.c:994) and the output_flist dump.
#[test]
fn flist3_receiver_adds_dump_and_eof() {
    let scratch = TempDir::new().expect("tempdir");
    let src = build_fixture(&scratch);
    let wire = send_fixture_flist(&src);

    let dest = scratch.path().join("dest");
    fs::create_dir_all(&dest).expect("mkdir dest");
    init_debug("flist3");
    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new(
        &handshake,
        receiver_config(&dest),
        pipeline_for(ServerRole::Receiver),
    );
    ctx.receive_file_list(&mut wire.as_slice())
        .expect("receive_file_list");

    let messages = flist_messages();
    let count = |pred: &dyn Fn(&str) -> bool| messages.iter().filter(|m| pred(m)).count();
    assert_eq!(count(&|m| m == "[Receiver] flist_eof=1"), 1, "{messages:?}");
    assert_eq!(
        count(&|m| m.starts_with("[Receiver] flist start=0, used=4,")),
        1,
        "{messages:?}"
    );
    assert_eq!(
        count(&|m| m.starts_with("[Receiver] i=")),
        4,
        "one output_flist line per entry: {messages:?}"
    );
    // upstream: flist.c:3524 - the receiver's root column is the entry depth;
    // with neither -o nor root, no uid column appears.
    assert_eq!(
        count(&|m| m.starts_with("[Receiver] i=1 1 a.txt mode=0100644 len=6")),
        1,
        "{messages:?}"
    );
}
