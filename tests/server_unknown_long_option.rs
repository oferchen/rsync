//! A server refuses a long option it does not know instead of taking it for a
//! path.
//!
//! upstream: the server parses its argv with the same popt table as the client
//! (options.c:1497), so `-M--bogus-opt` makes it print
//! `--bogus-opt: unknown option` and exit RERR_SYNTAX (1) before it touches
//! the destination; the client then exits 12. A real path never reaches the
//! server looking like an option: `safe_arg()` sends a filename that starts
//! with `-` as `./-...` (options.c:2709).

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
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

struct Fixture {
    _temp: tempfile::TempDir,
    src_arg: String,
    server_cwd: PathBuf,
    shim: PathBuf,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::write(src.join("f"), b"x").expect("write file");
    let server_cwd = temp.path().join("server_cwd");
    fs::create_dir_all(&server_cwd).expect("create server cwd");
    let shim = write_rsh_shim(temp.path(), &server_cwd);
    Fixture {
        src_arg: format!("{}/", src.display()),
        server_cwd,
        shim,
        _temp: temp,
    }
}

fn push(fixture: &Fixture, extra: &[&str], dest: &str) -> Output {
    let binary = oc_rsync_binary();
    Command::new(&binary)
        .arg("-a")
        .args(extra)
        .arg("--rsh")
        .arg(&fixture.shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(&fixture.src_arg)
        .arg(dest)
        .output()
        .expect("run oc-rsync")
}

fn entries(root: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(root)
        .expect("read dir")
        .map(|entry| entry.expect("dir entry").path())
        .collect();
    entries.sort();
    entries
}

#[test]
fn server_refuses_an_unknown_remote_long_option_without_writing_anything() {
    let fixture = fixture();

    let output = push(&fixture, &["-M--bogus-opt"], "host:dst/");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(12), "{stderr}");
    // upstream: options.c:2053 prefixes the popt text with `on remote
    // machine: ` when am_server, then main.c:1913 exits RERR_SYNTAX.
    let mut lines = stderr.lines();
    assert_eq!(
        lines.next(),
        Some("oc-rsync: on remote machine: --bogus-opt: unknown option"),
        "{stderr}"
    );
    assert!(
        lines.next().is_some_and(|line| line
            .starts_with("oc-rsync error: syntax or usage error (code 1) at ")
            && line.contains(" [server=")),
        "{stderr}"
    );
    assert!(
        entries(&fixture.server_cwd).is_empty(),
        "the server took the option for a path: {:?}",
        entries(&fixture.server_cwd)
    );
}

/// A long option the server does know still passes: `--protocol=N` is an
/// ordinary server option (options.c:860), not a path and not a refusal.
#[test]
fn a_known_remote_long_option_is_still_accepted() {
    let fixture = fixture();

    let output = push(&fixture, &["-M--protocol=30"], "host:dst/");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read(fixture.server_cwd.join("dst").join("f")).expect("transferred file"),
        b"x"
    );
    assert_eq!(
        entries(&fixture.server_cwd),
        vec![fixture.server_cwd.join("dst")]
    );
}

/// The refusal must not reach real paths: a destination that starts with
/// `--` travels as `./--...` and lands where upstream puts it.
#[test]
fn a_destination_that_looks_like_a_long_option_is_still_a_path() {
    let fixture = fixture();

    let output = push(&fixture, &[], "host:--weird/");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read(fixture.server_cwd.join("--weird").join("f")).expect("transferred file"),
        b"x"
    );
}
