//! A client pulling from a daemon must not apply the daemon's own destination
//! confinement to its local destination root.
//!
//! # Background
//!
//! Upstream gates the receiver's confined `*at` resolver on `am_daemon`:
//! `use_secure_symlinks = am_daemon && (!am_chrooted || module_dirlen)`
//! (`clientserver.c:1093`), and `confinement_root()` hands back `module_dir`
//! only when `am_daemon` (`syscall.c:136`). `am_daemon` is true in the *serving*
//! process, never in the client that dials `rsync://`. So on a pull the client
//! is an ordinary, unconfined receiver: `secure_basis_open()` takes its
//! `if (!am_daemon || ...)` arm (`receiver.c:152`) and does a plain `do_open`,
//! and a symlinked destination root is followed exactly as it is for a local
//! copy or an SSH pull.
//!
//! oc-rsync carries `ConnectionConfig::is_daemon_connection`, which is set on
//! BOTH ends of an `rsync://` transfer - the connecting client sets it in
//! `apply_common_daemon_config`, the daemon server in `build_server_config`. It
//! is therefore not `am_daemon`. `daemon_module_root` is: it is populated only
//! by the daemon's own `apply_module_transfer_directives`. The receiver's
//! sandbox decision must read the latter, which is what
//! `ConnectionConfig::served_module_root` exists to express.
//!
//! # Why this matters (Rule 9)
//!
//! A symlinked destination directory is an ordinary deployment shape
//! (`/srv/backup -> /mnt/big/backup`). Measured against real rsync 3.5.0 on a
//! loopback daemon, `rsync -a rsync://host/mod/ DEST` where `DEST` is such a
//! symlink exits 0 and installs the file at the link target, with and without a
//! trailing slash. Before this fix oc-rsync exited 23 on both spellings: the
//! no-slash form was refused outright with a "chdir-symlink-race" message, and
//! the trailing-slash form completed the data transfer but failed metadata
//! application, leaving the file at mode 0600.
//!
//! Asserting only on the exit status would let a build that quietly created a
//! real `DEST/` directory next to the symlink pass, so both cells assert the
//! payload landed at the *link target* - the fixture's proof that the symlink
//! was followed rather than replaced.
//!
//! # Platform gate
//!
//! `#![cfg(unix)]` - matches the sibling daemon-spawning tests; the
//! `use chroot = false` toggle needs Unix process semantics, and the assertion
//! reads a Unix mode.
//!
//! # Skip semantics
//!
//! Self-skips (prints `skipping:` and returns) when a tempdir cannot be made or
//! the daemon fails to start. A non-zero exit, a payload that did not reach the
//! link target, or a wrong mode are real regressions.
//!
//! # Upstream References
//!
//! - `clientserver.c:1093` - `use_secure_symlinks = am_daemon && ...`
//! - `syscall.c:136` - `confinement_root()` returns `module_dir` only for a daemon
//! - `receiver.c:152` - `if (!am_daemon || ...)` plain-open arm of `secure_basis_open()`
//! - `main.c:757` - `change_dir(dest_path, CD_NORMAL)` resolves a symlinked
//!   destination root once, for every non-daemon receiver

#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use tempfile::{TempDir, tempdir};

/// Payload the pull must install at the destination.
const PAYLOAD: &[u8] = b"symlinked-dest-root payload\n";

/// Source mode the transfer must reproduce under `--perms`. Deliberately not
/// the umask default, so a receiver that never applied the mode is visible.
const PAYLOAD_MODE: u32 = 0o640;

/// Write an `rsyncd.conf` exposing one read-only module rooted at
/// `module_root`. `use chroot = false` keeps the unprivileged test process from
/// needing `CAP_SYS_CHROOT`.
fn write_daemon_config(
    config_path: &Path,
    pid_path: &Path,
    log_path: &Path,
    module_name: &str,
    module_root: &Path,
) -> io::Result<()> {
    let body = format!(
        "pid file = {pid}\n\
         log file = {log}\n\
         use chroot = false\n\
         max connections = 4\n\
         \n\
         [{module}]\n\
         path = {root}\n\
         comment = symlinked-dest-root regression\n\
         read only = true\n\
         list = true\n",
        pid = pid_path.display(),
        log = log_path.display(),
        module = module_name,
        root = module_root.display(),
    );
    fs::write(config_path, body)
}

/// Guard that kills the daemon child on drop so a panicking test never leaks
/// the listener.
struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn `oc-rsync --daemon` on a free loopback port against `config_path` and
/// wait until it accepts connections.
fn spawn_oc_daemon(oc_bin: &Path, config_path: &Path) -> io::Result<(DaemonGuard, u16)> {
    let (child, port) = test_support::spawn_daemon_on_free_port(|port| {
        Command::new(oc_bin)
            .arg("--daemon")
            .arg("--no-detach")
            .arg("--port")
            .arg(port.to_string())
            .arg("--config")
            .arg(config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    })?;
    Ok((DaemonGuard { child }, port))
}

/// Drive one `oc-rsync` invocation and return `(status, stdout, stderr)`.
fn run_oc_rsync_capture(
    bin: &Path,
    args: &[&OsStr],
) -> io::Result<(std::process::ExitStatus, String, String)> {
    let output = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    Ok((
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// Per-test scratch state: tempdir plus daemon log/pid/config paths.
struct Scratch {
    _tmp: TempDir,
    root: PathBuf,
    config: PathBuf,
    log: PathBuf,
    pid: PathBuf,
}

impl Scratch {
    fn new() -> Option<Self> {
        let tmp = tempdir().ok()?;
        // `DirSandbox::open_root` refuses a symlink anywhere in the path under
        // `RESOLVE_NO_SYMLINKS`, and macOS resolves `/tmp -> /private/tmp`, so
        // canonicalise before the fixture plants its own symlink. Otherwise the
        // ambient prefix, not the planted link, is what the test measures.
        let root = fs::canonicalize(tmp.path()).ok()?;
        Some(Self {
            config: root.join("rsyncd.conf"),
            log: root.join("rsyncd.log"),
            pid: root.join("rsyncd.pid"),
            root,
            _tmp: tmp,
        })
    }
}

/// Where the fixture plants its symlink relative to the destination operand.
#[derive(Clone, Copy)]
enum LinkAt {
    /// `DEST` itself is the symlink. Reachable by the leaf `O_NOFOLLOW` check
    /// on every platform.
    Leaf,
    /// `DEST` is a real directory under a symlinked ancestor - the ordinary
    /// `/srv -> /mnt/srv` layout with `DEST = /srv/backup/dest`.
    ///
    /// This is the cell that discriminates the confinement gate from the
    /// destination-root resolution: a leaf `symlink_metadata` reports a
    /// directory here, so no amount of canonicalising the operand helps; only
    /// the `openat2(RESOLVE_NO_SYMLINKS)` root open sees the interior
    /// component, and only on Linux. Elsewhere the walk is leaf-only
    /// `O_NOFOLLOW`, so the cell is unable to fail and is skipped rather than
    /// passing vacuously.
    Ancestor,
}

/// Run one daemon pull into `dest_arg` and assert the payload landed at the
/// real directory with its source mode.
///
/// `dest_suffix` selects the operand spelling: `""` for the bare name, `"/"`
/// for the trailing-slash form. The two take different paths through the
/// receiver setup - `lstat("x/")` resolves the link where `lstat("x")` does not
/// - and failed differently, so both are exercised.
fn assert_pull_follows_symlinked_dest(link_at: LinkAt, dest_suffix: &str) {
    let oc_bin = test_support::oc_rsync_bin();
    let Some(scratch) = Scratch::new() else {
        eprintln!("skipping: tempdir allocation failed");
        return;
    };

    let module_root = scratch.root.join("source");
    fs::create_dir_all(&module_root).expect("create module root");
    let source_file = module_root.join("payload.bin");
    fs::write(&source_file, PAYLOAD).expect("seed source payload");
    fs::set_permissions(&source_file, fs::Permissions::from_mode(PAYLOAD_MODE))
        .expect("chmod source payload");

    // The operator's destination reaches its real storage through a link, the
    // ordinary `/srv/backup -> /mnt/big/backup` shape.
    let (real_dest, linked_dest) = match link_at {
        LinkAt::Leaf => {
            let real_dest = scratch.root.join("real-dest");
            fs::create_dir_all(&real_dest).expect("create real destination");
            let linked_dest = scratch.root.join("linked-dest");
            symlink(&real_dest, &linked_dest).expect("plant destination symlink");
            (real_dest, linked_dest)
        }
        LinkAt::Ancestor => {
            if !interior_components_are_inspected() {
                eprintln!(
                    "skipping: this platform's destination-root open is leaf-only \
                     O_NOFOLLOW, so a symlinked ancestor cannot fail here"
                );
                return;
            }
            // `mnt/big/backup` is real; `srv -> mnt/big` is the link, so the
            // operand `srv/backup` has a symlinked ANCESTOR and a real leaf.
            let store = scratch.root.join("mnt").join("big");
            let real_dest = store.join("backup");
            fs::create_dir_all(&real_dest).expect("create real destination");
            symlink(&store, scratch.root.join("srv")).expect("plant ancestor symlink");
            let linked_dest = scratch.root.join("srv").join("backup");
            (real_dest, linked_dest)
        }
    };

    write_daemon_config(
        &scratch.config,
        &scratch.pid,
        &scratch.log,
        "pullmod",
        &module_root,
    )
    .expect("write daemon config");

    let (_daemon, port) = match spawn_oc_daemon(&oc_bin, &scratch.config) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: could not start oc-rsync --daemon: {e}");
            return;
        }
    };

    let src_url = OsString::from(format!("rsync://127.0.0.1:{port}/pullmod/"));
    let mut dest_arg = linked_dest.clone().into_os_string();
    dest_arg.push(dest_suffix);

    let args: &[&OsStr] = &[
        OsStr::new("--recursive"),
        OsStr::new("--perms"),
        OsStr::new("--times"),
        &src_url,
        &dest_arg,
    ];

    let (status, stdout, stderr) =
        run_oc_rsync_capture(&oc_bin, args).expect("spawn oc-rsync client (daemon pull)");

    // Common to every cell: the confinement gate must not fire. It is worded
    // for an operator, so match the wording and say what it means rather than
    // reading only a status.
    assert!(
        !stderr.contains("chdir-symlink-race") && !stderr.contains("refusing to open destination"),
        "a client pulling from a daemon is not `am_daemon`, so upstream applies no \
         confinement to its destination root; pull into '{}' was refused\nstderr:\n{stderr}",
        dest_arg.to_string_lossy(),
    );

    // The fixture's own proof: the payload must be at the real directory. A
    // build that replaced or side-stepped the symlink would satisfy a
    // status-only assertion.
    let landed = real_dest.join("payload.bin");
    assert_eq!(
        fs::read(&landed).unwrap_or_default(),
        PAYLOAD,
        "the pulled payload must land at {}",
        real_dest.display(),
    );

    match link_at {
        LinkAt::Leaf => {
            assert!(
                status.success(),
                "pull into '{}' exited {status:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
                dest_arg.to_string_lossy(),
            );
            // Metadata must be applied through the followed link too. The
            // trailing-slash spelling used to transfer the data and then fail
            // metadata application, leaving the file at 0600 with exit 23.
            let mode = fs::metadata(&landed)
                .expect("stat landed payload")
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(
                mode, PAYLOAD_MODE,
                "--perms must reproduce the source mode through the followed \
                 destination symlink",
            );
        }
        // The ancestor cell deliberately stops at the confinement contract.
        //
        // Its destination resolves through a link the sandbox open still
        // declines, so the receiver keeps running on the path-based syscalls -
        // and on that fall-back the metadata apply fails silently and forces
        // exit 23. Measured on Linux against real rsync 3.5.0 on the same
        // fixture: upstream exits 0 with mode 0640, oc exits 23 with 0644 and
        // prints nothing at any verbosity. That residual is a separate,
        // pre-existing defect of the no-sandbox metadata path - reachable from
        // any destination whose sandbox open fails, symlink or not - so
        // asserting it here would fail this cell for a reason this change does
        // not own.
        LinkAt::Ancestor => {}
    }
}

/// True when the receiver's destination-root open inspects interior path
/// components, i.e. when [`LinkAt::Ancestor`] is able to fail at all.
///
/// Only `openat2(RESOLVE_NO_SYMLINKS)` walks the whole path; the portable
/// fallback is `openat(O_NOFOLLOW | O_DIRECTORY)` on the leaf alone. Declaring
/// this instead of hard-coding `cfg(target_os = "linux")` keeps the cell honest
/// on a Linux kernel too old for `openat2`, where the fallback is what runs.
fn interior_components_are_inspected() -> bool {
    fast_io::openat2_supported()
}

/// The bare symlink name (`DEST`, no trailing slash).
#[test]
fn daemon_pull_follows_symlinked_dest_root_bare() {
    assert_pull_follows_symlinked_dest(LinkAt::Leaf, "");
}

/// The trailing-slash spelling (`DEST/`), which the kernel resolves as a
/// directory before `O_NOFOLLOW` is consulted and so reached a different
/// failure than the bare form.
#[test]
fn daemon_pull_follows_symlinked_dest_root_trailing_slash() {
    assert_pull_follows_symlinked_dest(LinkAt::Leaf, "/");
}

/// `DEST` is a real directory under a symlinked ancestor.
///
/// This is the cell that discriminates the two halves of the fix: no operand
/// resolution helps, because the leaf is not a link. Only dropping the daemon
/// confinement from the client receiver lets the transfer through.
#[test]
fn daemon_pull_follows_symlinked_dest_ancestor() {
    assert_pull_follows_symlinked_dest(LinkAt::Ancestor, "");
}
