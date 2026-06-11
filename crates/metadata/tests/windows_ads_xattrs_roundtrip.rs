//! End-to-end Windows NTFS Alternate Data Stream round-trip via the
//! oc-rsync `--xattrs` transfer path.
//!
//! Sibling unit tests in `crates/metadata/src/xattr_windows.rs::tests`
//! cover the FFI primitives (`FindFirstStreamW` enumeration,
//! `CreateFileW` on `path:name:$DATA`, `DeleteFileW` on a stream path).
//! Those tests verify the helpers in isolation but never exercise the
//! full receiver pipeline: feature gating in the preflight check,
//! `--xattrs` capability negotiation, the cross-platform xattr wire
//! decoder, and the `apply_xattrs_from_list` dispatch into the Windows
//! backend.
//!
//! This module runs a real `oc-rsync --xattrs --archive` subprocess
//! against a source tree carrying ADS, then inspects the destination
//! tree through the public `read_xattrs_for_wire` API. The receiver
//! enumerates streams via `FindFirstStreamW` + `FindNextStreamW` and
//! returns each stream payload through `read_attribute`, so a green
//! round-trip proves the entire chain - preflight cfg, long-path helper
//! for FFI sites, ADS read/write - is wired end-to-end.
//!
//! ## Scenarios
//!
//! - `ads_zone_identifier_round_trips_through_xattrs`. Single stream
//!   case mimicking the Windows MOTW (Mark Of The Web) `Zone.Identifier`
//!   stream that browsers attach to downloaded files. Catches the
//!   common "user downloaded an installer and rsynced it elsewhere"
//!   workflow.
//!
//! - `ads_multi_stream_round_trips`. File carrying two named streams.
//!   Forces `FindNextStreamW` iteration in `list_attributes` and
//!   verifies both stream payloads land at the destination, catching
//!   regressions where only the first stream survives.
//!
//! ## Skip conditions
//!
//! Both tests skip cleanly (no failure) when:
//! - The host is not Windows (file is gated via `#![cfg(...)]`).
//! - The `xattr` feature is not enabled at build time.
//! - The backing scratch volume does not support ADS (FAT32, exFAT, or
//!   a non-NTFS mount). Probed by attempting to write a sentinel stream
//!   to a temp file; failure means the volume rejects ADS.
//! - The `oc-rsync` binary cannot be located via `CARGO_BIN_EXE_oc-rsync`
//!   or `target/{release,debug,dist}/oc-rsync.exe` relative to the
//!   workspace root.

#![cfg(all(target_os = "windows", feature = "xattr"))]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use metadata::read_xattrs_for_wire;
use tempfile::tempdir;

/// Bare stream name written via the NTFS `path:streamname` path syntax.
/// Mimics the Mark Of The Web stream browsers attach to downloads.
const ZONE_IDENTIFIER: &str = "Zone.Identifier";

/// Realistic MOTW payload. The exact bytes are part of the round-trip
/// contract: the destination must observe an identical stream.
const ZONE_IDENTIFIER_PAYLOAD: &[u8] = b"[ZoneTransfer]\r\nZoneId=3\r\n";

/// Wire-format name for the Zone.Identifier xattr observed by
/// `read_xattrs_for_wire`. The Windows backend in `xattr_windows.rs`
/// does not insert a `user.` prefix; stream names cross the wire
/// verbatim.
const ZONE_IDENTIFIER_WIRE: &[u8] = b"Zone.Identifier";

/// Custom non-MOTW stream name used by the multi-stream scenario to
/// force `FindNextStreamW` iteration beyond the first stream.
const CUSTOM_STREAM: &str = "oc_rsync_test_stream";

/// Wire-format counterpart of `CUSTOM_STREAM`.
const CUSTOM_STREAM_WIRE: &[u8] = b"oc_rsync_test_stream";

/// Custom stream payload, distinct from the MOTW payload so the
/// assertions can tell the two streams apart on the destination.
const CUSTOM_STREAM_PAYLOAD: &[u8] = b"custom-ads-payload-bytes";

/// Builds the NTFS `path:streamname` form so a stream can be opened
/// through standard `std::fs` APIs without touching the Win32 FFI from
/// the test.
fn ads_path(base: &Path, stream: &str) -> PathBuf {
    let mut s = base.as_os_str().to_owned();
    s.push(":");
    s.push(stream);
    PathBuf::from(s)
}

/// Locate the oc-rsync binary built for this workspace.
///
/// Order:
/// 1. `CARGO_BIN_EXE_oc-rsync` (set automatically by cargo for tests in
///    the binary's owning package; metadata tests do not get this for
///    free so we treat it as best-effort).
/// 2. `target/{release,debug,dist}/oc-rsync.exe` relative to the
///    workspace root (two directories above `CARGO_MANIFEST_DIR` which
///    is the `metadata` crate dir).
fn locate_oc_rsync() -> Option<PathBuf> {
    if let Some(env_path) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let p = PathBuf::from(env_path);
        if p.is_file() {
            return Some(p);
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent()?.parent()?;
    for profile in ["release", "debug", "dist"] {
        let candidate = workspace_root
            .join("target")
            .join(profile)
            .join("oc-rsync.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Probes whether the volume behind `file` supports ADS. Writes a
/// sentinel stream via the standard `path:streamname` syntax and reads
/// it back; any error means the volume rejects ADS (FAT32, exFAT, or a
/// sandboxed scratch directory) and the test must skip rather than
/// fail.
fn ads_supported(file: &Path) -> bool {
    let probe = ads_path(file, "oc_rsync_ads_probe");
    if fs::write(&probe, b"1").is_err() {
        return false;
    }
    let ok = fs::read(&probe).map(|v| v == b"1").unwrap_or(false);
    // Best-effort cleanup; tempdir teardown will discard the carrier
    // file even if the stream lingers.
    let _ = fs::remove_file(&probe);
    ok
}

/// Logs a skip reason and returns cleanly. Matches the convention used
/// by the workspace-level interop harness.
fn skip(reason: &str) {
    eprintln!("skipped: {reason}");
}

/// Reads `path:streamname` as raw bytes for assertion against the
/// expected payload. Avoids `read_to_string` because ADS payloads can
/// be arbitrary binary and the unit-level fidelity matters here.
fn read_stream_bytes(file: &Path, stream: &str) -> std::io::Result<Vec<u8>> {
    fs::read(ads_path(file, stream))
}

/// Asserts that the wire-format xattr list reported for `path` contains
/// an entry named `wire_name` whose datum matches `expected`. Reading
/// via the public `read_xattrs_for_wire` API exercises the receiver's
/// own enumeration path (FindFirstStreamW + FindNextStreamW) instead of
/// trusting an isolated stream read.
fn assert_wire_entry(path: &Path, wire_name: &[u8], expected: &[u8]) {
    let list = read_xattrs_for_wire(path, false, true, 0).expect("read xattrs back");
    let entry = list
        .iter()
        .find(|e| e.name() == wire_name)
        .unwrap_or_else(|| {
            let names: Vec<String> = list
                .iter()
                .map(|e| String::from_utf8_lossy(e.name()).into_owned())
                .collect();
            panic!(
                "wire entry {:?} missing on destination; saw {names:?}",
                String::from_utf8_lossy(wire_name),
            );
        });
    assert_eq!(
        entry.datum(),
        expected,
        "wire entry {:?} payload diverged",
        String::from_utf8_lossy(wire_name),
    );
}

/// Round-trip a Zone.Identifier ADS through oc-rsync's `--xattrs` path.
///
/// Steps:
/// 1. Build a source file with a `Zone.Identifier` ADS mimicking the
///    Mark Of The Web stream browsers attach to downloads.
/// 2. Invoke `oc-rsync --xattrs --archive src/ dst/` as a subprocess.
/// 3. Assert the destination file carries an identical
///    `Zone.Identifier` stream with byte-identical payload, both via
///    direct `path:streamname` read and via the public
///    `read_xattrs_for_wire` API the receiver itself uses.
/// 4. Assert the destination file's primary data stream matches the
///    source's primary content.
#[test]
fn ads_zone_identifier_round_trips_through_xattrs() {
    let dir = tempdir().expect("create temp dir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    fs::create_dir_all(&src).expect("create src");
    fs::create_dir_all(&dst).expect("create dst");

    let src_file = src.join("downloaded.exe");
    let primary_content = b"fake exe content";
    fs::write(&src_file, primary_content).expect("seed primary stream");

    if !ads_supported(&src_file) {
        skip("ADS not supported on this volume");
        return;
    }

    fs::write(
        ads_path(&src_file, ZONE_IDENTIFIER),
        ZONE_IDENTIFIER_PAYLOAD,
    )
    .expect("seed Zone.Identifier on source");

    let oc_rsync = match locate_oc_rsync() {
        Some(p) => p,
        None => {
            skip("oc-rsync binary not found");
            return;
        }
    };

    let src_arg = format!("{}\\", src.display());
    let dst_arg = format!("{}\\", dst.display());
    let output = Command::new(&oc_rsync)
        .args(["--xattrs", "--archive"])
        .arg(&src_arg)
        .arg(&dst_arg)
        .output()
        .expect("spawn oc-rsync");
    assert!(
        output.status.success(),
        "oc-rsync exited with {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let dst_file = dst.join("downloaded.exe");
    assert!(dst_file.is_file(), "dest primary stream missing");

    let dst_primary = fs::read(&dst_file).expect("read dest primary");
    assert_eq!(
        dst_primary, primary_content,
        "primary data stream diverged after transfer",
    );

    let dst_ads =
        read_stream_bytes(&dst_file, ZONE_IDENTIFIER).expect("read dest Zone.Identifier stream");
    assert_eq!(
        dst_ads, ZONE_IDENTIFIER_PAYLOAD,
        "Zone.Identifier ADS content diverged after transfer",
    );

    assert_wire_entry(&dst_file, ZONE_IDENTIFIER_WIRE, ZONE_IDENTIFIER_PAYLOAD);
}

/// Round-trip a file carrying two named streams. Exercises the
/// `FindFirstStreamW` + `FindNextStreamW` iteration path so a regression
/// that drops every stream after the first is caught.
#[test]
fn ads_multi_stream_round_trips() {
    let dir = tempdir().expect("create temp dir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    fs::create_dir_all(&src).expect("create src");
    fs::create_dir_all(&dst).expect("create dst");

    let src_file = src.join("multi.bin");
    let primary_content = b"primary stream bytes";
    fs::write(&src_file, primary_content).expect("seed primary stream");

    if !ads_supported(&src_file) {
        skip("ADS not supported on this volume");
        return;
    }

    fs::write(
        ads_path(&src_file, ZONE_IDENTIFIER),
        ZONE_IDENTIFIER_PAYLOAD,
    )
    .expect("seed Zone.Identifier on source");
    fs::write(ads_path(&src_file, CUSTOM_STREAM), CUSTOM_STREAM_PAYLOAD)
        .expect("seed custom stream on source");

    // Sanity check the enumeration sees both streams on the source so a
    // failure on the destination side can be attributed to the transfer
    // rather than the seeding step.
    let src_wire = read_xattrs_for_wire(&src_file, false, true, 0).expect("list source streams");
    let src_names: Vec<String> = src_wire
        .iter()
        .map(|e| String::from_utf8_lossy(e.name()).into_owned())
        .collect();
    assert!(
        src_names.iter().any(|n| n == "Zone.Identifier"),
        "source missing Zone.Identifier: {src_names:?}",
    );
    assert!(
        src_names.iter().any(|n| n == "oc_rsync_test_stream"),
        "source missing oc_rsync_test_stream: {src_names:?}",
    );

    let oc_rsync = match locate_oc_rsync() {
        Some(p) => p,
        None => {
            skip("oc-rsync binary not found");
            return;
        }
    };

    let src_arg = format!("{}\\", src.display());
    let dst_arg = format!("{}\\", dst.display());
    let output = Command::new(&oc_rsync)
        .args(["--xattrs", "--archive"])
        .arg(&src_arg)
        .arg(&dst_arg)
        .output()
        .expect("spawn oc-rsync");
    assert!(
        output.status.success(),
        "oc-rsync exited with {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let dst_file = dst.join("multi.bin");
    assert!(dst_file.is_file(), "dest primary stream missing");

    let dst_primary = fs::read(&dst_file).expect("read dest primary");
    assert_eq!(
        dst_primary, primary_content,
        "primary data stream diverged after transfer",
    );

    // Direct per-stream reads catch payload divergence even when the
    // enumeration happens to agree.
    let dst_zone =
        read_stream_bytes(&dst_file, ZONE_IDENTIFIER).expect("read dest Zone.Identifier stream");
    assert_eq!(
        dst_zone, ZONE_IDENTIFIER_PAYLOAD,
        "Zone.Identifier ADS content diverged after transfer",
    );

    let dst_custom = read_stream_bytes(&dst_file, CUSTOM_STREAM).expect("read dest custom stream");
    assert_eq!(
        dst_custom, CUSTOM_STREAM_PAYLOAD,
        "custom ADS content diverged after transfer",
    );

    // Public-API enumeration crosses the FindFirstStreamW +
    // FindNextStreamW path so a regression that drops every stream after
    // the first surfaces here even if direct reads succeed.
    assert_wire_entry(&dst_file, ZONE_IDENTIFIER_WIRE, ZONE_IDENTIFIER_PAYLOAD);
    assert_wire_entry(&dst_file, CUSTOM_STREAM_WIRE, CUSTOM_STREAM_PAYLOAD);
}
