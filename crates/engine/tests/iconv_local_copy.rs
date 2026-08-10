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

/// Full round-trip: UTF-8 source -> Latin-1 intermediate -> UTF-8 final.
///
/// Creates source files whose UTF-8 filenames use five distinct Latin-1
/// accented characters (e-acute, n-tilde, u-diaeresis, o-diaeresis,
/// a-grave) spread across a flat file, a nested file, and a subdirectory.
/// The forward leg converts UTF-8 to ISO-8859-1 on disk; the reverse leg
/// converts ISO-8859-1 back to UTF-8. After both legs, every filename
/// must be byte-identical to the original and every file's payload must
/// be intact.
///
/// This exercises the composed iconv path that upstream rsync uses for
/// bidirectional syncs between machines with different locale charsets:
///   - Sender: `iconv_open(UTF-8, LOCAL)` then `iconv_open(REMOTE, UTF-8)`
///   - Receiver: `iconv_open(UTF-8, REMOTE)` then `iconv_open(LOCAL, UTF-8)`
///
/// upstream: rsync.c:118-140 setup_iconv(), flist.c:1579-1603 send,
///           flist.c:738-754 recv.
#[test]
fn iconv_utf8_latin1_round_trip_preserves_filenames() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let intermediate = temp.path().join("intermediate");
    let final_dest = temp.path().join("final");
    fs::create_dir_all(source.join("données")).expect("create nested dir");
    fs::create_dir_all(&intermediate).expect("create intermediate");
    fs::create_dir_all(&final_dest).expect("create final");

    // Five distinct Latin-1-representable accented characters:
    //   é (U+00E9, UTF-8 c3 a9, Latin-1 e9)
    //   ñ (U+00F1, UTF-8 c3 b1, Latin-1 f1)
    //   ü (U+00FC, UTF-8 c3 bc, Latin-1 fc)
    //   ö (U+00F6, UTF-8 c3 b6, Latin-1 f6)
    //   à (U+00E0, UTF-8 c3 a0, Latin-1 e0)
    let files: &[(&[u8], &[u8])] = &[
        // "café.txt" - e-acute
        (b"caf\xc3\xa9.txt", b"espresso"),
        // "señor.txt" - n-tilde
        (b"se\xc3\xb1or.txt", b"hola"),
        // "grüß.txt" - u-diaeresis (ß is also Latin-1: df)
        (b"gr\xc3\xbc\xc3\x9f.txt", b"hallo"),
        // "données/König.txt" - nested, o-diaeresis
        (b"donn\xc3\xa9es/K\xc3\xb6nig.txt", b"nested-payload"),
        // "voilà.txt" - a-grave
        (b"voil\xc3\xa0.txt", b"bonjour"),
    ];

    for &(name_bytes, payload) in files {
        let path = source.join(OsStr::from_bytes(name_bytes));
        fs::write(&path, payload).expect("write source file");
    }

    // ---- Forward leg: UTF-8 -> ISO-8859-1 ----
    let fwd_converter = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("forward converter");
    let fwd_operands = vec![
        source.into_os_string(),
        intermediate.clone().into_os_string(),
    ];
    let fwd_plan = LocalCopyPlan::from_operands(&fwd_operands).expect("forward plan");
    let fwd_options = LocalCopyOptions::default()
        .recursive(true)
        .with_iconv(Some(fwd_converter));

    fwd_plan
        .execute_with_options(LocalCopyExecution::Apply, fwd_options)
        .expect("forward local copy succeeds");

    // Verify intermediate filenames are in Latin-1 encoding.
    let inter_root = intermediate.join("source");
    let inter_names = list_raw_names(&inter_root);

    // Expected Latin-1 basenames (sorted):
    //   "café.txt"    -> caf e9 .txt
    //   "données"     -> donn e9 es     (directory)
    //   "grüß.txt"    -> gr fc df .txt
    //   "señor.txt"   -> se f1 or .txt
    //   "voilà.txt"   -> voil e0 .txt
    let expected_latin1: Vec<Vec<u8>> = {
        let mut v = vec![
            b"caf\xe9.txt".to_vec(),
            b"donn\xe9es".to_vec(),
            b"gr\xfc\xdf.txt".to_vec(),
            b"se\xf1or.txt".to_vec(),
            b"voil\xe0.txt".to_vec(),
        ];
        v.sort();
        v
    };
    assert_eq!(
        inter_names, expected_latin1,
        "intermediate must have Latin-1 encoded filenames; got {inter_names:?}"
    );

    // Verify nested file exists with Latin-1 encoded name.
    let inter_nested = inter_root.join(OsStr::from_bytes(b"donn\xe9es"));
    let nested_names = list_raw_names(&inter_nested);
    assert_eq!(
        nested_names,
        vec![b"K\xf6nig.txt".to_vec()],
        "nested Latin-1 name wrong; got {nested_names:?}"
    );

    // ---- Reverse leg: ISO-8859-1 -> UTF-8 ----
    let rev_converter = FilenameConverter::new("ISO-8859-1", "UTF-8").expect("reverse converter");
    let rev_operands = vec![
        inter_root.into_os_string(),
        final_dest.clone().into_os_string(),
    ];
    let rev_plan = LocalCopyPlan::from_operands(&rev_operands).expect("reverse plan");
    let rev_options = LocalCopyOptions::default()
        .recursive(true)
        .with_iconv(Some(rev_converter));

    rev_plan
        .execute_with_options(LocalCopyExecution::Apply, rev_options)
        .expect("reverse local copy succeeds");

    // The reverse leg copies inter_root (named "source") into final_dest,
    // so the tree lands at final_dest/source/.
    let final_root = final_dest.join("source");
    let final_names = list_raw_names(&final_root);

    // After the round-trip, filenames must be back in UTF-8.
    let expected_utf8: Vec<Vec<u8>> = {
        let mut v = vec![
            b"caf\xc3\xa9.txt".to_vec(),
            b"donn\xc3\xa9es".to_vec(),
            b"gr\xc3\xbc\xc3\x9f.txt".to_vec(),
            b"se\xc3\xb1or.txt".to_vec(),
            b"voil\xc3\xa0.txt".to_vec(),
        ];
        v.sort();
        v
    };
    assert_eq!(
        final_names, expected_utf8,
        "round-tripped filenames must match original UTF-8; got {final_names:?}"
    );

    // Verify nested round-trip.
    let final_nested_dir = OsString::from_vec(b"donn\xc3\xa9es".to_vec());
    let final_nested = final_root.join(&final_nested_dir);
    let final_nested_names = list_raw_names(&final_nested);
    assert_eq!(
        final_nested_names,
        vec![b"K\xc3\xb6nig.txt".to_vec()],
        "nested filename must round-trip to UTF-8; got {final_nested_names:?}"
    );

    // Verify every file's payload survived the round-trip intact.
    let payload_checks: &[(&[u8], &[u8])] = &[
        (b"caf\xc3\xa9.txt", b"espresso"),
        (b"se\xc3\xb1or.txt", b"hola"),
        (b"gr\xc3\xbc\xc3\x9f.txt", b"hallo"),
        (b"voil\xc3\xa0.txt", b"bonjour"),
    ];
    for &(name_bytes, expected_payload) in payload_checks {
        let path = final_root.join(OsStr::from_bytes(name_bytes));
        let actual = fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "read round-tripped file {:?}: {e}",
                String::from_utf8_lossy(name_bytes)
            )
        });
        assert_eq!(
            actual,
            expected_payload,
            "payload mismatch for {:?}",
            String::from_utf8_lossy(name_bytes)
        );
    }

    let nested_path = final_nested.join(OsStr::from_bytes(b"K\xc3\xb6nig.txt"));
    let nested_payload = fs::read(&nested_path).expect("read nested round-tripped file");
    assert_eq!(
        nested_payload, b"nested-payload",
        "nested payload must survive round-trip"
    );
}

/// Round-trip with a trailing slash on the source operand, which copies
/// directory *contents* rather than the directory itself. Verifies the
/// iconv path handles both source-as-subdirectory and source-as-contents
/// semantics correctly.
///
/// upstream: main.c:978-982 - trailing-slash causes XMIT_TOP_DIR
///           semantics on the source, copying contents into dest.
#[test]
fn iconv_round_trip_trailing_slash_copies_contents() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let intermediate = temp.path().join("intermediate");
    let final_dest = temp.path().join("final");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&intermediate).expect("create intermediate");
    fs::create_dir_all(&final_dest).expect("create final");

    // Two files with accented UTF-8 names.
    let name_a = OsStr::from_bytes(b"r\xc3\xa9sum\xc3\xa9.txt");
    let name_b = OsStr::from_bytes(b"\xc3\xbcber.txt");
    fs::write(source.join(name_a), b"cv-data").expect("write a");
    fs::write(source.join(name_b), b"uber-data").expect("write b");

    // Forward: trailing slash -> contents land directly in intermediate.
    let fwd_converter = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("forward converter");
    let mut src_os = source.clone().into_os_string();
    src_os.push("/");
    let fwd_operands = vec![src_os, intermediate.clone().into_os_string()];
    let fwd_plan = LocalCopyPlan::from_operands(&fwd_operands).expect("forward plan");
    let fwd_options = LocalCopyOptions::default()
        .recursive(true)
        .with_iconv(Some(fwd_converter));

    fwd_plan
        .execute_with_options(LocalCopyExecution::Apply, fwd_options)
        .expect("forward copy succeeds");

    // With trailing slash, files land directly in intermediate/ (no
    // "source" subdirectory).
    let inter_names = list_raw_names(&intermediate);
    let expected_latin1: Vec<Vec<u8>> = {
        let mut v = vec![b"r\xe9sum\xe9.txt".to_vec(), b"\xfcber.txt".to_vec()];
        v.sort();
        v
    };
    assert_eq!(
        inter_names, expected_latin1,
        "trailing-slash forward must produce Latin-1 names; got {inter_names:?}"
    );

    // Reverse: trailing slash on intermediate.
    let rev_converter = FilenameConverter::new("ISO-8859-1", "UTF-8").expect("reverse converter");
    let mut inter_os = intermediate.into_os_string();
    inter_os.push("/");
    let rev_operands = vec![inter_os, final_dest.clone().into_os_string()];
    let rev_plan = LocalCopyPlan::from_operands(&rev_operands).expect("reverse plan");
    let rev_options = LocalCopyOptions::default()
        .recursive(true)
        .with_iconv(Some(rev_converter));

    rev_plan
        .execute_with_options(LocalCopyExecution::Apply, rev_options)
        .expect("reverse copy succeeds");

    let final_names = list_raw_names(&final_dest);
    let expected_utf8: Vec<Vec<u8>> = {
        let mut v = vec![
            b"r\xc3\xa9sum\xc3\xa9.txt".to_vec(),
            b"\xc3\xbcber.txt".to_vec(),
        ];
        v.sort();
        v
    };
    assert_eq!(
        final_names, expected_utf8,
        "trailing-slash round-trip must recover UTF-8 names; got {final_names:?}"
    );

    // Payload integrity.
    let path_a = final_dest.join(OsStr::from_bytes(b"r\xc3\xa9sum\xc3\xa9.txt"));
    assert_eq!(fs::read(&path_a).expect("read a"), b"cv-data");

    let path_b = final_dest.join(OsStr::from_bytes(b"\xc3\xbcber.txt"));
    assert_eq!(fs::read(&path_b).expect("read b"), b"uber-data");
}
