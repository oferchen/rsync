//! The sender transmits its file list in upstream's send order.
//!
//! upstream: `flist.c:send_directory()` writes each child as readdir returns
//! it (flist.c:2397-2421) and recurses into the child directories only after
//! the scan (flist.c:2434-2439). `send_file_list()` sorts the list only once it
//! is on the wire (flist.c:3059), and each INC_RECURSE sub-list likewise
//! (flist.c:2702). The wire order is therefore readdir order, not sorted
//! order, and every byte after a reordered entry depends on it: the
//! name-prefix compression against the previous name, and the number a
//! hard-link follower names its leader by.
//!
//! Each cell pulls one tree with a 3.5.1 client, once from a 3.5.1 sender and
//! once from an oc sender, taps the sender's stdout, and requires the
//! file-list bytes to match exactly. The destination is populated first, so
//! no file data follows the lists.

#![cfg(target_os = "linux")]

use std::fs;
use std::io::{self, Cursor, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use filetime::{FileTime, set_symlink_file_times};
use protocol::codec::{NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, create_ndx_codec};
use protocol::flist::{FileEntry, FileListReader, compare_file_entries};
use protocol::{CompatibilityFlags, ProtocolVersion, read_varint};
use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// Fixed mtime for every fixture entry, so the lists carry identical times.
const FIXED_MTIME_SECS: i64 = 1_700_000_000;

/// upstream: io.h `MPLEX_BASE` - a frame header's tag byte is `MPLEX_BASE +
/// msg code`, and `MSG_DATA` is code 0.
const MPLEX_BASE: u32 = 7;

/// Builds a tree whose readdir order differs from its sorted order.
///
/// The names are created out of order, and the hard-linked pair `hz`/`ha` is
/// created leader-last in sorted terms, so a sender that sorts before it
/// writes picks a different leader than one that writes in readdir order.
fn build_tree(src: &Path) {
    for dir in ["zz", "aa"] {
        fs::create_dir_all(src.join(dir)).expect("create dir");
    }
    for name in ["m", "c", "x", "a", "q", "b"] {
        fs::write(src.join(name), format!("{name}-data")).expect("write file");
    }
    for name in ["k5", "k1", "k9", "k3", "k7"] {
        fs::write(src.join("zz").join(name), name).expect("write nested file");
    }
    fs::write(src.join("aa/f"), "aa").expect("write nested file");
    fs::write(src.join("hz"), "linked").expect("write link leader");
    fs::hard_link(src.join("hz"), src.join("ha")).expect("hard link");
    pin_mtimes(src);
}

fn pin_mtimes(dir: &Path) {
    let mtime = FileTime::from_unix_time(FIXED_MTIME_SECS, 0);
    for entry in fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            pin_mtimes(&path);
        }
        set_symlink_file_times(&path, mtime, mtime).expect("pin mtime");
    }
    set_symlink_file_times(dir, mtime, mtime).expect("pin dir mtime");
}

/// Pulls `root/src/` into `dest` with `client`, the sender being `server`, and
/// returns the bytes the sender wrote when `tap` is set.
///
/// `lazy` turns on oc's lazy INC_RECURSE producer in the sender; upstream
/// ignores the variable.
fn pull(
    client: &Path,
    server: &Path,
    root: &Path,
    dest: &str,
    args: &[&str],
    tap: bool,
    lazy: bool,
) -> Vec<u8> {
    let rsh = root.join("rsh");
    let tap_file = root.join(format!("{dest}.tap"));
    fs::write(&rsh, "#!/bin/sh\nshift\n\"$@\" | tee \"$TAP\"\n").expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    let output = Command::new(client)
        .current_dir(root)
        .env(
            "TAP",
            if tap {
                tap_file.as_path()
            } else {
                Path::new("/dev/null")
            },
        )
        .env("OC_RSYNC_LAZY_FLIST", if lazy { "1" } else { "0" })
        .args(["-rlpt", "--checksum-seed=12345", "-e"])
        .arg(&rsh)
        .arg(format!("--rsync-path={}", server.display()))
        .args(args)
        .arg(format!("localhost:{}/src/", root.display()))
        .arg(format!("{dest}/"))
        .output()
        .expect("run pull");
    assert!(
        output.status.success(),
        "pull from {} failed: {}",
        server.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    if tap {
        fs::read(&tap_file).expect("read tap")
    } else {
        Vec::new()
    }
}

/// The sender's stream split at the start of multiplexing.
struct SenderStream {
    /// The raw setup bytes: version, compat flags, checksum list and seed.
    preamble: Vec<u8>,
    compat: CompatibilityFlags,
    /// The `MSG_DATA` payload, demultiplexed.
    data: Vec<u8>,
}

fn split(stream: &[u8]) -> SenderStream {
    let mut cur = Cursor::new(stream);
    let mut version = [0u8; 4];
    cur.read_exact(&mut version).expect("protocol version");
    assert_eq!(u32::from_le_bytes(version), 33, "both senders speak 33");
    let compat = CompatibilityFlags::from_bits(read_varint(&mut cur).expect("compat") as u32);
    let mut len = [0u8; 1];
    cur.read_exact(&mut len).expect("checksum list length");
    let mut list_len = usize::from(len[0]);
    if list_len & 0x80 != 0 {
        cur.read_exact(&mut len).expect("checksum list length");
        list_len = ((list_len & 0x7f) << 8) | usize::from(len[0]);
    }
    cur.set_position(cur.position() + list_len as u64 + 4);
    let start = cur.position() as usize;

    let mut data = Vec::new();
    let mut pos = start;
    while pos + 4 <= stream.len() {
        let header = u32::from_le_bytes(stream[pos..pos + 4].try_into().expect("header"));
        let len = (header & 0x00ff_ffff) as usize;
        let payload = &stream[pos + 4..(pos + 4 + len).min(stream.len())];
        if header >> 24 == MPLEX_BASE {
            data.extend_from_slice(payload);
        }
        pos += 4 + len;
    }
    SenderStream {
        preamble: stream[..start].to_vec(),
        compat,
        data,
    }
}

/// Reads one file list up to its end marker.
fn read_list(
    reader: &mut FileListReader,
    cur: &mut Cursor<&[u8]>,
    ndx_start: i32,
) -> io::Result<Vec<FileEntry>> {
    reader.reset_for_new_segment(ndx_start);
    let mut entries = Vec::new();
    while let Some(entry) = reader.read_entry_with_flist(cur, &entries)? {
        entries.push(entry);
    }
    Ok(entries)
}

/// Decodes the file lists at the head of `data`, returning their byte length
/// and the lists in the order they were sent.
fn file_lists(stream: &SenderStream, inc_recurse: bool) -> (usize, Vec<Vec<FileEntry>>) {
    let protocol = ProtocolVersion::try_from(33u8).expect("protocol 33");
    let mut reader = FileListReader::with_compat_flags(protocol, stream.compat)
        .with_preserve_links(true)
        .with_preserve_hard_links(true);
    let mut cur = Cursor::new(stream.data.as_slice());
    // upstream: flist.c:2777 - an incremental list numbers from 1.
    let mut ndx_start = i32::from(inc_recurse);
    let mut lists = vec![read_list(&mut reader, &mut cur, ndx_start).expect("initial list")];
    if inc_recurse {
        let mut codec = create_ndx_codec(33);
        loop {
            let ndx = codec.read_ndx(&mut cur).expect("ndx");
            if ndx == NDX_FLIST_EOF {
                break;
            }
            assert!(ndx <= NDX_FLIST_OFFSET, "a sub-list header, got NDX {ndx}");
            // upstream: flist.c:3511 - each sub-list opens one past the last.
            ndx_start += lists.last().map_or(0, Vec::len) as i32 + 1;
            lists.push(read_list(&mut reader, &mut cur, ndx_start).expect("sub-list"));
        }
    }
    (cur.position() as usize, lists)
}

fn names(list: &[FileEntry]) -> Vec<String> {
    list.iter().map(|e| e.name().to_owned()).collect()
}

fn assert_send_order_matches_upstream(args: &[&str], inc_recurse: bool, lazy: bool) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let temp = TempDir::new().expect("tempdir");
    build_tree(&temp.path().join("src"));
    fs::write(temp.path().join("files"), "m\nzz/k1\nc\naa/f\n").expect("write files-from list");
    let capture = |server: &Path, dest: &str| {
        let client = upstream.binary();
        pull(client, client, temp.path(), dest, args, false, false);
        split(&pull(client, server, temp.path(), dest, args, true, lazy))
    };
    let up = capture(upstream.binary(), "dst-up");
    let oc = capture(&oc_rsync_bin(), "dst-oc");

    let (up_len, up_lists) = file_lists(&up, inc_recurse);
    let (oc_len, oc_lists) = file_lists(&oc, inc_recurse);

    // Every kind of list the cell checks must be able to tell the orders
    // apart, or it proves nothing about a sender that sorts first.
    let unsorted = |list: &Vec<FileEntry>| {
        let mut sorted = list.clone();
        sorted.sort_by(compare_file_entries);
        names(&sorted) != names(list)
    };
    assert!(
        unsorted(&up_lists[0]),
        "readdir order equals sorted order on this filesystem"
    );
    assert!(
        !inc_recurse || up_lists[1..].iter().any(unsorted),
        "no sub-list differs from its sorted order on this filesystem"
    );

    for (up_list, oc_list) in up_lists.iter().zip(&oc_lists) {
        assert_eq!(
            names(oc_list),
            names(up_list),
            "send order of one file list"
        );
    }
    assert_eq!(oc_lists.len(), up_lists.len(), "number of file lists");
    assert_eq!(oc.preamble, up.preamble, "setup bytes");
    assert_eq!(
        oc.data[..oc_len],
        up.data[..up_len],
        "file-list bytes from an oc sender and a 3.5.1 sender"
    );
}

#[test]
fn full_file_list_goes_out_in_upstream_send_order() {
    assert_send_order_matches_upstream(&["-H", "--no-inc-recursive"], false, false);
}

#[test]
fn incremental_sub_lists_go_out_in_readdir_order() {
    assert_send_order_matches_upstream(&["-H"], true, false);
}

/// A `--files-from` sender sends each name's implied parents just before the
/// name (flist.c:2983-2986), not every name's parents ahead of the first name.
#[test]
fn files_from_names_follow_their_own_implied_parents() {
    assert_send_order_matches_upstream(&["--files-from=files", "--no-inc-recursive"], false, false);
}

/// The lazy producer scans each directory when its sub-list is due; `-H`
/// would make it fall back to the eager whole-tree walk.
#[test]
fn lazily_produced_sub_lists_go_out_in_readdir_order() {
    assert_send_order_matches_upstream(&[], true, true);
}
