//! macOS round-trip parity: Apple-specific xattrs vs upstream rsync 3.4.1
//! with `-aAX`.
//!
//! Apple stamps user-facing data into xattrs that drive Finder behaviour:
//! - `com.apple.FinderInfo` - 32-byte fixed-size icon/colour record.
//! - `com.apple.metadata:_kMDItemUserTags` - bplist of user tags.
//! - `com.apple.ResourceFork` - legacy resource fork blob.
//!
//! macOS POSIX ACLs exist but the userland surface is extremely limited
//! (`chmod +a`, `ls -le`) and the kernel re-canonicalises ACEs aggressively,
//! making byte-level diffs noisy. This test focuses on xattrs only.
//!
//! Round-trips both directions through upstream `rsync` (commonly installed
//! via Homebrew on `/opt/homebrew/bin/rsync` or `/usr/local/bin/rsync`):
//! - `oc-rsync -aAX src/ dst1/` then `rsync -aAX dst1/ dst2/`
//! - `rsync -aAX src/ dst1/` then `oc-rsync -aAX dst1/ dst2/`
//!
//! GATE: `OC_RSYNC_METADATA_INTEROP=1`. Skips cleanly when unset or when
//! the surrounding tooling cannot satisfy the fixture.

#![cfg(target_os = "macos")]

mod integration;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use integration::acl_xattr_interop_harness::{
    GATE_ENV_VAR, MetadataRecord, aax_args, command_on_path, diff_snapshots, gate_enabled,
    locate_oc_rsync, locate_upstream_rsync, make_scratch, run_rsync, skip, snapshot,
};

/// macOS Finder requires exactly 32 bytes for `com.apple.FinderInfo`. We
/// stamp a recognisable but inert payload (no type/creator) so Finder will
/// not try to rewrite it on access.
const FINDER_INFO_HEX: &str = concat!(
    "00000000", "00000000", "00000000", "00000000", "00000000", "00000000", "00000000", "00000000",
);

/// Opaque blob substituted for `_kMDItemUserTags`. The round-trip only
/// needs both sides to preserve the bytes verbatim; Finder treats anything
/// it cannot parse as "no tags", which is harmless for the test fixture.
const USER_TAGS_HEX: &str = "62706c697374303049696e7465726f7000496d6574616461746100";

/// Token resource fork. macOS preserves arbitrary opaque bytes here so any
/// stable blob round-trips fine for the parity check.
const RESOURCE_FORK_HEX: &str = "deadbeefcafef00d0123456789abcdef";

#[test]
fn acl_xattr_roundtrip_macos_oc_then_upstream() {
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

    run_rsync(&oc_bin, &aax_args(&src, &dst1)).expect("oc-rsync src -> dst1 failed");
    run_rsync(&upstream_bin, &aax_args(&dst1, &dst2)).expect("upstream dst1 -> dst2 failed");

    let src_snap = snapshot(&src, collect_record).expect("snapshot src");
    let dst_snap = snapshot(&dst2, collect_record).expect("snapshot dst2");
    let diff = diff_snapshots("src", &src_snap, "dst2", &dst_snap);
    assert!(
        diff.is_empty(),
        "oc-rsync->upstream round-trip xattr divergence:\n{}",
        diff.join("\n")
    );
}

#[test]
fn acl_xattr_roundtrip_macos_upstream_then_oc() {
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

    run_rsync(&upstream_bin, &aax_args(&src, &dst1)).expect("upstream src -> dst1 failed");
    run_rsync(&oc_bin, &aax_args(&dst1, &dst2)).expect("oc-rsync dst1 -> dst2 failed");

    let src_snap = snapshot(&src, collect_record).expect("snapshot src");
    let dst_snap = snapshot(&dst2, collect_record).expect("snapshot dst2");
    let diff = diff_snapshots("src", &src_snap, "dst2", &dst_snap);
    assert!(
        diff.is_empty(),
        "upstream->oc-rsync round-trip xattr divergence:\n{}",
        diff.join("\n")
    );
}

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
        if !command_on_path("xattr") {
            skip("xattr not on PATH (ship with macOS base; check PATH sanitisation)");
            return None;
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
                    "no upstream rsync found (install via brew or set OC_RSYNC_UPSTREAM); \
                     Apple-shipped rsync is too old for protocol 32",
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
                "fixture build failed (filesystem may not preserve Apple xattrs): {reason}"
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

fn build_fixture(src: &Path) -> io::Result<()> {
    fs::create_dir_all(src.join("nested"))?;
    fs::write(src.join("flat.txt"), b"flat-content")?;
    fs::write(src.join("nested/deep.txt"), b"deep-content")?;

    set_apple_xattr(
        &src.join("flat.txt"),
        "com.apple.FinderInfo",
        FINDER_INFO_HEX,
    )?;
    set_apple_xattr(
        &src.join("flat.txt"),
        "com.apple.metadata:_kMDItemUserTags",
        USER_TAGS_HEX,
    )?;
    set_apple_xattr(
        &src.join("nested/deep.txt"),
        "com.apple.ResourceFork",
        RESOURCE_FORK_HEX,
    )?;
    set_apple_xattr(
        &src.join("nested/deep.txt"),
        "com.apple.FinderInfo",
        FINDER_INFO_HEX,
    )?;
    Ok(())
}

/// Write a hex-encoded xattr via `xattr -wx <name> <hex> <path>`. The `-x`
/// flag is the documented way to feed binary values without shell-escape
/// hell.
fn set_apple_xattr(path: &Path, name: &str, hex_value: &str) -> io::Result<()> {
    let output = Command::new("xattr")
        .arg("-wx")
        .arg(name)
        .arg(hex_value)
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "xattr -wx {} ... {} failed: {}",
            name,
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

/// Build a `MetadataRecord` per file by listing xattrs and reading each one
/// in hex. macOS POSIX ACL state is intentionally omitted; see the module
/// docstring.
fn collect_record(abs: &Path, rel: &Path) -> io::Result<MetadataRecord> {
    let names_output = Command::new("xattr").arg(abs).output()?;
    if !names_output.status.success() {
        return Err(io::Error::other(format!(
            "xattr list on {} failed: {}",
            abs.display(),
            String::from_utf8_lossy(&names_output.stderr).trim()
        )));
    }
    let mut xattrs = Vec::new();
    for name in String::from_utf8_lossy(&names_output.stdout).lines() {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let value_output = Command::new("xattr")
            .arg("-px")
            .arg(name)
            .arg(abs)
            .output()?;
        if !value_output.status.success() {
            return Err(io::Error::other(format!(
                "xattr -px {} on {} failed: {}",
                name,
                abs.display(),
                String::from_utf8_lossy(&value_output.stderr).trim()
            )));
        }
        // `-px` returns a multi-line hex dump with embedded whitespace.
        // Strip all whitespace for stable byte-level comparison.
        let hex: String = String::from_utf8_lossy(&value_output.stdout)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        xattrs.push((name.to_string(), hex));
    }
    xattrs.sort();
    Ok(MetadataRecord {
        rel: rel.to_path_buf(),
        xattrs,
        // macOS ACLs intentionally not included; see module docstring.
        acl_text: String::new(),
    })
}
