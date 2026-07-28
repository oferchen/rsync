//! The `-e` capability string advertises `i` only for a recursive transfer.
//!
//! upstream: `compat.c:162-179 set_allow_inc_recurse()` runs
//! `if (!recurse || use_qsort) allow_inc_recurse = 0;` from `server_options()`
//! (options.c:2725), immediately before `maybe_add_e_option()` decides whether
//! to append `i` (options.c:3039). `allow_inc_recurse` initialises to 1
//! (options.c:114), so without that gate every non-recursive push advertises a
//! capability upstream withholds.
//!
//! `recurse` is a level, not a flag: `-r` sets 2 (options.c:621), `-a` sets 1
//! (options.c:1551), `--files-from` clears only level 1 (options.c:2188-2189)
//! and `--old-dirs` re-forces 1 afterwards (options.c:2197-2199). The gate is a
//! plain non-zero test, so `-r --files-from` still advertises `i` while
//! `-a --files-from` does not.
//!
//! Expectations below are the server argv rsync 3.4.4 (protocol 32) produced
//! for the same invocations, captured with a stub remote shell.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use test_support::oc_rsync_bin;

/// Writes a remote-shell stub that appends its argv to `log` and exits 0.
fn write_shell_stub(dir: &Path, log: &Path) -> std::path::PathBuf {
    let stub = dir.join("argv-stub.sh");
    fs::write(
        &stub,
        format!(
            "#!/bin/sh\nprintf '%s ' \"$@\" >> '{}'\nprintf '\\n' >> '{}'\nexit 0\n",
            log.display(),
            log.display()
        ),
    )
    .expect("write stub");
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).expect("chmod stub");
    stub
}

/// Runs `oc-rsync` against a stub remote shell and returns the compact flag
/// string it sent (the first single-dash argument of the server argv).
fn server_flag_string(extra: &[&str]) -> String {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    fs::create_dir_all(root.join("src/adir")).expect("src tree");
    fs::write(root.join("src/a.txt"), b"a").expect("src file");
    fs::write(root.join("src/adir/b.txt"), b"b").expect("nested file");
    fs::write(root.join("list.txt"), b"adir\n").expect("files-from list");

    let log = root.join("argv.log");
    let stub = write_shell_stub(root, &log);

    let mut cmd = Command::new(oc_rsync_bin());
    cmd.current_dir(root).arg("-e").arg(&stub);
    cmd.args(extra);
    cmd.arg("src/").arg("host:dst/");
    let output = cmd.output().expect("spawn oc-rsync");
    assert!(
        log.is_file(),
        "remote shell was never invoked for {extra:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let argv = fs::read_to_string(&log).expect("read argv log");
    argv.split_whitespace()
        .find(|a| a.starts_with('-') && !a.starts_with("--"))
        .unwrap_or_else(|| panic!("no compact flag string in server argv: {argv}"))
        .to_owned()
}

/// Returns the capability characters following `e.` in a compact flag string.
fn capabilities(flags: &str) -> &str {
    flags
        .split("e.")
        .nth(1)
        .unwrap_or_else(|| panic!("no capability suffix in {flags}"))
}

#[test]
fn non_recursive_push_omits_inc_recurse_capability() {
    // rsync 3.4.4: `--server -e.LsfxCIvu . dst/`
    for extra in [&[][..], &["-d"][..], &["--inc-recursive"][..]] {
        let flags = server_flag_string(extra);
        assert!(
            !capabilities(&flags).contains('i'),
            "non-recursive push with {extra:?} must not advertise 'i': {flags}"
        );
    }
}

#[test]
fn qsort_push_omits_inc_recurse_capability() {
    // rsync 3.4.4: `--server -re.LsfxCIvu --use-qsort . dst/`
    let flags = server_flag_string(&["-r", "--qsort"]);
    assert!(
        !capabilities(&flags).contains('i'),
        "--qsort must not advertise 'i' (upstream compat.c:172 use_qsort): {flags}"
    );
}

#[test]
fn recursive_push_still_advertises_inc_recurse_capability() {
    // rsync 3.4.4: `-r` -> `-re.iLsfxCIvu`, `-a` -> `-logDtpre.iLsfxCIvu`.
    for extra in [&["-r"][..], &["-a"][..]] {
        let flags = server_flag_string(extra);
        assert!(
            capabilities(&flags).contains('i'),
            "recursive push with {extra:?} must advertise 'i': {flags}"
        );
    }
}

#[test]
fn files_from_keeps_inc_recurse_only_for_explicit_recursion() {
    // rsync 3.4.4: `-r --files-from` -> `-rRe.iLsfxCIvu`, but
    // `-a --files-from` -> `-logDtpRe.LsfxCIvu` and a bare `--files-from` ->
    // `-Re.LsfxCIvu`, because only `recurse == 1` is cleared at options.c:2189.
    let explicit = server_flag_string(&["-r", "--files-from=list.txt"]);
    assert!(
        capabilities(&explicit).contains('i'),
        "an explicit -r survives --files-from and keeps 'i': {explicit}"
    );

    for extra in [
        &["-a", "--files-from=list.txt"][..],
        &["--files-from=list.txt"][..],
    ] {
        let flags = server_flag_string(extra);
        assert!(
            !capabilities(&flags).contains('i'),
            "{extra:?} clears recursion, so 'i' must go with it: {flags}"
        );
    }
}
