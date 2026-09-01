//! A `--relative` implied parent must never be built through a symlink that
//! leaves the daemon's module root.
//!
//! The `-R` implied parents of `a/b/c/file.txt` are `a/`, `a/b/` and `a/b/c/`.
//! Upstream builds every one of them through a confined mkdir. The file list's
//! own directory pass uses `gen_entry_mkdir()`, which classifies the
//! destination first and DELETES a conflicting symlink before creating a real
//! directory in its place:
//!
//! ```c
//! /* generator.c:1451-1455 */
//! delete_item(fname, sx.st.st_mode, del_opts | DEL_FOR_DIR);
//! /* generator.c:1873 */
//! gen_entry_mkdir(fname, file, file->mode|added_perms)
//! ```
//!
//! and the `relative_paths` fallback at `generator.c:1718-1725` uses
//! `make_path()`, whose every component goes through `do_mkdir_at()`
//! (`util1.c:238`, `util1.c:277`) - the ownership walk plus `mkdirat()`, not a
//! bare `mkdir(2)` (`syscall.c:2066`).
//!
//! oc had a third, unconfined creator: `ensure_relative_parents` ran as a
//! PRE-pass over the file list and built every implied parent with a
//! path-based `std::fs::create_dir`. Path-based creation follows a symlinked
//! component, and running first meant it always won the race with the confined
//! creator that was supposed to handle the same directory. Measured on a
//! daemon push where the module root holds `a -> <outside the module>`:
//!
//! ```text
//! $ oc-rsync -a -i -R ./a/b/c/file.txt rsync://127.0.0.1:PORT/data/
//! cd+++++++++ a/
//! ...
//! client exit=0
//! --- outside/ ---
//! outside/b            <-- created OUTSIDE the module root
//! outside/b/c          <-- created OUTSIDE the module root
//! outside/marker
//! ```
//!
//! rsync 3.5.0 on the same fixture leaves `outside/` holding only its marker,
//! and so does a fixed oc.
//!
//! ⚠ The escape cell opts out of oc's Landlock and seccomp layers. Landlock
//! refuses this write on its own, BEFORE oc's operator-path resolution is
//! consulted, so a cell run under the sandbox would measure the kernel's
//! refusal and stay green whether or not this crate resolves the path
//! correctly. The availability cell keeps the layers on. Opting out is a
//! test-only narrowing; the shipped daemon installs both layers by default.
//!
//! Skip conditions (test passes with a printed reason):
//! - The `oc-rsync` binary is not built.
//! - The daemon could not be started.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// Seeded in the out-of-module directory so a stray creation there is
/// distinguishable from a fixture that named the wrong directory.
const OUTSIDE_MARKER: &str = "OUTSIDE-THE-MODULE-UNTOUCHED";
/// The pushed file's contents.
const PAYLOAD: &str = "PAYLOAD-FROM-THE-PEER\n";

struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// What stands at the first implied parent (`a`) inside the module root.
#[derive(Clone, Copy)]
enum Plant {
    /// A trusted-owned directory symlink pointing OUTSIDE the module root.
    DirSymlinkOutsideModule,
    /// A trusted-owned directory symlink pointing to another directory INSIDE
    /// the module root: the over-refusal control.
    DirSymlinkInsideModule,
}

/// Whether the spawned daemon worker installs oc's kernel sandbox layers.
#[derive(Clone, Copy)]
enum Sandbox {
    /// Landlock and seccomp installed, as a production daemon runs them.
    Enforced,
    /// Both layers opted out, so any refusal observed must be oc's own.
    OptedOut,
}

impl Sandbox {
    fn daemon_env(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Enforced => &[],
            Self::OptedOut => &[("OC_RSYNC_NO_LANDLOCK", "1"), ("OC_RSYNC_NO_SECCOMP", "1")],
        }
    }
}

/// The filesystem state the assertions are made against.
struct Outcome {
    /// Every path under the out-of-module directory, sorted. An escape shows up
    /// here as extra entries beside the marker.
    outside: Vec<PathBuf>,
    /// Every path under the module root, sorted: the availability half.
    inside: Vec<PathBuf>,
    client_exit: Option<i32>,
    client_stderr: String,
    daemon_log: String,
}

impl Outcome {
    fn diagnostics(&self) -> String {
        format!(
            "\noutside: {:?}\ninside: {:?}\nstderr:\n{}\ndaemon log:\n{}",
            self.outside, self.inside, self.client_stderr, self.daemon_log
        )
    }
}

fn tree(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                stack.push(path.clone());
            }
            out.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
        }
    }
    out.sort();
    out
}

fn write_config(config: &Path, module_root: &Path, log_root: &Path) -> std::io::Result<()> {
    fs::write(
        config,
        format!(
            "pid file = {pid}\n\
             log file = {log}\n\
             use chroot = false\n\
             \n\
             [data]\n\
             path = {root}\n\
             read only = false\n",
            pid = log_root.join("rsyncd.pid").display(),
            log = log_root.join("rsyncd.log").display(),
            root = module_root.display(),
        ),
    )
}

fn spawn_daemon(
    oc_bin: &Path,
    config: &Path,
    sandbox: Sandbox,
) -> std::io::Result<(DaemonGuard, u16)> {
    let (child, port) = test_support::spawn_daemon_on_free_port(|port| {
        let mut cmd = Command::new(oc_bin);
        cmd.arg("--daemon")
            .arg("--no-detach")
            .arg("--port")
            .arg(port.to_string())
            .arg("--config")
            .arg(config)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in sandbox.daemon_env() {
            cmd.env(key, value);
        }
        cmd.spawn()
    })?;
    Ok((DaemonGuard(child), port))
}

/// Stages the module with `plant` at `a`, pushes `./a/b/c/file.txt` with `-R`
/// plus `extra` over the daemon, and reports the resulting filesystem state.
fn push_relative(plant: Plant, extra: &[&str], sandbox: Sandbox) -> Option<Outcome> {
    let oc_bin = test_support::oc_rsync_bin();
    if !oc_bin.is_file() {
        eprintln!("skip: oc-rsync binary not built at {}", oc_bin.display());
        return None;
    }
    let tmp = test_support::create_tempdir();
    let root = tmp.path();

    let source_dir = root.join("src");
    let module_root = root.join("module");
    let outside_dir = root.join("outside");
    fs::create_dir_all(source_dir.join("a/b/c")).expect("create source tree");
    fs::create_dir_all(&module_root).expect("create module root");
    fs::create_dir_all(&outside_dir).expect("create the out-of-module dir");
    fs::write(source_dir.join("a/b/c/file.txt"), PAYLOAD).expect("seed source file");
    fs::write(outside_dir.join("marker"), OUTSIDE_MARKER).expect("seed the out-of-module marker");

    let inside_target = module_root.join("realdir");
    let planted = module_root.join("a");
    match plant {
        Plant::DirSymlinkOutsideModule => {
            symlink(&outside_dir, &planted).expect("plant the out-of-module dir symlink");
        }
        Plant::DirSymlinkInsideModule => {
            fs::create_dir(&inside_target).expect("create the in-module target");
            symlink(&inside_target, &planted).expect("plant the in-module dir symlink");
        }
    }

    // The plant must be TRUSTED-owned, or a refusal would only be the ownership
    // arm firing and would prove nothing about the module root.
    let meta = fs::symlink_metadata(&planted).expect("the plant must exist");
    assert!(
        fast_io::symlink_owner_is_trusted(std::os::unix::fs::MetadataExt::uid(&meta)),
        "the planted directory must be owned by uid 0 or our euid; got uid {}",
        std::os::unix::fs::MetadataExt::uid(&meta),
    );

    let config = root.join("rsyncd.conf");
    write_config(&config, &module_root, root).expect("write daemon config");
    let (_daemon, port) = spawn_daemon(&oc_bin, &config, sandbox).ok()?;

    let destination_url = format!("rsync://127.0.0.1:{port}/data/");
    let output = Command::new(&oc_bin)
        .current_dir(&source_dir)
        .arg("-a")
        .arg("-i")
        .arg("-R")
        .args(extra)
        .arg("./a/b/c/file.txt")
        .arg(&destination_url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run oc-rsync client");

    Some(Outcome {
        outside: tree(&outside_dir),
        inside: tree(&module_root),
        client_exit: output.status.code(),
        client_stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        daemon_log: fs::read_to_string(root.join("rsyncd.log")).unwrap_or_default(),
    })
}

/// Asserts nothing was deposited outside the module root, non-vacuously.
fn assert_nothing_escaped(outcome: &Outcome, what: &str) {
    // NON-VACUITY first: an untouched out-of-module directory is exactly what a
    // session that died before the directory pass would leave behind, so
    // without this the pin below could not tell a refusal from a dead session.
    assert!(
        outcome.daemon_log.contains("receiving file list"),
        "{what}: the daemon never reached the transfer, so the assertion below \
         would hold vacuously rather than describe a refusal{}",
        outcome.diagnostics(),
    );
    assert_eq!(
        outcome.outside,
        vec![PathBuf::from("marker")],
        "{what}: a --relative implied parent was created OUTSIDE the module \
         root (client exit {:?}){}",
        outcome.client_exit,
        outcome.diagnostics(),
    );
}

/// THE PIN, at the default `-R` shape. The file list carries the implied
/// parents as directory entries, so the confined directory pass owns them and
/// nothing may create them ahead of it.
#[test]
fn relative_implied_parents_must_not_be_built_outside_the_module() {
    let Some(outcome) = push_relative(Plant::DirSymlinkOutsideModule, &[], Sandbox::OptedOut)
    else {
        return;
    };
    assert_nothing_escaped(&outcome, "default -R");
}

/// THE SAME PIN where `ensure_relative_parents` is the SOLE creator.
///
/// `--no-implied-dirs` below protocol 30 omits the implied parents from the
/// file list entirely (`flist.c:2468`, mirrored at
/// `generator/file_list/mod.rs:134`), so the directory pass has nothing to
/// create and the `make_path()` equivalent is the only thing that builds `a/b`.
/// Without this cell the ordering fix alone would keep the pin above green and
/// the unconfined `create_dir` inside `ensure_relative_parents` would kill
/// nothing - measured: reverting just the confined mkdir leaves the cell above
/// passing and fails only this one.
#[test]
fn a_sole_creator_relative_parent_must_not_escape_the_module() {
    let Some(outcome) = push_relative(
        Plant::DirSymlinkOutsideModule,
        &["--no-implied-dirs", "--protocol", "29"],
        Sandbox::OptedOut,
    ) else {
        return;
    };
    assert_nothing_escaped(&outcome, "--no-implied-dirs at protocol 29");
}

/// AVAILABILITY, on the same fixture. Confining the implied-parent creation
/// must not turn a working transfer into a failure: the module still receives
/// the whole `-R` path. Measured identical to rsync 3.5.0, which likewise
/// replaces the conflicting symlink with a real directory
/// (`generator.c:1451-1455`).
#[test]
fn a_confined_relative_push_still_delivers_the_whole_path() {
    let Some(outcome) = push_relative(Plant::DirSymlinkOutsideModule, &[], Sandbox::OptedOut)
    else {
        return;
    };

    assert_eq!(
        outcome.client_exit,
        Some(0),
        "the push must still succeed{}",
        outcome.diagnostics(),
    );
    assert!(
        outcome.inside.contains(&PathBuf::from("a/b/c/file.txt")),
        "the module must receive the full --relative path{}",
        outcome.diagnostics(),
    );
}

/// OVER-REFUSAL CONTROL. A trusted-owned directory symlink whose target stays
/// INSIDE the module is a legitimate operator layout. "Refuse to build an
/// implied parent through any symlink" would satisfy the pin above while
/// breaking it, so the transfer must still land the whole path.
#[test]
fn an_in_module_symlinked_component_still_receives_the_transfer() {
    let Some(outcome) = push_relative(Plant::DirSymlinkInsideModule, &[], Sandbox::Enforced) else {
        return;
    };

    assert_eq!(
        outcome.client_exit,
        Some(0),
        "an in-module symlinked component must not fail the transfer{}",
        outcome.diagnostics(),
    );
    assert!(
        outcome.inside.contains(&PathBuf::from("a/b/c/file.txt")),
        "the module must receive the full --relative path{}",
        outcome.diagnostics(),
    );
}
