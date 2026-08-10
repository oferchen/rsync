//! Shared harness for ACL/xattr round-trip interop tests vs upstream rsync 3.4.1.
//!
//! These tests stage a directory tree carrying rich metadata (POSIX ACLs,
//! NFSv4 ACLs where supported, user/system xattrs), bounce it through a
//! sequence of `oc-rsync` and upstream `rsync` invocations using `-aAX`, and
//! diff the surviving metadata against the original. The harness covers both
//! directions:
//!
//! - `oc-rsync` then upstream `rsync` (oc-rsync sender, upstream receiver).
//! - upstream `rsync` then `oc-rsync` (upstream sender, oc-rsync receiver).
//!
//! ## Skip conditions
//!
//! The harness skips cleanly (no test failure) when:
//! - The `OC_RSYNC_METADATA_INTEROP` env var is not set to `1`.
//! - An upstream `rsync` binary is not available.
//! - The oc-rsync binary cannot be located.
//! - Per-OS metadata tools are missing (`setfacl`/`getfacl`/`setfattr`/
//!   `getfattr` on Linux, `xattr` on macOS).
//! - The backing filesystem rejects the metadata operations (no ACL/xattr
//!   support on `/tmp`).
//!
//! ## Comparison model
//!
//! Equivalence is recursive over the tree, anchored at the destination root.
//! For each file the harness collects a canonicalised metadata record
//! (sorted xattr key/value list, ACL text dump) and asserts byte-identical
//! records between source and final destination. Order of xattr enumeration
//! is not guaranteed by either implementation, so all comparisons sort
//! before diffing.
//!
//! Upstream source references:
//! - `acls.c:send_acl()` / `acls.c:receive_acl()` - POSIX/NFSv4 ACL wire.
//! - `xattrs.c:send_xattr()` / `xattrs.c:receive_xattr()` - xattr wire.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use super::helpers::{TestDir, spawn_with_timeout};

/// Environment variable that gates execution. Tests skip when this is unset
/// (or not equal to "1"). Set when running the metadata interop suite.
pub const GATE_ENV_VAR: &str = "OC_RSYNC_METADATA_INTEROP";

/// Wall-clock timeout for any single rsync invocation in the round-trip.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(180);

/// Convenience: log a skip reason and return from the test cleanly.
pub fn skip(reason: &str) {
    eprintln!("skip: {reason}");
}

/// Check whether the gate env var is set to "1".
pub fn gate_enabled() -> bool {
    env::var(GATE_ENV_VAR).ok().as_deref() == Some("1")
}

/// Check `command -v <cmd>` on PATH.
pub fn command_on_path(cmd: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {cmd} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Locate the oc-rsync binary built by cargo. Walks the standard places
/// nextest exposes plus the workspace `target/{debug,release,dist}` layout.
pub fn locate_oc_rsync() -> Option<PathBuf> {
    if let Some(env_path) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let path = PathBuf::from(env_path);
        if path.is_file() {
            return Some(path);
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for profile in ["debug", "release", "dist"] {
        let candidate = manifest_dir.join("target").join(profile).join("oc-rsync");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Locate an upstream rsync binary. Honours `OC_RSYNC_UPSTREAM`, then the
/// `target/interop/upstream-install/<ver>/bin/rsync` cache, then `which`.
pub fn locate_upstream_rsync() -> Option<PathBuf> {
    if let Some(p) = env::var_os("OC_RSYNC_UPSTREAM") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for version in ["3.4.2", "3.4.1", "3.1.3", "3.0.9"] {
        let in_tree = manifest_dir
            .join("target/interop/upstream-install")
            .join(version)
            .join("bin/rsync");
        if in_tree.is_file() {
            return Some(in_tree);
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
    let path = PathBuf::from(String::from_utf8(which.stdout).ok()?.trim());
    if path.is_file() { Some(path) } else { None }
}

/// Run an rsync-style binary with the canonical metadata flag set and
/// fail-fast on a non-zero exit.
pub fn run_rsync(bin: &Path, args: &[String]) -> io::Result<Output> {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let output = spawn_with_timeout(cmd, RUN_TIMEOUT)?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{} {:?} exited with {:?}\nstdout:\n{}\nstderr:\n{}",
            bin.display(),
            args,
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )));
    }
    Ok(output)
}

/// Build the standard `-aAX` argv for transferring `src/` into `dst`.
///
/// The trailing slash on the source is essential: it preserves the "copy
/// contents" semantic so paths line up across the four legs of the
/// round-trip.
pub fn aax_args(src: &Path, dst: &Path) -> Vec<String> {
    vec![
        "-aAX".to_string(),
        "--numeric-ids".to_string(),
        format!("{}/", src.display()),
        dst.to_string_lossy().into_owned(),
    ]
}

/// Provision the standard scratch layout: `<tmp>/src`, `<tmp>/dst1`,
/// `<tmp>/dst2`. Returns the three paths plus the owning `TestDir`.
pub fn make_scratch() -> io::Result<(TestDir, PathBuf, PathBuf, PathBuf)> {
    let dir = TestDir::new()?;
    let src = dir.mkdir("src")?;
    let dst1 = dir.mkdir("dst1")?;
    let dst2 = dir.mkdir("dst2")?;
    Ok((dir, src, dst1, dst2))
}

/// A canonicalised metadata record for one file/directory.
///
/// `xattrs` and `acl_text` are sorted/normalised so two records compare
/// equal exactly when their on-disk metadata is equivalent for the purposes
/// of the interop assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataRecord {
    /// Path relative to the tree root.
    pub rel: PathBuf,
    /// Sorted list of `(key, value)` xattr pairs, hex-encoded values.
    pub xattrs: Vec<(String, String)>,
    /// ACL text dump (`getfacl -p` style) with comment lines stripped, or
    /// an empty string when the platform has no POSIX ACL tooling.
    pub acl_text: String,
}

/// Build a sorted `BTreeMap<rel_path, MetadataRecord>` by walking `root`.
pub fn snapshot<F>(root: &Path, mut collect: F) -> io::Result<BTreeMap<PathBuf, MetadataRecord>>
where
    F: FnMut(&Path, &Path) -> io::Result<MetadataRecord>,
{
    let mut out = BTreeMap::new();
    walk(root, root, &mut |abs, rel| {
        let rec = collect(abs, rel)?;
        out.insert(rel.to_path_buf(), rec);
        Ok(())
    })?;
    Ok(out)
}

fn walk(
    base: &Path,
    current: &Path,
    visit: &mut dyn FnMut(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let abs = entry.path();
        let rel = abs.strip_prefix(base).unwrap_or(&abs).to_path_buf();
        visit(&abs, &rel)?;
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk(base, &abs, visit)?;
        }
    }
    Ok(())
}

/// Diff two metadata snapshots, returning a list of human-readable
/// mismatches. Empty result means the snapshots agree.
pub fn diff_snapshots(
    left_label: &str,
    left: &BTreeMap<PathBuf, MetadataRecord>,
    right_label: &str,
    right: &BTreeMap<PathBuf, MetadataRecord>,
) -> Vec<String> {
    let mut errors = Vec::new();
    for path in left.keys() {
        if !right.contains_key(path) {
            errors.push(format!(
                "{}: path {} present in {} only",
                path.display(),
                path.display(),
                left_label
            ));
        }
    }
    for path in right.keys() {
        if !left.contains_key(path) {
            errors.push(format!(
                "{}: path {} present in {} only",
                path.display(),
                path.display(),
                right_label
            ));
        }
    }
    for (path, l) in left {
        let Some(r) = right.get(path) else { continue };
        if l.xattrs != r.xattrs {
            errors.push(format!(
                "xattr mismatch at {}\n  {}: {:?}\n  {}: {:?}",
                path.display(),
                left_label,
                l.xattrs,
                right_label,
                r.xattrs,
            ));
        }
        if l.acl_text != r.acl_text {
            errors.push(format!(
                "ACL mismatch at {}\n  {}:\n{}\n  {}:\n{}",
                path.display(),
                left_label,
                indent(&l.acl_text),
                right_label,
                indent(&r.acl_text),
            ));
        }
    }
    errors
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}
