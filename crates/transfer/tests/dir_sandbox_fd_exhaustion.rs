//! Observes the descriptor-exhaustion hint firing on the daemon receiver's
//! own entry point into the SEC-1 dirfd carrier.
//!
//! `fast_io::dir_sandbox` prints a one-shot hint when a component open
//! fails with `EMFILE`/`ENFILE`, because the carrier holds a dirfd per live
//! path component and can therefore run out of descriptors where a single
//! path-based `open()` would not. Without the hint the bare "Too many open
//! files" says nothing about which limit to raise.
//!
//! upstream: `rsync-3.5.0/syscall.c:2924-2936` - `ds_descend()` emits it as
//! a bare `rprintf(FWARNING, ...)`, which `rwrite()` routes to stderr
//! verbatim (`log.c:341`). The `rsync warning: ... (code N) at FILE(LINE)`
//! envelope is spelled out at its own call site (`log.c:956`) and is *not*
//! applied here, so this test pins the bare text.
//!
//! The unit coverage in `fast_io` drives the hint through
//! `DirSandbox::enter`, which no production caller uses. The path an
//! operator actually reaches is the daemon receiver's:
//! `receiver::transfer::setup::sandbox::open_sandbox_for_dest_anchored`
//! splits the flattened destination back into the operator-named module
//! root and the peer-supplied tail, then calls
//! `DirSandbox::open_dest_anchor(module_root, peer_tail)` - which walks the
//! tail component by component through the emitting open. That public call
//! is what this test drives, so a refactor that stops routing the anchored
//! walk through the emitting open turns this cell red.
//!
//! # Why the ceiling is computed rather than hard-coded
//!
//! `open_dest_anchor` allocates the anchor dirfd itself, so a limit low
//! enough to refuse every open would fail at the anchor and never reach a
//! tail component. The harness instead probes the lowest descriptor number
//! the kernel would hand out and sets the soft limit exactly one above it:
//! the anchor open succeeds, the first tail component is refused.
//!
//! Both the `RLIMIT_NOFILE` change and the stderr redirect are
//! process-global. That is safe only because nextest runs each test in its
//! own process - the workspace mandates nextest. Every descriptor the test
//! needs is allocated *before* the limit drops, and both globals are
//! restored before any assertion runs so a failing assert can still
//! allocate for its own output.

#![cfg(unix)]

use std::io::{Read, Seek};
use std::os::fd::AsRawFd;
use std::path::Path;

use fast_io::DirSandbox;
use rustix::io::{Errno, dup};
use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
use rustix::stdio::{dup2_stderr, stderr};

/// The hint text, byte for byte as upstream prints it.
///
/// upstream: `rsync-3.5.0/syscall.c:2930-2931`. Held as a literal rather
/// than read from `fast_io` (the constant is private) so this cell fails on
/// a reworded or enveloped emit, not just a deleted one.
const FD_EXHAUSTION_HINT: &str =
    "out of file descriptors resolving a deep path; raise the open-file limit (e.g. `ulimit -n`)";

/// `tempdir()` paths may include a symlink prefix (macOS `/tmp ->
/// /private/tmp`, some CI runners). The anchor open resolves symlinks, but
/// the tail walk does not, so canonicalise to keep the fixture honest.
fn canonical_tempdir() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let canon = std::fs::canonicalize(dir.path()).expect("canonicalize");
    (dir, canon)
}

/// The lowest descriptor number the kernel would hand out right now.
///
/// `open(2)` always returns the lowest free number, so opening and closing
/// one probe file names the number the *next* allocation will take. Setting
/// the soft limit to that number plus one therefore admits exactly one more
/// descriptor.
fn next_free_fd() -> u64 {
    let probe = tempfile::tempfile().expect("probe descriptor");
    let raw = probe.as_raw_fd();
    assert!(raw >= 0, "probe descriptor must be valid");
    raw as u64
}

/// Restore a previously read `RLIMIT_NOFILE`.
fn restore(limit: &Rlimit) {
    setrlimit(
        Resource::Nofile,
        Rlimit {
            current: limit.current,
            maximum: limit.maximum,
        },
    )
    .expect("restore RLIMIT_NOFILE");
}

/// A daemon receiver anchoring its destination under a descriptor ceiling
/// must warn once about the ceiling and still report `EMFILE` to its caller.
///
/// Three things are checked, and each fails for a different reason:
///
/// 1. The two control runs prove the fixture is a *working* anchored walk
///    and that the hint stays silent both when the walk resolves and when
///    it fails for an ordinary reason (`ENOENT`). The second leg is what
///    kills a predicate widened to fire on any error.
/// 2. The observed soft limit is asserted against the requested one.
///    `setrlimit` refuses a request above `rlim_max` and the production
///    raise discards its error, so a harness that assumed the drop landed
///    could report a green cell having exercised nothing.
/// 3. The hint count and the caller-visible errno pin the emit itself.
///    Deleting the `eprintln!` leaves the errno assertion green, so the
///    stderr half is the one carrying the non-vacuity.
#[test]
fn daemon_anchored_walk_warns_once_when_descriptors_run_out() {
    let (_keep, module_root) = canonical_tempdir();
    let peer_tail = Path::new("archive/2026/hosts");
    std::fs::create_dir_all(module_root.join(peer_tail)).expect("build peer tail");
    let tail_components = peer_tail.components().count();

    // Controls, both under the ambient limit and both silent:
    //
    // - a walk that resolves, proving the fixture is a *working* anchored
    //   walk rather than one that fails for an unrelated reason;
    // - a walk that fails with an ordinary `ENOENT`, which is the traffic a
    //   predicate widened to "any error" would start warning on. Without
    //   this leg the emit-site mutation `exhausted = true` survives here.
    //
    // The first leg also warms the `openat2_supported()` probe, which
    // allocates a descriptor of its own and would otherwise be poisoned by
    // the lowered ceiling. Both sandboxes are dropped immediately so their
    // descriptors are free again.
    let mut captured = tempfile::tempfile().expect("stderr capture file");
    let saved_stderr = dup(stderr()).expect("save stderr");
    dup2_stderr(&captured).expect("redirect stderr");
    let resolved = DirSandbox::open_dest_anchor(&module_root, peer_tail);
    let missing = DirSandbox::open_dest_anchor(&module_root, Path::new("archive/absent"));
    dup2_stderr(&saved_stderr).expect("restore stderr");

    let mut control_stderr = String::new();
    captured.rewind().expect("rewind control capture");
    captured
        .read_to_string(&mut control_stderr)
        .expect("read control stderr");
    resolved.expect("the anchored walk must succeed under the ambient limit");
    let missing = missing.expect_err("a tail component that does not exist must fail");
    assert_eq!(
        missing.raw_os_error(),
        Some(Errno::NOENT.raw_os_error()),
        "the negative control must fail with ENOENT, not some other errno"
    );
    assert_eq!(
        control_stderr, "",
        "neither a resolving walk nor an ordinary ENOENT may print the \
         descriptor-exhaustion hint; got: {control_stderr:?}"
    );

    // Every descriptor the measured run needs is allocated here, before the
    // ceiling drops.
    let mut captured = tempfile::tempfile().expect("stderr capture file");
    let saved_stderr = dup(stderr()).expect("save stderr");
    let original = getrlimit(Resource::Nofile);

    // One descriptor of headroom: the anchor open takes it, the first tail
    // component is refused.
    let ceiling = next_free_fd() + 1;

    dup2_stderr(&captured).expect("redirect stderr");
    let lowered = setrlimit(
        Resource::Nofile,
        Rlimit {
            current: Some(ceiling),
            maximum: original.maximum,
        },
    );
    let observed = getrlimit(Resource::Nofile).current;

    // Only run the measurement if the ceiling really moved. Running it
    // regardless would turn a failed drop into a silent pass.
    let outcome = (lowered.is_ok() && observed == Some(ceiling))
        .then(|| DirSandbox::open_dest_anchor(&module_root, peer_tail));

    restore(&original);
    dup2_stderr(&saved_stderr).expect("restore stderr");

    let mut emitted = String::new();
    captured.rewind().expect("rewind capture");
    captured
        .read_to_string(&mut emitted)
        .expect("read captured stderr");

    assert_eq!(
        observed,
        Some(ceiling),
        "the harness could not lower RLIMIT_NOFILE to {ceiling} \
         (setrlimit: {lowered:?}, original: {original:?}); nothing below \
         this point would have been exercised"
    );
    let outcome = outcome.expect("the measurement is gated on the drop above");

    let err = outcome.expect_err(&format!(
        "the anchored walk completed with RLIMIT_NOFILE={ceiling} and \
         {tail_components} peer-tail components; no descriptor pressure was \
         applied, so this cell would have asserted nothing"
    ));
    assert_eq!(
        err.raw_os_error(),
        Some(Errno::MFILE.raw_os_error()),
        "the daemon receiver must still see EMFILE, not an error re-derived \
         after the hint was printed (RLIMIT_NOFILE={ceiling})"
    );
    assert_eq!(
        emitted.matches(FD_EXHAUSTION_HINT).count(),
        1,
        "the anchored walk must print upstream's hint exactly once under \
         RLIMIT_NOFILE={ceiling} with {tail_components} tail components; \
         got: {emitted:?}"
    );
    assert!(
        !emitted.contains("rsync warning:"),
        "upstream's FWARNING at this site carries no envelope (log.c:341); \
         got: {emitted:?}"
    );
}
