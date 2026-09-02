//! `hosts allow` / `hosts deny` must be evaluated against the REAL peer on the
//! remote-shell daemon path, not a fabricated localhost.
//!
//! A daemon started behind a remote shell (`--server --daemon`, upstream's
//! `am_daemon < 0`) has no socket to interrogate, so upstream takes the peer
//! address from the environment and seeds it with `0.0.0.0`:
//!
//! ```c
//! /* clientname.c:63-78 */
//! if (am_daemon < 0) {              /* daemon over --rsh mode */
//!     strlcpy(ipaddr_buf, "0.0.0.0", sizeof ipaddr_buf);
//!     if ((env_str = getenv("REMOTE_HOST"))    != NULL
//!      || (env_str = getenv("SSH_CONNECTION")) != NULL
//!      || (env_str = getenv("SSH_CLIENT"))     != NULL
//!      || (env_str = getenv("SSH2_CLIENT"))    != NULL) {
//!         strlcpy(ipaddr_buf, env_str, sizeof ipaddr_buf);
//!         if ((p = strchr(ipaddr_buf, ' ')) != NULL) *p = '\0';
//!     }
//!     if (valid_ipaddr(ipaddr_buf, True)) return ipaddr_buf;
//! }
//! ```
//!
//! oc previously hardcoded `127.0.0.1:0` here, which made
//! `hosts allow = 127.0.0.1` - the canonical "local only" rule - admit every
//! client on earth, and made an allow-list naming real subnets reject every
//! legitimate one. The `0.0.0.0` seed is what closes it: that address matches
//! no realistic allow token, so an unconfigured remote-shell daemon denies.
//!
//! WHY these three cases and not one: a single denial case would also pass on a
//! daemon that denied everything, and a single admit case would pass on the old
//! fabricating build. The pair that discriminates is
//! [`loopback_allow_does_not_admit_a_remote_shell_client`] (must DENY - it
//! ADMITTED before this fix) together with
//! [`remote_host_env_admits_the_matching_allow_rule`] (must ADMIT - it would
//! have been DENIED before, since the fabricated peer was never `192.0.2.7`).
//! Each is the other's control, so neither can pass vacuously.
//!
//! Skip conditions (test passes with a printed reason):
//! - Not Unix (the remote-shell shim uses `/bin/sh`).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// An address that is neither loopback nor anything the harness runs on, so a
/// rule naming it can only match when the environment is genuinely consulted.
const PEER_IP: &str = "192.0.2.7";

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Writes a remote-shell shim that changes into the module directory (so the
/// daemon finds `rsyncd.conf` in its CWD, upstream `RSYNCD_USERCONF`) and execs
/// the server command it was handed.
fn write_rsh_shim(dir: &Path) -> PathBuf {
    let script = dir.join("rsh.sh");
    let body = format!(
        "#!/bin/sh\n\
         cd {} || exit 1\n\
         while [ $# -gt 0 ]; do\n\
         case \"$1\" in\n\
         -*) shift ;;\n\
         *) break ;;\n\
         esac\n\
         done\n\
         shift || true\n\
         exec \"$@\"\n",
        dir.display()
    );
    fs::write(&script, body).expect("write rsh shim");
    let mut perms = fs::metadata(&script).expect("stat shim").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).expect("chmod shim");
    script
}

/// Lays out a daemon module whose `hosts allow` names exactly `allow`.
fn setup(allow: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let module = root.join("mod");
    fs::create_dir_all(&module).expect("mkdir module");
    fs::write(module.join("payload.txt"), b"payload\n").expect("write payload");
    fs::write(
        root.join("rsyncd.conf"),
        format!(
            "use chroot = false\n\
             [m]\n\
             \x20   path = {}\n\
             \x20   read only = true\n\
             \x20   hosts allow = {allow}\n",
            module.display()
        ),
    )
    .expect("write rsyncd.conf");
    (tmp, root)
}

/// Pulls from the module over a remote-shell daemon, optionally advertising a
/// peer address the way `sshd` would.
fn pull(root: &Path, peer_env: Option<(&str, &str)>) -> Output {
    let shim = write_rsh_shim(root);
    let dest = root.join("dest");
    fs::create_dir_all(&dest).expect("mkdir dest");

    let mut cmd = Command::new(oc_binary());
    cmd.current_dir(root)
        // upstream: flist.c:2723-2726 - a directory operand is skipped outright
        // ("skipping directory .") unless `xfer_dirs` is on, and `xfer_dirs`
        // comes from `-r`, `-d`, or `--list-only` (options.c:2314-2320). This
        // fixture's admit/deny signal is "did payload.txt arrive", so without
        // `-r` the admitted case would deliver nothing either and the control
        // could not discriminate. Verified against a real rsync 3.5.0 daemon.
        .arg("-r")
        .arg("-e")
        .arg(&shim)
        .arg("--rsync-path")
        .arg(oc_binary())
        .arg("fakehost::m/")
        .arg(&dest);

    // A remote shell that has no peer information in its environment must not
    // inherit one from the test runner.
    for name in ["REMOTE_HOST", "SSH_CONNECTION", "SSH_CLIENT", "SSH2_CLIENT"] {
        cmd.env_remove(name);
    }
    if let Some((name, value)) = peer_env {
        cmd.env(name, value);
    }

    cmd.output().expect("run oc-rsync")
}

fn transferred(output: &Output, dest: &Path) -> bool {
    output.status.success() && dest.join("payload.txt").is_file()
}

/// THE headline case. Before this fix the daemon fabricated `127.0.0.1`, so the
/// canonical "local only" rule matched a client that is not local at all.
///
/// upstream: clientname.c:65 seeds `0.0.0.0`, which matches no loopback token.
#[test]
fn loopback_allow_does_not_admit_a_remote_shell_client() {
    let (tmp, root) = setup("127.0.0.1");
    let out = pull(&root, None);
    assert!(
        !transferred(&out, &root.join("dest")),
        "`hosts allow = 127.0.0.1` must NOT admit a remote-shell client whose \
         peer address is unknown; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    drop(tmp);
}

/// The control for the case above: with a real peer advertised the way sshd
/// does it, the matching allow rule admits. Without this, the denial above
/// would also pass on a daemon that denied everything unconditionally.
///
/// upstream: clientname.c:66 - `REMOTE_HOST` is the first variable consulted.
#[test]
fn remote_host_env_admits_the_matching_allow_rule() {
    let (tmp, root) = setup(PEER_IP);
    let out = pull(&root, Some(("REMOTE_HOST", PEER_IP)));
    assert!(
        transferred(&out, &root.join("dest")),
        "REMOTE_HOST={PEER_IP} must satisfy `hosts allow = {PEER_IP}`; \
         stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    drop(tmp);
}

/// `SSH_CONNECTION` carries four space-separated fields; only the first is the
/// peer. A build that used the whole value would fail to parse it and deny.
///
/// upstream: clientname.c:70-74 - truncate at the first space.
#[test]
fn ssh_connection_env_is_truncated_to_the_peer_address() {
    let (tmp, root) = setup(PEER_IP);
    let out = pull(
        &root,
        Some((
            "SSH_CONNECTION",
            &format!("{PEER_IP} 54321 198.51.100.1 22"),
        )),
    );
    assert!(
        transferred(&out, &root.join("dest")),
        "SSH_CONNECTION's first field must be used as the peer; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    drop(tmp);
}

/// An allow-list naming a real subnet must still deny an unknown peer rather
/// than admitting it - the same defect in its other direction.
///
/// ⚠ This case does NOT discriminate on its own: `127.0.0.1` fails a
/// `198.51.100.0/24` rule too, so it passed on the fabricating build as well.
/// It is kept because it pins the second operator-visible symptom (an allow-list
/// of real subnets), not because it would have caught the defect.
#[test]
fn subnet_allow_does_not_admit_an_unknown_peer() {
    let (tmp, root) = setup("198.51.100.0/24");
    let out = pull(&root, None);
    assert!(
        !transferred(&out, &root.join("dest")),
        "an unknown peer must not satisfy a subnet allow rule; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    drop(tmp);
}
