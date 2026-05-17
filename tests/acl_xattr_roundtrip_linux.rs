//! Linux round-trip parity: POSIX ACLs + NFSv4 ACLs + xattrs vs upstream
//! rsync 3.4.1 with `-aAX`.
//!
//! Builds a source tree under `/tmp`, stamps each file with a mix of
//! - POSIX access ACLs and default ACLs (`setfacl -m u:nobody:rwx,d:u:nobody:rwx`),
//! - user xattrs (`user.test.color=blue`), and
//! - system xattrs where the kernel permits them,
//!
//! then runs the tree through two round-trips:
//! - `oc-rsync -aAX src/ dst1/` then `rsync -aAX dst1/ dst2/`
//! - `rsync -aAX src/ dst1/` then `oc-rsync -aAX dst1/ dst2/`
//!
//! After each round-trip the harness compares per-path ACL text and xattr
//! key/value pairs between `src/` and `dst2/`. A divergence here points at
//! a wire-format incompatibility between the two implementations.
//!
//! GATE: `OC_RSYNC_METADATA_INTEROP=1`. Skips cleanly when unset or when
//! the surrounding tooling/filesystem cannot satisfy the fixture.
//!
//! Upstream source references:
//! - `acls.c:rsync_xal_h2l()` / `rsync_xal_l2h()` - host/wire ACL conversion.
//! - `xattrs.c:rsync_xal_get()` / `rsync_xal_set()` - xattr enumeration.

#![cfg(target_os = "linux")]

mod integration;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use integration::acl_xattr_interop_harness::{
    GATE_ENV_VAR, MetadataRecord, aax_args, command_on_path, diff_snapshots, gate_enabled,
    locate_oc_rsync, locate_upstream_rsync, make_scratch, run_rsync, skip, snapshot,
};

/// Tools the Linux fixture needs in `PATH`. Missing any of them -> skip.
const REQUIRED_TOOLS: &[&str] = &["setfacl", "getfacl", "setfattr", "getfattr"];

#[test]
fn acl_xattr_roundtrip_linux_oc_then_upstream() {
    let Some(ctx) = TestContext::try_setup() else {
        return;
    };
    let TestContext {
        _dir,
        src,
        dst1,
        dst2,
        oc_bin,
        upstream_bin,
    } = ctx;

    // Leg 1: oc-rsync sender -> dst1.
    run_rsync(&oc_bin, &aax_args(&src, &dst1)).expect("oc-rsync src -> dst1 failed");
    // Leg 2: upstream rsync sender -> dst2.
    run_rsync(&upstream_bin, &aax_args(&dst1, &dst2)).expect("upstream dst1 -> dst2 failed");

    let src_snap = snapshot(&src, collect_record).expect("snapshot src");
    let dst_snap = snapshot(&dst2, collect_record).expect("snapshot dst2");
    let diff = diff_snapshots("src", &src_snap, "dst2", &dst_snap);
    assert!(
        diff.is_empty(),
        "oc-rsync->upstream round-trip metadata divergence:\n{}",
        diff.join("\n")
    );
}

#[test]
fn acl_xattr_roundtrip_linux_upstream_then_oc() {
    let Some(ctx) = TestContext::try_setup() else {
        return;
    };
    let TestContext {
        _dir,
        src,
        dst1,
        dst2,
        oc_bin,
        upstream_bin,
    } = ctx;

    // Leg 1: upstream rsync sender -> dst1.
    run_rsync(&upstream_bin, &aax_args(&src, &dst1)).expect("upstream src -> dst1 failed");
    // Leg 2: oc-rsync sender -> dst2.
    run_rsync(&oc_bin, &aax_args(&dst1, &dst2)).expect("oc-rsync dst1 -> dst2 failed");

    let src_snap = snapshot(&src, collect_record).expect("snapshot src");
    let dst_snap = snapshot(&dst2, collect_record).expect("snapshot dst2");
    let diff = diff_snapshots("src", &src_snap, "dst2", &dst_snap);
    assert!(
        diff.is_empty(),
        "upstream->oc-rsync round-trip metadata divergence:\n{}",
        diff.join("\n")
    );
}

/// Per-test setup state. Returns `None` after logging the skip reason
/// whenever a precondition fails.
struct TestContext {
    _dir: integration::helpers::TestDir,
    src: PathBuf,
    dst1: PathBuf,
    dst2: PathBuf,
    oc_bin: PathBuf,
    upstream_bin: PathBuf,
}

impl TestContext {
    fn try_setup() -> Option<Self> {
        if !gate_enabled() {
            skip(&format!(
                "{GATE_ENV_VAR} not set to 1; opt in to run ACL/xattr interop tests"
            ));
            return None;
        }
        for tool in REQUIRED_TOOLS {
            if !command_on_path(tool) {
                skip(&format!(
                    "required tool not on PATH: {tool} (install acl + attr packages)"
                ));
                return None;
            }
        }
        let oc_bin = match locate_oc_rsync() {
            Some(p) => p,
            None => {
                skip("oc-rsync binary not located");
                return None;
            }
        };
        let upstream_bin = match locate_upstream_rsync() {
            Some(p) => p,
            None => {
                skip(
                    "no upstream rsync found (set OC_RSYNC_UPSTREAM or install \
                     target/interop/upstream-install/<ver>/bin/rsync)",
                );
                return None;
            }
        };

        let (dir, src, dst1, dst2) = match make_scratch() {
            Ok(t) => t,
            Err(e) => {
                skip(&format!("could not create scratch dir: {e}"));
                return None;
            }
        };

        if let Err(reason) = build_fixture(&src) {
            skip(&format!(
                "fixture build failed (likely no ACL/xattr support on tmpfs): {reason}"
            ));
            return None;
        }

        Some(Self {
            _dir: dir,
            src,
            dst1,
            dst2,
            oc_bin,
            upstream_bin,
        })
    }
}

/// Populate the source tree and stamp each entry with the test metadata.
///
/// Returns `Err` if any setfacl/setfattr probe fails: that signals the
/// fixture cannot run on this filesystem (e.g. tmpfs without ACL support)
/// and the caller should skip rather than fail.
fn build_fixture(src: &Path) -> io::Result<()> {
    // Tree layout: a few files plus a nested directory so we exercise both
    // access ACLs (files) and default ACLs (directories).
    fs::create_dir_all(src.join("nested"))?;
    fs::write(src.join("flat.txt"), b"flat-content")?;
    fs::write(src.join("nested/deep.txt"), b"deep-content")?;
    fs::write(src.join("nested/extra.bin"), b"\x00\x01\x02\x03ABCD")?;

    // POSIX access ACLs on files. `nobody` is universally present on Linux
    // base systems; if not, setfacl errors out and we skip.
    set_posix_acl(&src.join("flat.txt"), "u:nobody:rwx", false)?;
    set_posix_acl(&src.join("nested/deep.txt"), "u:nobody:rwx", false)?;
    // Access + default ACLs on the directory.
    set_posix_acl(&src.join("nested"), "u:nobody:rwx", false)?;
    set_posix_acl(&src.join("nested"), "u:nobody:rwx", true)?;

    // User xattrs on all files. System xattrs require root, so we only
    // probe `security.test` and ignore failure: the fixture stays valid
    // either way.
    set_user_xattr(&src.join("flat.txt"), "user.test.color", "blue")?;
    set_user_xattr(&src.join("nested/deep.txt"), "user.test.color", "red")?;
    set_user_xattr(&src.join("nested/extra.bin"), "user.test.color", "green")?;
    set_user_xattr(&src.join("nested/extra.bin"), "user.test.shape", "octagon")?;

    Ok(())
}

/// Apply a POSIX ACL entry via `setfacl`. `default` flips to `setfacl -d`.
fn set_posix_acl(path: &Path, entry: &str, default: bool) -> io::Result<()> {
    let mut cmd = Command::new("setfacl");
    if default {
        cmd.arg("-d");
    }
    cmd.arg("-m").arg(entry).arg(path);
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "setfacl {} {} failed: {}",
            if default { "-d -m" } else { "-m" },
            entry,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

/// Apply a user-namespace xattr via `setfattr`.
fn set_user_xattr(path: &Path, name: &str, value: &str) -> io::Result<()> {
    let output = Command::new("setfattr")
        .arg("-n")
        .arg(name)
        .arg("-v")
        .arg(value)
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "setfattr -n {} -v {} failed: {}",
            name,
            value,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

/// Build a `MetadataRecord` for one path by shelling out to `getfacl` and
/// `getfattr`. Output is normalised so two records compare equal exactly
/// when the on-disk metadata matches.
fn collect_record(abs: &Path, rel: &Path) -> io::Result<MetadataRecord> {
    Ok(MetadataRecord {
        rel: rel.to_path_buf(),
        xattrs: read_xattrs(abs)?,
        acl_text: read_acl_text(abs)?,
    })
}

fn read_xattrs(path: &Path) -> io::Result<Vec<(String, String)>> {
    // `-d` dumps values, `-m -` matches all names (no `user.` filter), `-e hex`
    // round-trips binary safely. `--no-dereference` mirrors rsync's policy of
    // operating on the link itself.
    let output = Command::new("getfattr")
        .arg("-d")
        .arg("-m")
        .arg("-")
        .arg("-e")
        .arg("hex")
        .arg("--no-dereference")
        .arg("--absolute-names")
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "getfattr on {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let mut pairs = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            pairs.push((k.to_string(), v.trim_matches('"').to_string()));
        }
    }
    pairs.sort();
    Ok(pairs)
}

fn read_acl_text(path: &Path) -> io::Result<String> {
    // `-p` keeps relative paths verbatim, `--omit-header` drops the per-file
    // comment block. We strip the residual `# file:` style metadata lines to
    // keep the diff focused on ACEs, not paths or umask hints.
    let output = Command::new("getfacl")
        .arg("-p")
        .arg("--omit-header")
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "getfacl on {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let mut lines: Vec<&str> = body
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    lines.sort();
    Ok(lines.join("\n"))
}
