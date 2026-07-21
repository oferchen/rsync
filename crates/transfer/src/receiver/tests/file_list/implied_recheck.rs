//! Receiver-side implied-include validation of received file-list names.
//!
//! CVE-2022-29154: a malicious rsync sender can append extra entries to the
//! file list that the client never requested, causing the receiver to write
//! files outside the intended set. Upstream records each requested source arg
//! as an implied include (`exclude.c:add_implied_include`) and rejects any
//! received name not covered by it (`flist.c:1026 recv_file_entry`,
//! `exit_cleanup(RERR_UNSUPPORTED)`). These tests encode that invariant: an
//! injected name aborts with exit code 4, while every legitimately requested
//! name (and its implied parent directories) passes. They also prove the
//! upstream skip conditions (`trust_sender_args`, no recorded source args).

use std::io;

use protocol::flist::FileEntry;

use super::super::super::ReceiverContext;
use super::super::support::{test_config, test_handshake};

#[test]
fn injected_name_rejected_with_exit_code_4() {
    // `oc-rsync -r host:dir dest` requests only `dir`; a sender that also
    // streams `evil` is exploiting CVE-2022-29154 and must be refused.
    let mut config = test_config();
    config.flags.recursive = true;
    config.connection.implied_source_args = vec!["dir".to_owned()];
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_directory("dir".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("dir/wanted.txt".into(), 10, 0o644));
    ctx.file_list
        .push(FileEntry::new_file("evil".into(), 20, 0o644));

    let err = ctx.recheck_received_implied_includes().unwrap_err();
    // io::ErrorKind::Unsupported maps to ExitCode::Unsupported (4), matching
    // upstream exit_cleanup(RERR_UNSUPPORTED).
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    assert_eq!(
        err.to_string(),
        "ERROR: rejecting unrequested file-list name: evil"
    );
}

#[test]
fn requested_names_and_subtree_pass() {
    let mut config = test_config();
    config.flags.recursive = true;
    config.connection.implied_source_args = vec!["dir".to_owned()];
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_directory("dir".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("dir/a.txt".into(), 10, 0o644));
    ctx.file_list
        .push(FileEntry::new_file("dir/sub/b.txt".into(), 10, 0o644));

    ctx.recheck_received_implied_includes()
        .expect("names under the requested directory must pass");
}

#[test]
fn relative_implied_parent_directories_pass() {
    // `-R host:a/b/c` keeps the full path and implies parents `a` and `a/b`;
    // those directory entries arrive in the list and must not be rejected.
    let mut config = test_config();
    config.flags.recursive = true;
    config.flags.relative = true;
    config.connection.implied_source_args = vec!["a/b/c".to_owned()];
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_directory("a".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_directory("a/b".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_directory("a/b/c".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("a/b/c/leaf".into(), 10, 0o644));

    ctx.recheck_received_implied_includes()
        .expect("implied parent directories of a relative arg must pass");
}

#[test]
fn relative_sibling_injection_rejected() {
    // The implied parent `a` is a directory rule only: a sibling file `a/evil`
    // next to the requested `a/b/c` was never requested and must be refused.
    let mut config = test_config();
    config.flags.recursive = true;
    config.flags.relative = true;
    config.connection.implied_source_args = vec!["a/b/c".to_owned()];
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.file_list
        .push(FileEntry::new_directory("a".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("a/evil".into(), 20, 0o644));

    let err = ctx.recheck_received_implied_includes().unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    assert_eq!(
        err.to_string(),
        "ERROR: rejecting unrequested file-list name: a/evil"
    );
}

#[test]
fn wildcard_arg_stays_active_and_admits_matching_names() {
    // upstream 3.4.4 does NOT disable the check for wildcard args; it builds a
    // FILTRULE_WILD rule (exclude.c:415). A `d*` request admits matching names
    // yet still rejects a non-matching injection.
    let mut config = test_config();
    config.flags.recursive = true;
    config.connection.implied_source_args = vec!["d*".to_owned()];
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.file_list
        .push(FileEntry::new_directory("data".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("data/file".into(), 10, 0o644));
    ctx.recheck_received_implied_includes()
        .expect("names matching the wildcard request must pass");

    ctx.file_list
        .push(FileEntry::new_file("evil".into(), 20, 0o644));
    let err = ctx.recheck_received_implied_includes().unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    assert_eq!(
        err.to_string(),
        "ERROR: rejecting unrequested file-list name: evil"
    );
}

#[test]
fn daemon_files_from_subdir_entry_passes_without_module_strip() {
    // Regression: `oc-rsync --files-from=LIST rsync://host/m/ dst/` selecting a
    // subdirectory entry `sub/d.txt`. The forwarded files-from entries are the
    // implied source args and are already module-relative, so upstream records
    // them with skip_daemon_module=0 (io.c:427,464) even on a daemon
    // connection. Stripping the leading path component would turn `sub/d.txt`
    // into `d.txt` and wrongly reject the arriving `sub/d.txt` as unrequested.
    // --files-from defaults relative_paths=1 and xfer_dirs=1 (options.c:2206,
    // 2620) with recursion off, so the received `sub` dir and `sub/d.txt` file
    // must both pass.
    let mut config = test_config();
    config.flags.relative = true;
    config.flags.dirs = true;
    config.flags.recursive = false;
    config.connection.is_daemon_connection = true;
    config.connection.implied_skip_daemon_module = false;
    config.connection.implied_source_args = vec!["a.txt".to_owned(), "sub/d.txt".to_owned()];
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("a.txt".into(), 10, 0o644));
    ctx.file_list
        .push(FileEntry::new_directory("sub".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("sub/d.txt".into(), 10, 0o644));

    ctx.recheck_received_implied_includes()
        .expect("a files-from subdir entry on a daemon pull must not be rejected");
}

#[test]
fn daemon_files_from_still_rejects_unrequested_name() {
    // The guard must remain intact: an entry the files-from list never named
    // (CVE-2022-29154) is still refused on a daemon files-from pull.
    let mut config = test_config();
    config.flags.relative = true;
    config.flags.dirs = true;
    config.flags.recursive = false;
    config.connection.is_daemon_connection = true;
    config.connection.implied_skip_daemon_module = false;
    config.connection.implied_source_args = vec!["a.txt".to_owned(), "sub/d.txt".to_owned()];
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.file_list
        .push(FileEntry::new_file("evil".into(), 20, 0o644));

    let err = ctx.recheck_received_implied_includes().unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    assert_eq!(
        err.to_string(),
        "ERROR: rejecting unrequested file-list name: evil"
    );
}

#[test]
fn daemon_module_operand_still_strips_module_name() {
    // A raw daemon `module/path` operand (no --files-from) keeps
    // skip_daemon_module=1 (main.c:1549): the module name `m` is stripped so
    // the requested `path` and its subtree are validated against the received
    // names, which arrive module-relative.
    let mut config = test_config();
    config.flags.recursive = true;
    config.connection.is_daemon_connection = true;
    config.connection.implied_skip_daemon_module = true;
    config.connection.implied_source_args = vec!["m/dir".to_owned()];
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.file_list
        .push(FileEntry::new_directory("dir".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("dir/file".into(), 10, 0o644));

    ctx.recheck_received_implied_includes()
        .expect("module-stripped daemon operand must admit its own subtree");
}

#[test]
fn trust_sender_skips_implied_check() {
    // upstream: options.c:2510 / exclude.c:385 - trust_sender_args makes
    // add_implied_include() a no-op, so the implied list is empty and the
    // receiver performs no name validation.
    let mut config = test_config();
    config.trust_sender = true;
    config.flags.recursive = true;
    config.connection.implied_source_args = vec!["dir".to_owned()];
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.file_list
        .push(FileEntry::new_file("evil".into(), 20, 0o644));

    ctx.recheck_received_implied_includes()
        .expect("trust_sender must skip the implied-include check");
}

#[test]
fn no_source_args_is_a_no_op() {
    // A push or server-receiver never records source args: the check must not
    // disturb a normal transfer.
    let mut config = test_config();
    config.flags.recursive = true;
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.file_list
        .push(FileEntry::new_file("anything".into(), 20, 0o644));

    ctx.recheck_received_implied_includes()
        .expect("no recorded source args means nothing to validate");
}
