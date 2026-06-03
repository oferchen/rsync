//! Integration tests verifying that `--iconv` interacts correctly with
//! filter rules containing non-ASCII patterns.
//!
//! In upstream rsync, filter rules are evaluated against **source-side**
//! filenames (in the local charset) before `ic_send` transcodes them for
//! the wire. The receiver's `ic_recv` then converts wire bytes to the
//! remote charset for the destination filesystem. For local-copy mode
//! these two converters compose to a single LOCAL -> REMOTE transcoding
//! applied at destination path construction time.
//!
//! The tests in this file confirm that filter evaluation and iconv
//! transcoding are independent: filters match source names in their
//! original encoding, while iconv transforms only the destination
//! filename. This matches upstream behaviour where `check_filter()`
//! in `exclude.c` runs before `send_file_name()` in `flist.c` applies
//! `iconvbufs(ic_send, ...)`.
//!
//! # Upstream Reference
//!
//! - `exclude.c:check_filter()` - first-match-wins filter evaluation
//!   against local-charset filenames.
//! - `flist.c:1579-1603` `send_file_name()` - `ic_send` conversion
//!   applied AFTER filter evaluation on the sender.
//! - `flist.c:738-754` `recv_file_entry()` - `ic_recv` conversion on
//!   the receiver side.
//! - `rsync.c:118-140` `setup_iconv()` - LOCAL/REMOTE split.

// macOS APFS and Windows NTFS reject raw non-UTF-8 bytes (e.g., the
// 0xe9 byte produced by UTF-8 -> ISO-8859-1 conversion). Restrict to
// Linux where tmpfs/ext4 faithfully round-trip arbitrary byte sequences.
#![cfg(all(target_os = "linux", feature = "iconv"))]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use filters::{FilterRule, FilterSet};
use protocol::iconv::FilenameConverter;
use tempfile::tempdir;

/// Reads the immediate file/dir basenames inside `dir` and returns them
/// as raw byte vectors for byte-level assertion.
fn list_raw_names(dir: &std::path::Path) -> Vec<Vec<u8>> {
    let mut names: Vec<Vec<u8>> = fs::read_dir(dir)
        .expect("read_dir")
        .map(|entry| entry.expect("dirent").file_name().as_bytes().to_vec())
        .collect();
    names.sort();
    names
}

// =========================================================================
// 1. Exclude pattern with non-ASCII characters under --iconv
// =========================================================================

/// An `--exclude='café*'` pattern should match source files named
/// `café.txt` (UTF-8). The excluded file must not appear at the
/// destination, even though iconv would have transcoded it to Latin-1.
///
/// This verifies that filter evaluation happens on the source-side
/// filename (pre-iconv), matching upstream's `check_filter()` ->
/// `send_file_name()` ordering.
#[test]
fn exclude_non_ascii_pattern_prevents_iconv_copy() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Two source files: one matching the exclude pattern, one not.
    let cafe_name = OsStr::from_bytes(b"caf\xc3\xa9.txt");
    let plain_name = OsStr::from_bytes(b"plain.txt");
    fs::write(source.join(cafe_name), b"excluded").expect("write cafe");
    fs::write(source.join(plain_name), b"included").expect("write plain");

    let filter_set =
        FilterSet::from_rules([FilterRule::exclude("café*")]).expect("filter compiles");
    let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("converter");

    let operands = vec![source.into_os_string(), dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .with_filters(Some(filter_set))
        .with_iconv(Some(converter));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");

    let dest_root = temp.path().join("dest").join("source");
    let names = list_raw_names(&dest_root);

    // Only plain.txt should be present; café.txt was excluded.
    assert_eq!(
        names,
        vec![b"plain.txt".to_vec()],
        "excluded file should not appear at destination; got {names:?}"
    );
}

// =========================================================================
// 2. Include pattern with non-ASCII characters under --iconv
// =========================================================================

/// An `--include='résumé*'` followed by `--exclude='*'` should allow
/// only files matching the include through. With `--iconv=UTF-8,ISO-8859-1`
/// active, the surviving file's destination name should be transcoded.
#[test]
fn include_non_ascii_pattern_allows_file_under_iconv() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Source: "résumé.txt" (UTF-8) and "notes.txt"
    let resume_name = OsStr::from_bytes(b"r\xc3\xa9sum\xc3\xa9.txt");
    let notes_name = OsStr::from_bytes(b"notes.txt");
    fs::write(source.join(resume_name), b"resume data").expect("write resume");
    fs::write(source.join(notes_name), b"notes data").expect("write notes");

    let filter_set =
        FilterSet::from_rules([FilterRule::include("résumé*"), FilterRule::exclude("*")])
            .expect("filter compiles");
    let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("converter");

    let operands = vec![source.into_os_string(), dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .with_filters(Some(filter_set))
        .with_iconv(Some(converter));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");

    let dest_root = temp.path().join("dest").join("source");
    let names = list_raw_names(&dest_root);

    // Only résumé.txt should survive, transcoded to Latin-1 on disk:
    // "é" (c3 a9) -> 0xe9.
    assert_eq!(
        names,
        vec![b"r\xe9sum\xe9.txt".to_vec()],
        "only the included file should appear, with Latin-1 encoding; got {names:?}"
    );

    // Verify payload integrity.
    let copied =
        fs::read(dest_root.join(OsStr::from_bytes(b"r\xe9sum\xe9.txt"))).expect("read dest");
    assert_eq!(copied, b"resume data");
}

// =========================================================================
// 3. Filter rules in the reverse iconv direction
// =========================================================================

/// Filter evaluation still works correctly when iconv is configured in
/// the Latin-1 -> UTF-8 direction. Source files are in Latin-1, the
/// exclude pattern is expressed in the source charset (Latin-1 raw bytes
/// as an OsStr pattern match), and the destination receives UTF-8 names.
///
/// Since `FilterSet` uses glob patterns against `Path`, and Path on
/// Linux is raw bytes, the pattern must match the raw source bytes.
/// A UTF-8 pattern string "café*" will NOT match a Latin-1 source name
/// (0xe9 vs c3 a9). Instead, we use a glob pattern that matches the
/// ASCII prefix to demonstrate filter + iconv independence.
#[test]
fn filter_works_with_reverse_iconv_direction() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Source files with Latin-1 names.
    let latin1_name = OsStr::from_bytes(b"caf\xe9.txt");
    let keep_name = OsStr::from_bytes(b"keep.txt");
    fs::write(source.join(latin1_name), b"coffee").expect("write cafe");
    fs::write(source.join(keep_name), b"keeper").expect("write keep");

    // Exclude pattern uses glob prefix to match Latin-1 filename.
    let filter_set = FilterSet::from_rules([FilterRule::exclude("caf*")]).expect("filter compiles");
    let converter = FilenameConverter::new("ISO-8859-1", "UTF-8").expect("converter");

    let operands = vec![source.into_os_string(), dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .with_filters(Some(filter_set))
        .with_iconv(Some(converter));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");

    let dest_root = temp.path().join("dest").join("source");
    let names = list_raw_names(&dest_root);

    // café.txt (Latin-1) was excluded by "caf*". Only keep.txt remains.
    assert_eq!(
        names,
        vec![b"keep.txt".to_vec()],
        "excluded Latin-1 file should not appear at destination; got {names:?}"
    );
}

// =========================================================================
// 4. Non-excluded files transcoded correctly when filter is active
// =========================================================================

/// When a filter excludes some files but not others, the surviving files
/// must still have their destination names transcoded by iconv. This
/// verifies the filter and iconv pipelines do not interfere with each
/// other.
#[test]
fn surviving_files_transcoded_when_filter_active() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Three source files, all UTF-8.
    let cafe = OsStr::from_bytes(b"caf\xc3\xa9.txt");
    let uber = OsStr::from_bytes(b"\xc3\xbcber.txt"); // "über.txt"
    let skip = OsStr::from_bytes(b"skip.log");
    fs::write(source.join(cafe), b"coffee").expect("write cafe");
    fs::write(source.join(uber), b"uber").expect("write uber");
    fs::write(source.join(skip), b"logs").expect("write skip");

    // Exclude *.log files only.
    let filter_set =
        FilterSet::from_rules([FilterRule::exclude("*.log")]).expect("filter compiles");
    let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("converter");

    let operands = vec![source.into_os_string(), dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .with_filters(Some(filter_set))
        .with_iconv(Some(converter));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");

    let dest_root = temp.path().join("dest").join("source");
    let names = list_raw_names(&dest_root);

    // skip.log excluded. café.txt and über.txt survive, transcoded to Latin-1.
    // "café.txt" -> caf\xe9.txt, "über.txt" -> \xfcber.txt.
    assert_eq!(
        names,
        vec![b"caf\xe9.txt".to_vec(), b"\xfcber.txt".to_vec()],
        "non-excluded files should be transcoded to Latin-1; got {names:?}"
    );
}

// =========================================================================
// 5. Filter with non-ASCII pattern - no iconv (control test)
// =========================================================================

/// Without iconv, a non-ASCII exclude pattern still matches source files
/// correctly. This is the baseline that confirms filter matching works
/// with non-ASCII patterns independently of iconv.
#[test]
fn non_ascii_filter_without_iconv_still_matches() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let cafe_name = OsStr::from_bytes(b"caf\xc3\xa9.txt");
    let plain_name = OsStr::from_bytes(b"plain.txt");
    fs::write(source.join(cafe_name), b"excluded").expect("write cafe");
    fs::write(source.join(plain_name), b"kept").expect("write plain");

    let filter_set =
        FilterSet::from_rules([FilterRule::exclude("café*")]).expect("filter compiles");

    let operands = vec![source.into_os_string(), dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .with_filters(Some(filter_set));
    // No iconv configured.

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");

    let dest_root = temp.path().join("dest").join("source");
    let names = list_raw_names(&dest_root);

    assert_eq!(
        names,
        vec![b"plain.txt".to_vec()],
        "non-ASCII exclude should work without iconv; got {names:?}"
    );
}

// =========================================================================
// 6. Nested directory with non-ASCII exclude under iconv
// =========================================================================

/// Filters apply recursively to nested directories. An exclude pattern
/// should prevent matching files from being copied even inside
/// subdirectories, while iconv transcodes the surviving filenames.
#[test]
fn exclude_non_ascii_in_nested_directories_under_iconv() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let nested = source.join("subdir");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&nested).expect("create nested");
    fs::create_dir_all(&dest).expect("create dest");

    // Top-level: plain file that survives.
    let plain = OsStr::from_bytes(b"plain.txt");
    fs::write(source.join(plain), b"top").expect("write top");

    // Nested: one matching exclude, one surviving.
    let nested_cafe = OsStr::from_bytes(b"caf\xc3\xa9_notes.txt");
    let nested_keep = OsStr::from_bytes(b"r\xc3\xa9port.txt"); // "réport.txt"
    fs::write(nested.join(nested_cafe), b"excluded").expect("write nested cafe");
    fs::write(nested.join(nested_keep), b"kept").expect("write nested keep");

    // Exclude anything starting with "café".
    let filter_set =
        FilterSet::from_rules([FilterRule::exclude("café*")]).expect("filter compiles");
    let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("converter");

    let operands = vec![source.into_os_string(), dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .with_filters(Some(filter_set))
        .with_iconv(Some(converter));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");

    let dest_root = temp.path().join("dest").join("source");

    // Top level: plain.txt survives (ASCII, no transcoding effect).
    let top_names = list_raw_names(&dest_root);
    assert!(
        top_names.contains(&b"plain.txt".to_vec()),
        "plain.txt should survive; got {top_names:?}"
    );
    assert!(
        top_names.contains(&b"subdir".to_vec()),
        "subdir should be present; got {top_names:?}"
    );

    // Nested level: café_notes.txt excluded, réport.txt survives and is
    // transcoded to Latin-1.
    let nested_names = list_raw_names(&dest_root.join("subdir"));
    assert_eq!(
        nested_names,
        vec![b"r\xe9port.txt".to_vec()],
        "only non-excluded nested file should appear, Latin-1 transcoded; got {nested_names:?}"
    );
}

// =========================================================================
// 7. Edge case: encoding mismatch between filter pattern and source names
// =========================================================================

/// When the source filesystem uses Latin-1 names and the filter pattern
/// is expressed as a UTF-8 string, the UTF-8 pattern bytes will not
/// match the Latin-1 source bytes on Linux (where Path is raw bytes).
/// This confirms that filter patterns must be in the same encoding as
/// the source filenames for matching to work - a real-world footgun
/// that upstream rsync documents in its man page.
///
/// With `--iconv=ISO-8859-1,UTF-8` the destination receives UTF-8
/// names, but the filter operates on the pre-conversion source side.
#[test]
fn encoding_mismatch_filter_pattern_does_not_match() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Source file: Latin-1 "café.txt" (0xe9 for é).
    let latin1_name = OsStr::from_bytes(b"caf\xe9.txt");
    fs::write(source.join(latin1_name), b"payload").expect("write latin1");

    // Filter pattern: UTF-8 "café*" (c3 a9 for é). This will NOT match
    // the Latin-1 source name because the raw bytes differ.
    let filter_set =
        FilterSet::from_rules([FilterRule::exclude("café*")]).expect("filter compiles");
    let converter = FilenameConverter::new("ISO-8859-1", "UTF-8").expect("converter");

    let operands = vec![source.into_os_string(), dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .with_filters(Some(filter_set))
        .with_iconv(Some(converter));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");

    let dest_root = temp.path().join("dest").join("source");
    let names = list_raw_names(&dest_root);

    // The file was NOT excluded (pattern encoding mismatch), so it
    // appears at the destination with UTF-8 transcoding applied.
    assert_eq!(
        names,
        vec![b"caf\xc3\xa9.txt".to_vec()],
        "encoding-mismatched pattern should not exclude; file should be transcoded to UTF-8; got {names:?}"
    );
}

// =========================================================================
// 8. Multiple non-ASCII include/exclude rules combined with iconv
// =========================================================================

/// A complex filter chain with multiple non-ASCII rules and iconv active.
/// Tests that the first-match-wins semantics work correctly when both
/// include and exclude rules contain non-ASCII patterns.
#[test]
fn mixed_non_ascii_include_exclude_with_iconv() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Four source files (all UTF-8).
    let cafe = OsStr::from_bytes(b"caf\xc3\xa9.txt");
    let resume = OsStr::from_bytes(b"r\xc3\xa9sum\xc3\xa9.doc");
    let uber = OsStr::from_bytes(b"\xc3\xbcber.txt");
    let plain = OsStr::from_bytes(b"readme.txt");
    fs::write(source.join(cafe), b"1").expect("write cafe");
    fs::write(source.join(resume), b"2").expect("write resume");
    fs::write(source.join(uber), b"3").expect("write uber");
    fs::write(source.join(plain), b"4").expect("write plain");

    // Filter chain: include résumé*, exclude *.doc, exclude café*, include *
    // First match wins:
    // - résumé.doc -> matches "résumé*" (include) -> kept
    // - café.txt -> matches "café*" (exclude) -> excluded
    // - über.txt -> matches "*" (include) -> kept
    // - readme.txt -> matches "*" (include) -> kept
    let filter_set = FilterSet::from_rules([
        FilterRule::include("résumé*"),
        FilterRule::exclude("*.doc"),
        FilterRule::exclude("café*"),
        FilterRule::include("*"),
    ])
    .expect("filter compiles");
    let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("converter");

    let operands = vec![source.into_os_string(), dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .with_filters(Some(filter_set))
        .with_iconv(Some(converter));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");

    let dest_root = temp.path().join("dest").join("source");
    let names = list_raw_names(&dest_root);

    // Expected survivors (all transcoded to Latin-1):
    // readme.txt (ASCII, unchanged)
    // résumé.doc -> r\xe9sum\xe9.doc
    // über.txt -> \xfcber.txt
    // café.txt excluded.
    assert_eq!(names.len(), 3, "expected 3 files; got {names:?}");
    assert!(
        names.contains(&b"readme.txt".to_vec()),
        "readme.txt should survive"
    );
    assert!(
        names.contains(&b"r\xe9sum\xe9.doc".to_vec()),
        "résumé.doc should survive and be transcoded"
    );
    assert!(
        names.contains(&b"\xfcber.txt".to_vec()),
        "über.txt should survive and be transcoded"
    );
    assert!(
        !names.contains(&b"caf\xe9.txt".to_vec()),
        "café.txt should be excluded"
    );
}
