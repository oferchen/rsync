//! `--max-alloc` bounds peer-driven file-list growth the way upstream does.
//!
//! Upstream grows the received file list through `flist_expand()`, whose
//! `realloc_array()` of the `file_struct *` pointer array goes through
//! `my_alloc()`. Once the requested element count reaches
//! `max_alloc / sizeof(pointer)` the receiver prints
//! `[Receiver] exceeded --max-alloc=N setting (file=flist.c, line=626)` and
//! exits `RERR_MALLOC` (22). Without that bound a hostile sender can stream
//! entries until the receiver runs out of memory.
//!
//! upstream: util2.c:73-81 my_alloc(), flist.c:591-639 flist_expand(),
//! flist.c:3172-3179,3216,3239-3241 recv_file_list().
//!
//! Each test sets the process-global `--max-alloc`; nextest runs every test in
//! its own process, so the settings never leak between tests.

use std::io::{self, Cursor};

use protocol::flist::{FileEntry, FileListReader, FileListWriter};
use protocol::{CompatibilityFlags, MallocFailure, ProtocolVersion, set_max_alloc};

/// The smallest `--max-alloc` upstream accepts (options.c:2073).
const ONE_MIB: usize = 1024 * 1024;

/// Largest list upstream receives under `--max-alloc=1M`.
///
/// flist_expand() starts at FLIST_START_LARGE (32768) and doubles. The step to
/// `max_alloc / sizeof(pointer)` elements is the first one my_alloc() refuses,
/// so the last accepted capacity is half of it: 65536 entries on a 64-bit
/// build, matching the upstream 3.5.1 A/B (65537th entry exits 22).
fn largest_accepted_list() -> usize {
    ONE_MIB / size_of::<*const u8>() / 2
}

fn upstream_text(role: &str) -> String {
    format!("[{role}] exceeded --max-alloc={ONE_MIB} setting (file=flist.c, line=626)")
}

fn inc_recurse() -> CompatibilityFlags {
    CompatibilityFlags::INC_RECURSE | CompatibilityFlags::VARINT_FLIST_FLAGS
}

/// Encodes one file list (entries plus its end marker) onto `wire`.
fn encode_list(writer: &mut FileListWriter, wire: &mut Vec<u8>, entries: &[FileEntry]) {
    for entry in entries {
        writer.write_entry(wire, entry).expect("encode entry");
    }
    writer.write_end(wire, None).expect("encode end marker");
}

fn files(count: usize) -> Vec<FileEntry> {
    (0..count)
        .map(|i| FileEntry::new_file(format!("f{i:07}").into(), 0, 0o644))
        .collect()
}

fn dirs(prefix: &str, count: usize) -> Vec<FileEntry> {
    (0..count)
        .map(|i| FileEntry::new_directory(format!("{prefix}{i:07}").into(), 0o755))
        .collect()
}

/// Reads one list to its end marker, returning how many entries it held.
fn read_list(reader: &mut FileListReader, wire: &mut Cursor<Vec<u8>>) -> io::Result<usize> {
    let mut count = 0;
    while reader.read_entry(wire)?.is_some() {
        count += 1;
    }
    Ok(count)
}

/// Reads until the reader fails, returning the error and the entries decoded
/// before it.
fn read_until_refused(
    reader: &mut FileListReader,
    wire: &mut Cursor<Vec<u8>>,
) -> (io::Error, usize) {
    let mut count = 0;
    loop {
        match reader.read_entry(wire) {
            Ok(Some(_)) => count += 1,
            Ok(None) => panic!("list ended after {count} entries without the --max-alloc refusal"),
            Err(err) => return (err, count),
        }
    }
}

#[test]
fn flist_growth_past_max_alloc_is_refused_with_the_upstream_text() {
    // WHY: an unbounded flist lets a hostile sender exhaust the receiver's
    // memory. Upstream stops it at the first pointer-array growth that reaches
    // --max-alloc, on the 65537th entry at 1M, before decoding that entry.
    set_max_alloc(ONE_MIB);
    let limit = largest_accepted_list();
    let protocol = ProtocolVersion::NEWEST;
    let mut wire = Vec::new();
    encode_list(
        &mut FileListWriter::new(protocol),
        &mut wire,
        &files(limit + 1),
    );

    let mut reader = FileListReader::new(protocol);
    let (err, accepted) = read_until_refused(&mut reader, &mut Cursor::new(wire));

    assert_eq!(accepted, limit, "the refusal must land on entry limit+1");
    assert_eq!(err.to_string(), upstream_text("Receiver"));
    assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
    // The MallocFailure tag is what maps the refusal to exit 22 (RERR_MALLOC).
    assert!(
        err.get_ref()
            .is_some_and(|inner| inner.is::<MallocFailure>())
    );
}

#[test]
fn flist_at_the_max_alloc_boundary_is_accepted() {
    // WHY: positive control - the bound must not refuse a list upstream
    // accepts, or a legitimate large transfer would fail under --max-alloc.
    set_max_alloc(ONE_MIB);
    let limit = largest_accepted_list();
    let protocol = ProtocolVersion::NEWEST;
    let mut wire = Vec::new();
    encode_list(&mut FileListWriter::new(protocol), &mut wire, &files(limit));

    let mut reader = FileListReader::new(protocol);
    let received = read_list(&mut reader, &mut Cursor::new(wire)).expect("list within bound");
    assert_eq!(received, limit);
}

#[test]
fn inc_recurse_directory_list_bound_spans_sub_lists() {
    // WHY: under INC_RECURSE every sub-list gets a fresh flist, but the
    // receiver's dir_flist keeps every directory it has seen (flist.c:3239).
    // A sender must not evade the bound by splitting directories across
    // sub-lists: the cumulative directory count trips it, in the forked
    // receiver ("receiver"), even though no single list is near the limit.
    set_max_alloc(ONE_MIB);
    let limit = largest_accepted_list();
    let first = limit / 2;
    let protocol = ProtocolVersion::NEWEST;
    let mut writer = FileListWriter::with_compat_flags(protocol, inc_recurse());
    let mut wire = Vec::new();
    encode_list(&mut writer, &mut wire, &dirs("a", first));
    encode_list(&mut writer, &mut wire, &dirs("b", limit - first + 1));

    let mut reader = FileListReader::with_compat_flags(protocol, inc_recurse());
    let mut wire = Cursor::new(wire);
    assert_eq!(
        read_list(&mut reader, &mut wire).expect("initial list"),
        first
    );
    let (err, accepted) = read_until_refused(&mut reader, &mut wire);

    assert_eq!(
        accepted,
        limit - first,
        "the cumulative directory {} trips",
        limit + 1
    );
    assert_eq!(err.to_string(), upstream_text("receiver"));
}

#[test]
fn file_list_bound_resets_for_each_sub_list() {
    // WHY: upstream frees nothing it does not have to, but each sub-list is
    // its own flist (flist.c:3171-3172), so non-directory entries are bounded
    // per list. Two sub-lists that together exceed the limit are accepted.
    set_max_alloc(ONE_MIB);
    let limit = largest_accepted_list();
    let protocol = ProtocolVersion::NEWEST;
    let mut writer = FileListWriter::with_compat_flags(protocol, inc_recurse());
    let mut wire = Vec::new();
    encode_list(&mut writer, &mut wire, &files(limit));
    encode_list(&mut writer, &mut wire, &files(limit));

    let mut reader = FileListReader::with_compat_flags(protocol, inc_recurse());
    let mut wire = Cursor::new(wire);
    assert_eq!(
        read_list(&mut reader, &mut wire).expect("first list"),
        limit
    );
    assert_eq!(
        read_list(&mut reader, &mut wire).expect("second list"),
        limit
    );
}
