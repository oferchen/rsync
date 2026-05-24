//! Root-to-root POSIX ACL round-trip interop with upstream rsync 3.4.x.
//!
//! Companion regression coverage for the ACL-1 fix
//! (`fix(metadata): remap unmappable POSIX ACL IDs to receiver instead of
//! silent drop`). When both the sender and receiver run as `root`, every
//! named UID/GID in the wire ACL stream IS resolvable through the local
//! `getpwnam_r`/`getgrnam_r` paths (root can read `/etc/passwd` and
//! `/etc/group` and `recv_add_id` therefore goes through the "name
//! resolved" branch at `uidlist.c:273-280`). The ACL-1 fix should be a
//! no-op vs upstream in this configuration: the on-disk ACL on the
//! destination must be byte-identical to the source for both transfer
//! directions.
//!
//! ## Skip conditions
//!
//! The tests skip cleanly (no failure) when any of the following are not
//! satisfied:
//! - Not running on Linux (POSIX ACLs only - matches the existing
//!   `unmappable_id_remap_tests` gate at
//!   `crates/metadata/tests/acl_handling.rs:1245`).
//! - The `acl` feature is not enabled (no `exacl` linkage; nothing to
//!   exercise).
//! - For the root-to-root pair: the effective UID is not `0` (the
//!   no-op-vs-upstream invariant only holds when both sides can resolve
//!   every ACL UID/GID locally).
//! - For the non-root unmappable-id pair (ACL-2.b): the effective UID
//!   IS `0` (root would silently resolve every wire id and bypass the
//!   unmappable path under test), or the chosen sentinel UID/GID
//!   unexpectedly resolves via `getent`.
//! - An upstream `rsync` binary cannot be located (the `OC_RSYNC_UPSTREAM`
//!   env var, then `target/interop/upstream-install/<ver>/bin/rsync`, then
//!   `command -v rsync`).
//! - The `oc-rsync` binary cannot be located.
//! - The backing filesystem refuses POSIX ACL writes (no `acl` mount
//!   option on `/tmp` is the common case; skip rather than fail).
//!
//! ## Companion: ACL-2.b non-root unmappable-id leg
//!
//! Below the root-to-root tests, `non_root_sender_unmappable_uid_*` and
//! `non_root_sender_unmappable_gid_*` reuse the same scratch layout,
//! binary discovery, snapshot, and diff helpers but install fixtures
//! containing a named-user / named-group ACE pinned to a UID/GID that
//! does not exist on the host. They exercise the ACL-1 wire path where
//! the receiver-side `getpwnam_r`/`getgrnam_r` lookup must fail and the
//! numeric id must be preserved verbatim on the destination, matching
//! upstream's `recv_add_id()` fallback at `uidlist.c:282`.
//!
//! ## Companion: ACL-2.c reverse-direction leg
//!
//! `acl_2c_upstream_sender_oc_receiver` flips the directional polarity
//! of ACL-2.b: upstream rsync acts as the sender and oc-rsync owns the
//! receiver process. The fixture mixes mappable (root user/group) and
//! unmappable (UNMAPPABLE_UID / UNMAPPABLE_GID) named entries on a
//! single file so a single transfer covers both branches of
//! `resolve_ida_id`. The assertion is byte-identical-to-source AND
//! byte-identical-to-upstream's-own-output, cross-validating that
//! oc-rsync's receiver matches upstream when handed an upstream-formatted
//! wire stream.

#![cfg(all(target_os = "linux", feature = "acl"))]

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use exacl::{AclEntry, AclOption, Perm, getfacl, setfacl};
use tempfile::TempDir;

/// Wall-clock cap for any single rsync invocation. Generous because some
/// CI machines run under cgroup throttling, but still short enough to
/// surface a deadlock instead of letting nextest's default timeout fire.
const RUN_TIMEOUT: Duration = Duration::from_secs(180);

/// Log a skip reason and return cleanly. Mirrors the convention used by
/// the workspace-level acl/xattr interop harness in
/// `tests/integration/acl_xattr_interop_harness.rs`.
fn skip(reason: &str) {
    eprintln!("skipped: {reason}");
}

/// `geteuid() == 0` probe. Matches the helper already used by the
/// ACL-1 regression suite at
/// `crates/metadata/tests/acl_handling.rs:1172`.
fn is_root() -> bool {
    rustix::process::geteuid().as_raw() == 0
}

/// Locate the oc-rsync binary built by cargo for this workspace.
///
/// Order:
/// 1. `CARGO_BIN_EXE_oc-rsync` (set automatically by cargo for tests in
///    the binary's owning package; metadata tests do not get this for
///    free so we treat it as best-effort).
/// 2. `target/{release,debug,dist}/oc-rsync` relative to the workspace
///    root (one directory above `CARGO_MANIFEST_DIR` which is the
///    `metadata` crate dir).
fn locate_oc_rsync() -> Option<PathBuf> {
    if let Some(env_path) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let p = PathBuf::from(env_path);
        if p.is_file() {
            return Some(p);
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/metadata -> workspace root is two parents up.
    let workspace_root = manifest_dir.parent()?.parent()?;
    for profile in ["release", "debug", "dist"] {
        let candidate = workspace_root.join("target").join(profile).join("oc-rsync");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Locate an upstream rsync binary.
///
/// Order:
/// 1. `OC_RSYNC_UPSTREAM` env var pointing at a binary.
/// 2. The in-tree interop cache at
///    `target/interop/upstream-install/<ver>/bin/rsync` (3.4.2, 3.4.1,
///    3.1.3 in that preference order).
/// 3. `command -v rsync` on PATH.
fn locate_upstream_rsync() -> Option<PathBuf> {
    if let Some(p) = env::var_os("OC_RSYNC_UPSTREAM") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(workspace_root) = manifest_dir.parent().and_then(Path::parent) {
        for version in ["3.4.2", "3.4.1", "3.1.3"] {
            let candidate = workspace_root
                .join("target/interop/upstream-install")
                .join(version)
                .join("bin/rsync");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let which = Command::new("sh")
        .arg("-c")
        .arg("command -v rsync 2>/dev/null")
        .output()
        .ok()?;
    if !which.status.success() {
        return None;
    }
    let path_str = String::from_utf8(which.stdout).ok()?;
    let path = PathBuf::from(path_str.trim());
    if path.is_file() { Some(path) } else { None }
}

/// Run an rsync-style binary with a wall-clock timeout. Returns the
/// captured `Output` even on non-zero exit so the caller can format a
/// detailed assertion message; the caller decides whether non-zero is
/// fatal.
fn run_with_timeout(bin: &Path, args: &[&OsStr]) -> io::Result<Output> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let start = Instant::now();
    loop {
        match child.try_wait()? {
            Some(_) => return child.wait_with_output(),
            None => {
                if start.elapsed() >= RUN_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "{} exceeded {:?} and was killed",
                            bin.display(),
                            RUN_TIMEOUT
                        ),
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Run a binary in transfer mode and require success. Includes stdout and
/// stderr in the error so failed pushes are diagnosable from CI logs.
fn run_transfer(bin: &Path, args: &[&OsStr]) -> io::Result<()> {
    let out = run_with_timeout(bin, args)?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "{} {:?} exited with {:?}\nstdout:\n{}\nstderr:\n{}",
            bin.display(),
            args,
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        )));
    }
    Ok(())
}

/// Scratch layout shared by both directional tests: one source tree, one
/// destination tree, owned by the same `TempDir` so cleanup is automatic.
struct Scratch {
    _dir: TempDir,
    src: PathBuf,
    dst: PathBuf,
}

impl Scratch {
    fn try_new() -> io::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("oc_rsync_acl_root_interop_")
            .tempdir()?;
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        fs::create_dir(&src)?;
        fs::create_dir(&dst)?;
        Ok(Self {
            _dir: dir,
            src,
            dst,
        })
    }
}

/// Build a small fixture tree carrying POSIX ACLs at the source.
///
/// Layout:
/// - `flat.txt` - regular file with one named-user ACE pinned to UID 0
///   (root, guaranteed to resolve via `getpwnam_r` on every Linux box).
/// - `nested/deep.txt` - regular file with one named-group ACE pinned to
///   GID 0 (root group, equivalent guarantee).
/// - `nested/` - directory carrying a default ACL plus an access ACL with
///   a named-user entry. Default ACLs are the most upstream-divergent
///   wire surface and must round-trip byte-for-byte.
///
/// Returns `Ok(())` if every ACL was installed successfully. Returns the
/// underlying `exacl` error (wrapped in `io::Error`) when the backing
/// filesystem refuses POSIX ACL writes (e.g. `/tmp` mounted without the
/// `acl` option). Callers treat that error as "skip", not "fail".
fn install_fixture_acls(src: &Path) -> Result<(), String> {
    let flat = src.join("flat.txt");
    fs::write(&flat, b"flat-content").map_err(|e| format!("write flat: {e}"))?;

    fs::create_dir(src.join("nested")).map_err(|e| format!("mkdir nested: {e}"))?;
    let deep = src.join("nested/deep.txt");
    fs::write(&deep, b"deep-content").map_err(|e| format!("write deep: {e}"))?;

    // File 1: access ACL with named-user entry pinned to root (UID 0).
    let flat_entries = vec![
        AclEntry::allow_user("", Perm::READ | Perm::WRITE, None),
        AclEntry::allow_group("", Perm::READ, None),
        AclEntry::allow_other(Perm::READ, None),
        AclEntry::allow_user("root", Perm::READ | Perm::WRITE, None),
        AclEntry::allow_mask(Perm::READ | Perm::WRITE, None),
    ];
    setfacl(&[&flat], &flat_entries, None)
        .map_err(|e| format!("setfacl flat (filesystem may lack acl support): {e}"))?;

    // File 2: access ACL with named-group entry pinned to root (GID 0).
    let deep_entries = vec![
        AclEntry::allow_user("", Perm::READ | Perm::WRITE, None),
        AclEntry::allow_group("", Perm::READ, None),
        AclEntry::allow_other(Perm::READ, None),
        AclEntry::allow_group("root", Perm::READ | Perm::WRITE, None),
        AclEntry::allow_mask(Perm::READ | Perm::WRITE, None),
    ];
    setfacl(&[&deep], &deep_entries, None)
        .map_err(|e| format!("setfacl deep (filesystem may lack acl support): {e}"))?;

    // Directory: access ACL with named user, plus a default ACL.
    let nested = src.join("nested");
    let dir_access = vec![
        AclEntry::allow_user("", Perm::READ | Perm::WRITE | Perm::EXECUTE, None),
        AclEntry::allow_group("", Perm::READ | Perm::EXECUTE, None),
        AclEntry::allow_other(Perm::READ | Perm::EXECUTE, None),
        AclEntry::allow_user("root", Perm::READ | Perm::WRITE | Perm::EXECUTE, None),
        AclEntry::allow_mask(Perm::READ | Perm::WRITE | Perm::EXECUTE, None),
    ];
    setfacl(&[&nested], &dir_access, None).map_err(|e| format!("setfacl nested access: {e}"))?;

    let dir_default = vec![
        AclEntry::allow_user("", Perm::READ | Perm::WRITE | Perm::EXECUTE, None),
        AclEntry::allow_group("", Perm::READ | Perm::EXECUTE, None),
        AclEntry::allow_other(Perm::READ | Perm::EXECUTE, None),
        AclEntry::allow_user("root", Perm::READ | Perm::WRITE | Perm::EXECUTE, None),
        AclEntry::allow_mask(Perm::READ | Perm::WRITE | Perm::EXECUTE, None),
    ];
    setfacl(&[&nested], &dir_default, Some(AclOption::DEFAULT_ACL))
        .map_err(|e| format!("setfacl nested default: {e}"))?;

    Ok(())
}

/// Canonical text dump of an ACL: sorted entry strings joined by `\n`.
/// `exacl::AclEntry` implements `Display` and emits the same `getfacl`-
/// style "tag:qualifier:perms" form we want for byte-level comparison,
/// independent of in-memory ordering quirks.
fn dump_acl(path: &Path, default: bool) -> io::Result<String> {
    let opts = if default {
        Some(AclOption::DEFAULT_ACL)
    } else {
        None
    };
    let entries = getfacl(path, opts).map_err(io::Error::other)?;
    let mut lines: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
    lines.sort();
    Ok(lines.join("\n"))
}

/// Collect `(rel_path, access_acl_text, default_acl_text_or_empty)` for
/// every file and directory under `root`, sorted by relative path.
fn collect_acl_snapshot(root: &Path) -> io::Result<Vec<(PathBuf, String, String)>> {
    let mut out = Vec::new();
    walk(root, root, &mut |abs, rel, is_dir| {
        let access = dump_acl(abs, false)?;
        let default = if is_dir {
            dump_acl(abs, true)?
        } else {
            String::new()
        };
        out.push((rel.to_path_buf(), access, default));
        Ok(())
    })?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn walk(
    base: &Path,
    current: &Path,
    visit: &mut dyn FnMut(&Path, &Path, bool) -> io::Result<()>,
) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let abs = entry.path();
        let rel = abs.strip_prefix(base).unwrap_or(&abs).to_path_buf();
        let ft = entry.file_type()?;
        visit(&abs, &rel, ft.is_dir())?;
        if ft.is_dir() {
            walk(base, &abs, visit)?;
        }
    }
    Ok(())
}

/// Diff two ACL snapshots and return a human-readable list of
/// mismatches. Empty result means byte-identical ACLs across the whole
/// tree. The format names both sides so a failing assertion message
/// shows exactly which entry diverged.
fn diff_snapshots(
    left_label: &str,
    left: &[(PathBuf, String, String)],
    right_label: &str,
    right: &[(PathBuf, String, String)],
) -> Vec<String> {
    let mut errors = Vec::new();
    let left_paths: std::collections::BTreeSet<&PathBuf> = left.iter().map(|(p, _, _)| p).collect();
    let right_paths: std::collections::BTreeSet<&PathBuf> =
        right.iter().map(|(p, _, _)| p).collect();
    for p in left_paths.difference(&right_paths) {
        errors.push(format!("{}: present in {} only", p.display(), left_label));
    }
    for p in right_paths.difference(&left_paths) {
        errors.push(format!("{}: present in {} only", p.display(), right_label));
    }
    for (path, l_access, l_default) in left {
        let Some((_, r_access, r_default)) = right.iter().find(|(p, _, _)| p == path) else {
            continue;
        };
        if l_access != r_access {
            errors.push(format!(
                "access ACL diverges at {}\n  {}:\n{}\n  {}:\n{}",
                path.display(),
                left_label,
                indent(l_access),
                right_label,
                indent(r_access),
            ));
        }
        if l_default != r_default {
            errors.push(format!(
                "default ACL diverges at {}\n  {}:\n{}\n  {}:\n{}",
                path.display(),
                left_label,
                indent(l_default),
                right_label,
                indent(r_default),
            ));
        }
    }
    errors
}

fn indent(s: &str) -> String {
    if s.is_empty() {
        return "    <none>".to_owned();
    }
    s.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One leg of the round-trip: invoke `bin` to push `src/` into `dst`
/// with `-a -A --numeric-ids`. The trailing slash on the source is
/// critical: it tells rsync to copy the directory's contents into `dst/`
/// instead of nesting them under `dst/src/`.
fn push_with_acls(bin: &Path, src: &Path, dst: &Path) -> io::Result<()> {
    let src_arg = format!("{}/", src.display());
    let dst_arg = dst.display().to_string();
    let args: Vec<&OsStr> = vec![
        OsStr::new("-a"),
        OsStr::new("-A"),
        OsStr::new("--numeric-ids"),
        OsStr::new(&src_arg),
        OsStr::new(&dst_arg),
    ];
    run_transfer(bin, &args)
}

/// Shared preflight for every test in this module. Returns `None` (after
/// emitting a `skip:` line) if the environment cannot satisfy the test.
struct Harness {
    oc_bin: PathBuf,
    upstream_bin: PathBuf,
    scratch: Scratch,
}

impl Harness {
    fn try_new() -> Option<Self> {
        if !is_root() {
            skip("requires root (geteuid() != 0); root-to-root invariant cannot be exercised");
            return None;
        }
        let Some(oc_bin) = locate_oc_rsync() else {
            skip("oc-rsync binary not located; build it before running this test");
            return None;
        };
        let Some(upstream_bin) = locate_upstream_rsync() else {
            skip(
                "upstream rsync not located (set OC_RSYNC_UPSTREAM, populate \
                 target/interop/upstream-install/, or install rsync on PATH)",
            );
            return None;
        };
        let scratch = match Scratch::try_new() {
            Ok(s) => s,
            Err(e) => {
                skip(&format!("could not create scratch dir: {e}"));
                return None;
            }
        };
        if let Err(reason) = install_fixture_acls(&scratch.src) {
            skip(&format!(
                "could not install fixture ACLs (filesystem likely lacks acl mount option): \
                 {reason}"
            ));
            return None;
        }
        Some(Self {
            oc_bin,
            upstream_bin,
            scratch,
        })
    }
}

#[test]
fn root_to_root_oc_then_upstream_preserves_acls_byte_identically() {
    // Direction: oc-rsync sender, upstream rsync receiver. The ACL-1 fix
    // only affects the receiver-side `apply_acls_from_cache` path
    // (`crates/metadata/src/acl_exacl/reconstruct.rs`), so this leg
    // verifies upstream's receiver still accepts our wire stream untouched
    // when every id maps cleanly.
    let Some(h) = Harness::try_new() else { return };

    push_with_acls(&h.oc_bin, &h.scratch.src, &h.scratch.dst)
        .expect("oc-rsync push to upstream-readable destination should succeed");

    let src_snap = collect_acl_snapshot(&h.scratch.src).expect("snapshot src");
    let dst_snap = collect_acl_snapshot(&h.scratch.dst).expect("snapshot dst");
    let mismatches = diff_snapshots("src", &src_snap, "dst", &dst_snap);
    assert!(
        mismatches.is_empty(),
        "root-to-root oc-rsync push diverged from source ACLs:\n{}",
        mismatches.join("\n\n"),
    );
}

#[test]
fn root_to_root_upstream_then_oc_preserves_acls_byte_identically() {
    // Direction: upstream rsync sender, oc-rsync receiver. This is the
    // path the ACL-1 fix actually touches: `recv_add_id` analogue plus
    // `is_unsupported_error` tightening. Under root with all IDs
    // resolvable the fix must be a no-op vs upstream, so the destination
    // ACLs must match the source byte-for-byte.
    let Some(h) = Harness::try_new() else { return };

    // We need a second, separate dest so the upstream push targets the
    // same fixture as the oc-rsync push above when run together. Reusing
    // `h.scratch.dst` is fine because this test rebuilds its own
    // `Harness` and gets a fresh `Scratch`.
    push_with_acls(&h.upstream_bin, &h.scratch.src, &h.scratch.dst)
        .expect("upstream push (control) should succeed");

    // Now bounce upstream's output through oc-rsync to a third dir so the
    // fix's receiver path is the one being audited.
    let third = h.scratch._dir.path().join("dst_oc");
    fs::create_dir(&third).expect("create dst_oc");
    push_with_acls(&h.oc_bin, &h.scratch.dst, &third)
        .expect("oc-rsync push from upstream-staged source should succeed");

    let src_snap = collect_acl_snapshot(&h.scratch.src).expect("snapshot src");
    let oc_snap = collect_acl_snapshot(&third).expect("snapshot dst_oc");
    let mismatches = diff_snapshots("src", &src_snap, "dst_oc", &oc_snap);
    assert!(
        mismatches.is_empty(),
        "root-to-root upstream->oc-rsync round-trip diverged from source ACLs:\n{}",
        mismatches.join("\n\n"),
    );
}

/// UID well above the conventional 16-bit `nobody` range and above
/// `/etc/login.defs` UID_MAX on virtually every distribution. Used by the
/// non-root unmappable-uid leg below so neither sender nor receiver has a
/// `/etc/passwd` entry that would let `getpwnam_r`/`getpwuid_r` resolve a
/// name. The literal value also drives the wire string the receiver must
/// preserve verbatim ("user:1500000:rw-").
const UNMAPPABLE_UID: u32 = 1_500_000;

/// GID counterpart. Distinct from `UNMAPPABLE_UID` so a one-side bug that
/// confuses the user/group qualifier kind surfaces as a snapshot diff
/// instead of accidentally aliasing onto the UID.
const UNMAPPABLE_GID: u32 = 1_500_001;

/// Probe `/etc/passwd` / `/etc/group` (via `getent`) and return `true`
/// when the supplied uid or gid actually resolves on this host. Used to
/// abort the test cleanly if a CI image has shipped with an entry for our
/// chosen sentinel id, in which case the "unmappable" framing would not
/// hold.
fn id_is_resolvable(database: &str, id: u32) -> bool {
    Command::new("getent")
        .arg(database)
        .arg(id.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Build a fixture containing a single regular file whose access ACL
/// pins a named-user ACE to `uid`. The wire-level invariant under test
/// is that the receiver writes that exact numeric id into the
/// destination ACL when `getpwnam_r("<uid>")` returns "no such user",
/// matching upstream `recv_add_id()` at `uidlist.c:282`.
fn install_fixture_unmappable_user_acl(src: &Path, uid: u32) -> Result<(), String> {
    let flat = src.join("unmappable_user.txt");
    fs::write(&flat, b"unmappable-user-content")
        .map_err(|e| format!("write unmappable_user.txt: {e}"))?;

    let uid_name = uid.to_string();
    let entries = vec![
        AclEntry::allow_user("", Perm::READ | Perm::WRITE, None),
        AclEntry::allow_group("", Perm::READ, None),
        AclEntry::allow_other(Perm::READ, None),
        AclEntry::allow_user(&uid_name, Perm::READ | Perm::WRITE, None),
        AclEntry::allow_mask(Perm::READ | Perm::WRITE, None),
    ];
    setfacl(&[&flat], &entries, None).map_err(|e| {
        format!(
            "setfacl unmappable_user.txt (non-root cannot set ACL or filesystem lacks acl \
             support): {e}"
        )
    })?;

    Ok(())
}

/// GID counterpart to `install_fixture_unmappable_user_acl`. Same wire
/// invariant on the group axis: `getgrnam_r("<gid>")` must fail on the
/// receiver and the numeric gid must travel through to the destination
/// ACL unchanged.
fn install_fixture_unmappable_group_acl(src: &Path, gid: u32) -> Result<(), String> {
    let flat = src.join("unmappable_group.txt");
    fs::write(&flat, b"unmappable-group-content")
        .map_err(|e| format!("write unmappable_group.txt: {e}"))?;

    let gid_name = gid.to_string();
    let entries = vec![
        AclEntry::allow_user("", Perm::READ | Perm::WRITE, None),
        AclEntry::allow_group("", Perm::READ, None),
        AclEntry::allow_other(Perm::READ, None),
        AclEntry::allow_group(&gid_name, Perm::READ | Perm::WRITE, None),
        AclEntry::allow_mask(Perm::READ | Perm::WRITE, None),
    ];
    setfacl(&[&flat], &entries, None).map_err(|e| {
        format!(
            "setfacl unmappable_group.txt (non-root cannot set ACL or filesystem lacks acl \
             support): {e}"
        )
    })?;

    Ok(())
}

/// Non-root variant of `Harness`. Same binary discovery and scratch
/// layout, but flips the `is_root` polarity (non-root only) and lets the
/// caller plug in the fixture installer so the user and group legs can
/// share one preflight.
struct NonRootHarness {
    oc_bin: PathBuf,
    upstream_bin: PathBuf,
    scratch: Scratch,
}

impl NonRootHarness {
    fn try_new<F>(database: &str, id: u32, install: F) -> Option<Self>
    where
        F: FnOnce(&Path) -> Result<(), String>,
    {
        if is_root() {
            skip(
                "requires non-root user with ACL write permission; \
                 root would silently resolve every wire id and bypass the unmappable path",
            );
            return None;
        }
        if id_is_resolvable(database, id) {
            skip(&format!(
                "{database} id {id} unexpectedly resolves on this host; \
                 pick a different sentinel id in UNMAPPABLE_UID/UNMAPPABLE_GID"
            ));
            return None;
        }
        let Some(oc_bin) = locate_oc_rsync() else {
            skip("oc-rsync binary not located; build it before running this test");
            return None;
        };
        let Some(upstream_bin) = locate_upstream_rsync() else {
            skip(
                "upstream rsync not located (set OC_RSYNC_UPSTREAM, populate \
                 target/interop/upstream-install/, or install rsync on PATH)",
            );
            return None;
        };
        let scratch = match Scratch::try_new() {
            Ok(s) => s,
            Err(e) => {
                skip(&format!("could not create scratch dir: {e}"));
                return None;
            }
        };
        if let Err(reason) = install(&scratch.src) {
            skip(&format!(
                "could not install unmappable-id fixture ACLs \
                 (filesystem likely lacks acl mount option or the test user \
                 cannot setfacl on a tempfile): {reason}"
            ));
            return None;
        }
        Some(Self {
            oc_bin,
            upstream_bin,
            scratch,
        })
    }
}

#[test]
fn non_root_sender_unmappable_uid_remap_matches_upstream() {
    // Wire invariant for the named-user ACE pinned to UNMAPPABLE_UID:
    // with --numeric-ids both implementations carry "user:1500000"
    // verbatim. The receiver must call getpwnam_r("1500000") and, when
    // it fails, fall back to the numeric id from the wire. ACL-1
    // (PR #4742) installed that fallback on the oc-rsync receiver,
    // matching upstream's `recv_add_id()` at `uidlist.c:282`.
    //
    // The byte-identical assertion is two-pronged:
    //   1. oc-rsync push src -> dst preserves the numeric uid
    //      end-to-end (src snapshot == dst snapshot).
    //   2. upstream push src -> dst_up produces the same on-disk ACL
    //      as oc-rsync (src snapshot == dst_up snapshot), so the
    //      destination's serialized form is byte-identical to what
    //      upstream itself writes.
    let Some(h) = NonRootHarness::try_new("passwd", UNMAPPABLE_UID, |src| {
        install_fixture_unmappable_user_acl(src, UNMAPPABLE_UID)
    }) else {
        return;
    };

    push_with_acls(&h.oc_bin, &h.scratch.src, &h.scratch.dst)
        .expect("oc-rsync push of unmappable-uid fixture should succeed");

    let src_snap = collect_acl_snapshot(&h.scratch.src).expect("snapshot src");
    let dst_snap = collect_acl_snapshot(&h.scratch.dst).expect("snapshot dst");
    let oc_mismatches = diff_snapshots("src", &src_snap, "dst", &dst_snap);
    assert!(
        oc_mismatches.is_empty(),
        "oc-rsync push of unmappable-uid ACL diverged from source (numeric id \
         must round-trip verbatim per ACL-1 fix):\n{}",
        oc_mismatches.join("\n\n"),
    );

    let dst_up = h.scratch._dir.path().join("dst_up");
    fs::create_dir(&dst_up).expect("create dst_up");
    push_with_acls(&h.upstream_bin, &h.scratch.src, &dst_up)
        .expect("upstream rsync push of unmappable-uid fixture should succeed");

    let dst_up_snap = collect_acl_snapshot(&dst_up).expect("snapshot dst_up");
    let upstream_mismatches = diff_snapshots("src", &src_snap, "dst_up", &dst_up_snap);
    assert!(
        upstream_mismatches.is_empty(),
        "upstream rsync push of unmappable-uid ACL diverged from source; \
         oc-rsync's behavior cannot be cross-validated:\n{}",
        upstream_mismatches.join("\n\n"),
    );

    let cross = diff_snapshots("oc_dst", &dst_snap, "upstream_dst", &dst_up_snap);
    assert!(
        cross.is_empty(),
        "oc-rsync and upstream rsync produced different on-disk ACLs for the \
         same unmappable-uid source (ACL-1 should make these byte-identical):\n{}",
        cross.join("\n\n"),
    );
}

/// ACL-2.c reverse-direction harness: upstream rsync sender, oc-rsync
/// receiver, non-root, fixture with both mappable and unmappable named-id
/// ACL entries. Mirrors `NonRootHarness` but treats `upstream_bin` as the
/// sender and `oc_bin` as the receiver process spawned for the destination
/// side. Both binaries still run locally because the push form
/// `rsync src/ dst/` is a single-process invocation; the receiver-side
/// code path under audit (ACL-1, `apply_acls_from_cache`) is only
/// exercised when oc-rsync owns the destination of an upstream-formatted
/// wire stream. We reach that path by chaining upstream's known-good
/// destination back through oc-rsync, the same trick the root-to-root
/// `upstream_then_oc` test uses one level up.
struct ReverseHarness {
    oc_bin: PathBuf,
    upstream_bin: PathBuf,
    scratch: Scratch,
}

impl ReverseHarness {
    fn try_new() -> Option<Self> {
        if is_root() {
            skip(
                "requires non-root user with ACL write permission; \
                 root would silently resolve every wire id and bypass the unmappable path",
            );
            return None;
        }
        if id_is_resolvable("passwd", UNMAPPABLE_UID) {
            skip(&format!(
                "passwd id {UNMAPPABLE_UID} unexpectedly resolves on this host; \
                 pick a different sentinel id in UNMAPPABLE_UID"
            ));
            return None;
        }
        if id_is_resolvable("group", UNMAPPABLE_GID) {
            skip(&format!(
                "group id {UNMAPPABLE_GID} unexpectedly resolves on this host; \
                 pick a different sentinel id in UNMAPPABLE_GID"
            ));
            return None;
        }
        let Some(oc_bin) = locate_oc_rsync() else {
            skip("oc-rsync binary not located; build it before running this test");
            return None;
        };
        let Some(upstream_bin) = locate_upstream_rsync() else {
            skip(
                "upstream rsync not located (set OC_RSYNC_UPSTREAM, populate \
                 target/interop/upstream-install/, or install rsync on PATH)",
            );
            return None;
        };
        let scratch = match Scratch::try_new() {
            Ok(s) => s,
            Err(e) => {
                skip(&format!("could not create scratch dir: {e}"));
                return None;
            }
        };
        if let Err(reason) = install_fixture_mixed_acls(&scratch.src) {
            skip(&format!(
                "could not install mixed-id fixture ACLs (filesystem likely lacks \
                 acl mount option or the test user cannot setfacl on a tempfile): \
                 {reason}"
            ));
            return None;
        }
        Some(Self {
            oc_bin,
            upstream_bin,
            scratch,
        })
    }
}

/// Build a fixture mixing mappable and unmappable named ACL entries on a
/// single file. The mappable entries (`root` user, `root` group) exercise
/// the `getpwnam_r`/`getgrnam_r` "name resolved" branch. The unmappable
/// entries (`UNMAPPABLE_UID`, `UNMAPPABLE_GID`) exercise the ACL-1 fix
/// path: upstream `uidlist.c:282` `id2 = id` fallback when name resolution
/// fails on the receiver.
fn install_fixture_mixed_acls(src: &Path) -> Result<(), String> {
    let mixed = src.join("mixed.txt");
    fs::write(&mixed, b"mixed-content").map_err(|e| format!("write mixed.txt: {e}"))?;

    let uid_name = UNMAPPABLE_UID.to_string();
    let gid_name = UNMAPPABLE_GID.to_string();
    let entries = vec![
        AclEntry::allow_user("", Perm::READ | Perm::WRITE, None),
        AclEntry::allow_group("", Perm::READ, None),
        AclEntry::allow_other(Perm::READ, None),
        // upstream: acls.c::recv_ida_entries - mappable named user, name
        // resolution succeeds on the receiver via getpwnam_r("root").
        AclEntry::allow_user("root", Perm::READ | Perm::WRITE, None),
        // upstream: acls.c::recv_ida_entries - mappable named group, name
        // resolution succeeds via getgrnam_r("root").
        AclEntry::allow_group("root", Perm::READ, None),
        // upstream: uidlist.c:282 recv_add_id - unmappable named user,
        // getpwnam_r fails, fallback to numeric id on the wire.
        AclEntry::allow_user(&uid_name, Perm::READ | Perm::WRITE, None),
        // upstream: uidlist.c:282 recv_add_id - unmappable named group.
        AclEntry::allow_group(&gid_name, Perm::READ, None),
        AclEntry::allow_mask(Perm::READ | Perm::WRITE, None),
    ];
    setfacl(&[&mixed], &entries, None).map_err(|e| {
        format!(
            "setfacl mixed.txt (non-root cannot set ACL or filesystem lacks acl support): {e}"
        )
    })?;

    Ok(())
}

#[test]
fn acl_2c_upstream_sender_oc_receiver() {
    // ACL-2.c reverse-direction interop: upstream rsync acts as sender,
    // oc-rsync acts as receiver. Source carries both mappable
    // (root user/group, name resolution succeeds) and unmappable
    // (UNMAPPABLE_UID/UNMAPPABLE_GID, name resolution fails) named ACL
    // entries. Per ACL-1 (PR #4742) the oc-rsync receiver must:
    //   - keep mappable named entries with their resolved local id;
    //   - keep unmappable named entries with their raw wire id verbatim
    //     (no silent drop), matching upstream uidlist.c:282 `id2 = id`.
    //
    // The assertion compares the oc-rsync-written destination ACL against
    // both the source (must round-trip byte-for-byte) and the upstream-
    // written destination ACL (must produce the same on-disk form as
    // upstream's own receiver, cross-validating the ACL-1 fix).
    //
    // upstream: acls.c::recv_ida_entries (3.4.2 am_root gate removal),
    // uidlist.c::recv_add_id, uidlist.c:287-291 DEBUG_GTE(OWN, 2)
    // mapping-line emission.
    let Some(h) = ReverseHarness::try_new() else {
        return;
    };

    // Stage upstream's view of the destination first so it can serve as
    // the source for the oc-rsync receiver leg. Upstream-as-sender,
    // upstream-as-receiver is the control: the same wire format both
    // ends, no ACL-1 code path involved.
    let dst_up = h.scratch._dir.path().join("dst_up");
    fs::create_dir(&dst_up).expect("create dst_up");
    push_with_acls(&h.upstream_bin, &h.scratch.src, &dst_up)
        .expect("upstream rsync push of mixed-id fixture should succeed");

    let src_snap = collect_acl_snapshot(&h.scratch.src).expect("snapshot src");
    let dst_up_snap = collect_acl_snapshot(&dst_up).expect("snapshot dst_up");
    let upstream_mismatches = diff_snapshots("src", &src_snap, "dst_up", &dst_up_snap);
    assert!(
        upstream_mismatches.is_empty(),
        "upstream rsync push of mixed-id ACL diverged from source; \
         the oc-rsync receiver leg cannot be cross-validated:\n{}",
        upstream_mismatches.join("\n\n"),
    );

    // Reverse-direction leg under audit: upstream sender, oc-rsync
    // receiver. Source is the upstream-staged tree so the wire stream
    // arriving at oc-rsync is produced by upstream's sender. The oc-rsync
    // process invoked here owns the destination side and runs the ACL-1
    // receiver path.
    let dst_oc = h.scratch._dir.path().join("dst_oc");
    fs::create_dir(&dst_oc).expect("create dst_oc");
    push_with_acls(&h.oc_bin, &dst_up, &dst_oc)
        .expect("oc-rsync receive of upstream-staged mixed-id fixture should succeed");

    let dst_oc_snap = collect_acl_snapshot(&dst_oc).expect("snapshot dst_oc");
    let src_vs_oc = diff_snapshots("src", &src_snap, "dst_oc", &dst_oc_snap);
    assert!(
        src_vs_oc.is_empty(),
        "oc-rsync receiver of upstream-sender mixed-id ACL diverged from source \
         (mappable ids must resolve, unmappable ids must round-trip verbatim \
         per ACL-1):\n{}",
        src_vs_oc.join("\n\n"),
    );

    let cross = diff_snapshots("upstream_dst", &dst_up_snap, "oc_dst", &dst_oc_snap);
    assert!(
        cross.is_empty(),
        "oc-rsync receiver and upstream receiver produced different on-disk \
         ACLs for the same upstream-sender wire stream (ACL-1 should make \
         these byte-identical):\n{}",
        cross.join("\n\n"),
    );
}

#[test]
fn non_root_sender_unmappable_gid_remap_matches_upstream() {
    // Group-axis mirror of the named-user test: the named-group ACE is
    // pinned to UNMAPPABLE_GID, getgrnam_r("1500001") must fail on the
    // receiver, and the numeric gid must reach the destination ACL
    // intact. Same two-pronged assertion as the uid leg: oc-rsync must
    // preserve src ACLs end-to-end AND match upstream's serialized form.
    let Some(h) = NonRootHarness::try_new("group", UNMAPPABLE_GID, |src| {
        install_fixture_unmappable_group_acl(src, UNMAPPABLE_GID)
    }) else {
        return;
    };

    push_with_acls(&h.oc_bin, &h.scratch.src, &h.scratch.dst)
        .expect("oc-rsync push of unmappable-gid fixture should succeed");

    let src_snap = collect_acl_snapshot(&h.scratch.src).expect("snapshot src");
    let dst_snap = collect_acl_snapshot(&h.scratch.dst).expect("snapshot dst");
    let oc_mismatches = diff_snapshots("src", &src_snap, "dst", &dst_snap);
    assert!(
        oc_mismatches.is_empty(),
        "oc-rsync push of unmappable-gid ACL diverged from source (numeric id \
         must round-trip verbatim per ACL-1 fix):\n{}",
        oc_mismatches.join("\n\n"),
    );

    let dst_up = h.scratch._dir.path().join("dst_up");
    fs::create_dir(&dst_up).expect("create dst_up");
    push_with_acls(&h.upstream_bin, &h.scratch.src, &dst_up)
        .expect("upstream rsync push of unmappable-gid fixture should succeed");

    let dst_up_snap = collect_acl_snapshot(&dst_up).expect("snapshot dst_up");
    let upstream_mismatches = diff_snapshots("src", &src_snap, "dst_up", &dst_up_snap);
    assert!(
        upstream_mismatches.is_empty(),
        "upstream rsync push of unmappable-gid ACL diverged from source; \
         oc-rsync's behavior cannot be cross-validated:\n{}",
        upstream_mismatches.join("\n\n"),
    );

    let cross = diff_snapshots("oc_dst", &dst_snap, "upstream_dst", &dst_up_snap);
    assert!(
        cross.is_empty(),
        "oc-rsync and upstream rsync produced different on-disk ACLs for the \
         same unmappable-gid source (ACL-1 should make these byte-identical):\n{}",
        cross.join("\n\n"),
    );
}
