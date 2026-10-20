//! A local copy reports every source it cannot open, the way upstream's sender
//! does, and only then summarises the run.
//!
//! upstream: sender.c:711-722 - each failed open prints
//! `send_files failed to open %s` through `rsyserr(FERROR_XFER, ...)` and the
//! loop moves on; `cleanup.c:217-218` then exits `RERR_PARTIAL` (23) and
//! `log.c:98` names it `some files/attrs were not transferred (see previous
//! errors)`. oc used to print only the first failure, folded into its final
//! line, and dropped every later one.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    if built.is_file() {
        return built;
    }
    PathBuf::from("oc-rsync")
}

/// Makes `path` unreadable; returns `false` after a loud skip notice when the
/// process can still read it (running with `CAP_DAC_OVERRIDE`).
fn make_unreadable(path: &Path) -> bool {
    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).expect("chmod 000");
    if fs::File::open(path).is_ok() {
        eprintln!("SKIP: {} is still readable (running privileged)", path.display());
        return false;
    }
    true
}

fn run(args: &[&str], source: &Path, dest: &Path) -> std::process::Output {
    Command::new(oc_rsync_binary())
        .args(args)
        .arg(format!("{}/", source.display()))
        .arg(format!("{}/", dest.display()))
        .output()
        .expect("run oc-rsync")
}

#[test]
fn each_unreadable_source_is_reported_then_the_run_is_summarised() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(&source).expect("mkdir src");
    fs::create_dir_all(&dest).expect("mkdir dst");
    fs::write(source.join("b1"), b"one").expect("write b1");
    fs::write(source.join("b2"), b"two").expect("write b2");
    fs::write(source.join("ok"), b"ok").expect("write ok");
    if !make_unreadable(&source.join("b1")) || !make_unreadable(&source.join("b2")) {
        return;
    }

    let output = run(&["-a"], &source, &dest);
    for name in ["b1", "b2"] {
        let _ = fs::set_permissions(source.join(name), fs::Permissions::from_mode(0o644));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(23), "stderr: {stderr}");
    for name in ["b1", "b2"] {
        let expected = format!(
            "rsync: [sender] send_files failed to open \"{}\": Permission denied (13)",
            source.join(name).display()
        );
        assert_eq!(
            stderr.lines().filter(|line| *line == expected).count(),
            1,
            "each unreadable source is reported exactly once: {stderr}"
        );
    }
    let last = stderr.lines().last().unwrap_or_default();
    assert!(
        last.contains("some files/attrs were not transferred (see previous errors) (code 23)"),
        "the run ends with the RERR_PARTIAL summary: {stderr}"
    );
    assert!(
        !stderr.contains("failed to copy file"),
        "no per-file cause is repeated in the summary: {stderr}"
    );
    assert_eq!(fs::read(dest.join("ok")).expect("ok copied"), b"ok");
}
