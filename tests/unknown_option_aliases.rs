//! `--tmp-dir`, `--no-b` and `--no-fake-super` are not rsync options.
//!
//! upstream: options.c spells them `temp-dir`, `no-backup` and (for the
//! negation) nothing at all - `fake-super` has no `no-` form (options.c:670-672).
//! popt reports `<opt>: unknown option` and the process exits RERR_SYNTAX (1),
//! on the client and on a server reached through `-M` alike.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn run(args: &[&str]) -> Output {
    Command::new(oc_rsync_binary())
        .args(args)
        .output()
        .expect("run oc-rsync")
}

/// A remote shell that drops its own options and the host, then runs the
/// server command from `cwd`, so a server that took an option for a path
/// would create it there.
fn write_rsh_shim(dir: &Path, cwd: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let script = dir.join("fake_rsh.sh");
    let body = format!(
        "#!/bin/sh\n\
         while [ $# -gt 0 ]; do\n\
         case \"$1\" in\n\
         -*) shift ;;\n\
         *) break ;;\n\
         esac\n\
         done\n\
         shift || true\n\
         cd '{}' || exit 1\n\
         exec \"$@\"\n",
        cwd.display()
    );
    fs::write(&script, body).expect("write rsh shim");
    let mut perms = fs::metadata(&script).expect("stat shim").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).expect("chmod shim");
    script
}

fn tree_entries(root: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(root)
        .expect("read dir")
        .map(|entry| entry.expect("dir entry").path())
        .collect();
    entries.sort();
    entries
}

#[test]
fn client_rejects_option_spellings_upstream_does_not_have() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::write(src.join("f"), b"x").expect("write file");
    let dest = temp.path().join("dest");
    let src_arg = format!("{}/", src.display());
    let dest_arg = dest.display().to_string();

    for option in ["--tmp-dir=/tmp", "--no-b", "--no-fake-super"] {
        let output = run(&["-a", option, &src_arg, &dest_arg]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{option}: {stderr}");
        // upstream: options.c:915 option_error() then main.c:1913 exit_cleanup.
        let mut lines = stderr.lines();
        assert_eq!(
            lines.next(),
            Some(format!("oc-rsync: {option}: unknown option").as_str()),
            "{stderr}"
        );
        assert!(
            lines.next().is_some_and(
                |line| line.starts_with("oc-rsync error: syntax or usage error (code 1) at ")
            ),
            "{stderr}"
        );
        assert_eq!(lines.next(), None, "{stderr}");
        assert!(!dest.exists(), "{option}: nothing may be transferred");
    }
}

/// `-M--no-fake-super` reaches the server argv after the flag string. The
/// server must refuse it rather than take it for the destination, which would
/// create `--no-fake-super/` in its cwd and report success.
#[test]
fn server_refuses_remote_no_fake_super_without_writing_anything() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::write(src.join("f"), b"x").expect("write file");
    let server_cwd = temp.path().join("server_cwd");
    fs::create_dir_all(&server_cwd).expect("create server cwd");
    let binary = oc_rsync_binary();
    let shim = write_rsh_shim(temp.path(), &server_cwd);

    let output = Command::new(&binary)
        .arg("-a")
        .arg("-M--no-fake-super")
        .arg("--rsh")
        .arg(&shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(format!("{}/", src.display()))
        .arg("host:dst/")
        .output()
        .expect("run oc-rsync");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_ne!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("--no-fake-super: unknown option"),
        "the server's refusal must reach the client: {stderr}"
    );
    assert!(
        tree_entries(&server_cwd).is_empty(),
        "the server wrote into its cwd: {:?}",
        tree_entries(&server_cwd)
    );
}
