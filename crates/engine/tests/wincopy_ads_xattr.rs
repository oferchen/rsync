//! Windows fast-copy (`CopyFileExW`) xattr/ADS gating tests.
//!
//! On Windows an rsync "extended attribute" is an NTFS Alternate Data Stream
//! (`file:name:$DATA`). `CopyFileExW` copies every named stream verbatim, so
//! the local-copy fast path must reproduce the portable path's behaviour:
//! without `-X` the destination carries no source streams, with `-X` the
//! streams are kept, and a selective xattr `--filter` excluding a stream name
//! drops that stream. Mirrors upstream rsync: xattrs are only transferred when
//! `--xattrs` is requested (`xattrs.c:rsync_xal_set()`).

#![cfg(all(windows, feature = "xattr"))]

use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use engine::local_copy::{
    FilterProgram, FilterProgramEntry, LocalCopyExecution, LocalCopyOptions, LocalCopyPlan,
};
use filters::FilterRule;
use tempfile::tempdir;

/// Writes `value` into the named alternate data stream `file:name:$DATA`.
fn write_ads(file: &Path, name: &str, value: &[u8]) {
    let stream = format!("{}:{}", file.display(), name);
    let mut handle = fs::File::create(&stream).expect("create ADS");
    handle.write_all(value).expect("write ADS");
}

/// Reads the named alternate data stream, or `None` when it does not exist.
fn read_ads(file: &Path, name: &str) -> Option<Vec<u8>> {
    let stream = format!("{}:{}", file.display(), name);
    let mut handle = fs::File::open(&stream).ok()?;
    let mut buf = Vec::new();
    handle.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Returns `true` when the temp volume supports NTFS alternate data streams.
/// FAT/exFAT-backed runners cannot host streams, so those cases skip.
fn ads_supported(file: &Path) -> bool {
    let stream = format!("{}:oc_probe", file.display());
    match fs::File::create(&stream) {
        Ok(mut h) => h.write_all(b"1").is_ok(),
        Err(_) => false,
    }
}

/// Runs a plain recursive local copy (whole-file, so the `CopyFileExW` fast
/// path is eligible) with the supplied options and returns the dest file path.
fn run_copy(source_dir: &Path, dest_dir: &Path, options: LocalCopyOptions) {
    let operands = vec![
        source_dir.to_path_buf().into_os_string(),
        dest_dir.to_path_buf().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");
}

/// (a) Without `-X`, the fast path must strip the source ADS so the
/// destination matches a freshly written portable-path copy.
#[test]
fn wincopy_without_xattrs_drops_ads() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let source_file = source.join("payload.bin");
    fs::write(&source_file, b"data").expect("write source");
    if !ads_supported(&source_file) {
        return;
    }
    write_ads(&source_file, "stream", b"secret");

    let options = LocalCopyOptions::default()
        .recursive(true)
        .whole_file(true)
        .xattrs(false);
    run_copy(&source, &dest, options);

    let dest_file = dest.join("src").join("payload.bin");
    assert!(dest_file.exists(), "destination payload must exist");
    assert_eq!(
        read_ads(&dest_file, "stream"),
        None,
        "ADS must not survive a fast copy when --xattrs is off"
    );
}

/// (b) With `-X`, the fast path keeps the source ADS verbatim.
#[test]
fn wincopy_with_xattrs_keeps_ads() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let source_file = source.join("payload.bin");
    fs::write(&source_file, b"data").expect("write source");
    if !ads_supported(&source_file) {
        return;
    }
    write_ads(&source_file, "stream", b"secret");

    let options = LocalCopyOptions::default()
        .recursive(true)
        .whole_file(true)
        .xattrs(true);
    run_copy(&source, &dest, options);

    let dest_file = dest.join("src").join("payload.bin");
    assert!(dest_file.exists(), "destination payload must exist");
    assert_eq!(
        read_ads(&dest_file, "stream").as_deref(),
        Some(b"secret".as_slice()),
        "ADS must survive a fast copy when --xattrs is on"
    );
}

/// (c) A selective xattr filter excluding the stream name drops it even with
/// `-X` on. The presence of xattr filter rules also forces the fast path to
/// defer to the portable copy path, which applies the per-name filter.
#[test]
fn wincopy_selective_filter_excludes_named_stream() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let source_file = source.join("payload.bin");
    fs::write(&source_file, b"data").expect("write source");
    if !ads_supported(&source_file) {
        return;
    }
    write_ads(&source_file, "keep", b"one");
    write_ads(&source_file, "drop", b"two");

    let program = FilterProgram::new([FilterProgramEntry::Rule(
        FilterRule::exclude("drop").with_xattr_only(true),
    )])
    .expect("filter program");

    let options = LocalCopyOptions::default()
        .recursive(true)
        .whole_file(true)
        .xattrs(true)
        .with_filter_program(Some(program));
    run_copy(&source, &dest, options);

    let dest_file = dest.join("src").join("payload.bin");
    assert!(dest_file.exists(), "destination payload must exist");
    assert_eq!(
        read_ads(&dest_file, "keep").as_deref(),
        Some(b"one".as_slice()),
        "unfiltered ADS must survive"
    );
    assert_eq!(
        read_ads(&dest_file, "drop"),
        None,
        "ADS excluded by a selective xattr filter must be dropped"
    );
}
