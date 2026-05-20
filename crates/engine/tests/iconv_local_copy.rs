//! Integration tests for `--iconv=LOCAL,REMOTE` in the local-copy
//! executor.
//!
//! Confirms that filenames are transcoded from LOCAL to REMOTE charset
//! when a [`FilenameConverter`] is attached to
//! [`LocalCopyOptions`]. Upstream rsync's local-copy mode opens
//! `ic_send = iconv_open(UTF8, LOCAL)` on the sender and
//! `ic_recv = iconv_open(REMOTE, UTF8)` on the receiver
//! (`rsync.c:118-140`); composing those is equivalent to a single
//! LOCAL -> REMOTE converter applied to the destination filename.
//!
//! # Upstream Reference
//!
//! - `rsync.c:118-140` `setup_iconv()` - LOCAL/REMOTE split and
//!   `iconv_open` calls.
//! - `flist.c:1579-1603` `send_file_name()` sender filename transcode.
//! - `flist.c:738-754` `recv_file_entry()` receiver filename transcode.

// macOS APFS and Windows NTFS both reject the raw 0xe9 byte that
// `--iconv=UTF-8,ISO-8859-1` produces for the canonical "caf\xe9.txt"
// scenario. Restrict to Linux where tmpfs/ext4 accept arbitrary byte
// sequences and faithfully round-trip them through `read_dir`. The
// production iconv path itself is platform-agnostic (`executor::iconv`
// uses `OsStr::as_encoded_bytes` on non-Unix); the iconv setup wiring
// and the receiver-side reference behaviour are covered by the
// per-crate unit tests in `protocol::iconv`,
// `engine::local_copy::executor::iconv`, and
// `core::client::config::iconv` on all platforms.
#![cfg(all(target_os = "linux", feature = "iconv"))]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use protocol::iconv::FilenameConverter;
use tempfile::tempdir;

/// Reads the immediate file/dir basenames inside `dir` and returns them
/// as raw byte vectors so callers can match against non-UTF-8 bytes.
fn list_raw_names(dir: &std::path::Path) -> Vec<Vec<u8>> {
    let mut names: Vec<Vec<u8>> = fs::read_dir(dir)
        .expect("read_dir")
        .map(|entry| entry.expect("dirent").file_name().as_bytes().to_vec())
        .collect();
    names.sort();
    names
}

/// Source `café.txt` (UTF-8 bytes `c3 a9`) copied with
/// `--iconv=UTF-8,ISO-8859-1` lands at the destination as `café.txt`
/// encoded in Latin-1, i.e. a single 0xe9 byte for the accented `é`.
#[test]
fn iconv_utf8_to_latin1_transcodes_filename_on_disk() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Source file: "café.txt" with UTF-8 encoding of 'é' (c3 a9).
    let utf8_name = OsStr::from_bytes(b"caf\xc3\xa9.txt");
    fs::write(source.join(utf8_name), b"payload").expect("write source");

    let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("converter");
    let operands = vec![source.into_os_string(), dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .with_iconv(Some(converter));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");

    // The "dest/source" subdirectory carries the copied tree (no
    // trailing slash on the source -> upstream behaviour). Its single
    // entry should be the Latin-1 encoding of "café.txt": the 'é' is
    // a single byte 0xe9 instead of c3 a9.
    let dest_root = temp.path().join("dest").join("source");
    let names = list_raw_names(&dest_root);
    assert_eq!(
        names,
        vec![b"caf\xe9.txt".to_vec()],
        "expected Latin-1 'caf\\xe9.txt' on disk; got {names:?}"
    );

    // Payload should still be intact - iconv only touches filenames.
    let copied = fs::read(dest_root.join(OsStr::from_bytes(b"caf\xe9.txt"))).expect("read dest");
    assert_eq!(copied, b"payload");
}

/// Reverse direction: Latin-1 source byte `e9` is transcoded to UTF-8
/// `c3 a9` on the destination side.
#[test]
fn iconv_latin1_to_utf8_transcodes_filename_on_disk() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Source file name: raw Latin-1 byte 0xe9 for 'é'.
    let latin1_name = OsStr::from_bytes(b"caf\xe9.txt");
    fs::write(source.join(latin1_name), b"payload").expect("write source");

    let converter = FilenameConverter::new("ISO-8859-1", "UTF-8").expect("converter");
    let operands = vec![source.into_os_string(), dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .with_iconv(Some(converter));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");

    let dest_root = temp.path().join("dest").join("source");
    let names = list_raw_names(&dest_root);
    assert_eq!(
        names,
        vec![b"caf\xc3\xa9.txt".to_vec()],
        "expected UTF-8 'caf\\xc3\\xa9.txt' on disk; got {names:?}"
    );

    let utf8_name = OsString::from_vec(b"caf\xc3\xa9.txt".to_vec());
    let copied = fs::read(dest_root.join(utf8_name)).expect("read dest");
    assert_eq!(copied, b"payload");
}

/// Without `--iconv` configured, filenames pass through verbatim so the
/// destination basename equals the source basename byte-for-byte. This
/// is the no-regression control: confirms the iconv path stays cold on
/// the common case and the executor still produces the same on-disk
/// layout as before the feature wiring.
#[test]
fn no_iconv_preserves_source_filename_bytes() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let utf8_name = OsStr::from_bytes(b"caf\xc3\xa9.txt");
    fs::write(source.join(utf8_name), b"payload").expect("write source");

    let operands = vec![source.into_os_string(), dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().recursive(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");

    let dest_root = temp.path().join("dest").join("source");
    let names = list_raw_names(&dest_root);
    assert_eq!(
        names,
        vec![b"caf\xc3\xa9.txt".to_vec()],
        "no-iconv path must preserve source bytes; got {names:?}"
    );
}

/// Recursive subdirectory copy with `--iconv=UTF-8,ISO-8859-1` applies
/// the conversion to every nested filename. This guards the per-entry
/// `process_planned_entry` dispatch in addition to the top-level
/// handler.
#[test]
fn iconv_applies_to_nested_filenames_recursively() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let nested = source.join("subdir");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&nested).expect("create nested");
    fs::create_dir_all(&dest).expect("create dest");

    let top_name = OsStr::from_bytes(b"caf\xc3\xa9.txt");
    let leaf_name = OsStr::from_bytes(b"r\xc3\xa9sum\xc3\xa9.txt");
    fs::write(source.join(top_name), b"top").expect("write top");
    fs::write(nested.join(leaf_name), b"leaf").expect("write leaf");

    let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("converter");
    let operands = vec![source.into_os_string(), dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .with_iconv(Some(converter));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");

    let dest_root = temp.path().join("dest").join("source");
    let top_names = list_raw_names(&dest_root);
    assert!(
        top_names.contains(&b"caf\xe9.txt".to_vec()),
        "top-level Latin-1 name missing; got {top_names:?}"
    );
    assert!(top_names.contains(&b"subdir".to_vec()), "subdir missing");

    let leaf_dir = dest_root.join("subdir");
    let leaf_names = list_raw_names(&leaf_dir);
    assert_eq!(
        leaf_names,
        vec![b"r\xe9sum\xe9.txt".to_vec()],
        "nested Latin-1 name missing; got {leaf_names:?}"
    );
}
