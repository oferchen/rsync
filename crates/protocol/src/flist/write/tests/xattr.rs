use super::*;

#[test]
fn xattr_write_entry_sends_literal_data() {
    use crate::xattr::{XattrEntry, XattrList};

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(test_protocol()).with_preserve_xattrs(true);

    let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    let mut xattr_list = XattrList::new();
    xattr_list.push(XattrEntry::new("test_key", b"test_value".to_vec()));
    entry.set_xattr_list(xattr_list);

    writer.write_entry(&mut buf, &entry).unwrap();
    assert!(!buf.is_empty());
}

#[test]
fn xattr_write_empty_list_succeeds() {
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(test_protocol()).with_preserve_xattrs(true);

    // Entry without an xattr_list still emits an empty literal set on the wire.
    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    writer.write_entry(&mut buf, &entry).unwrap();
    assert!(!buf.is_empty());
}

#[test]
fn xattr_cache_deduplicates_identical_sets() {
    use crate::xattr::{XattrEntry, XattrList};

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(test_protocol()).with_preserve_xattrs(true);

    let make_list = || {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("key", b"value".to_vec()));
        list
    };

    // First entry emits literal xattr data; second entry should hit the cache
    // and encode just a varint index, producing strictly fewer wire bytes.
    let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
    entry1.set_xattr_list(make_list());
    writer.write_entry(&mut buf, &entry1).unwrap();
    let first_len = buf.len();

    let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
    entry2.set_xattr_list(make_list());
    writer.write_entry(&mut buf, &entry2).unwrap();
    let second_len = buf.len() - first_len;

    assert!(
        second_len < first_len,
        "cache hit should be smaller: {second_len} vs {first_len}",
    );
}

#[test]
fn xattr_write_roundtrip_with_reader() {
    use crate::xattr::{XattrEntry, XattrList};

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(test_protocol()).with_preserve_xattrs(true);

    let mut entry = FileEntry::new_file("roundtrip.txt".into(), 42, 0o644);
    let mut xattr_list = XattrList::new();
    // Use the verbatim wire name `user.my_attr` so the round-trip lands a
    // visible entry in the reader's cache on every platform: Linux keeps
    // `user.*` verbatim, non-Linux strips the prefix to `my_attr`. Without
    // the `user.` prefix the non-Linux receiver drops the entry as a
    // non-storable disguised namespace (see xattr::prefix::wire_to_local
    // and upstream xattrs.c:836-846).
    xattr_list.push(XattrEntry::new("user.my_attr", b"my_value".to_vec()));
    entry.set_xattr_list(xattr_list);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = std::io::Cursor::new(&buf);
    let mut reader =
        super::super::super::read::FileListReader::new(test_protocol()).with_preserve_xattrs(true);
    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read_entry.name(), "roundtrip.txt");
    assert!(
        read_entry.xattr_ndx().is_some(),
        "xattr_ndx should be set after reading"
    );
    let xattr_cache = reader.xattr_cache();
    let cached = xattr_cache.get(read_entry.xattr_ndx().unwrap() as usize);
    assert!(cached.is_some(), "xattr cache should have entry");
    let list = cached.unwrap();
    assert_eq!(list.len(), 1);
}

/// When the negotiated peer capabilities lack xattr support, the file list
/// writer must NOT emit xattr wire bytes - even if the local file entry
/// happens to carry an attached xattr list. The gating boolean
/// `preserve.xattrs` mirrors the negotiated state derived from
/// `CompatibilityFlags::AVOID_XATTR_OPTIMIZATION`. Suppression must be
/// silent: no error, no partial emission.
///
/// upstream: flist.c:send_file_entry() line 656 - `send_xattr()` is only
/// invoked when `preserve_xattrs` is set in the receiver's option block.
#[test]
fn xattr_emission_suppressed_when_peer_lacks_xattr_capability() {
    use crate::xattr::{XattrEntry, XattrList};

    // Simulate the post-negotiation state where the remote peer did not
    // advertise CF_AVOID_XATTR_OPTIM, causing the local options layer to
    // clear the xattrs preserve flag. The default for `PreserveFlags` is
    // false, but we construct the writer explicitly to document intent.
    let mut writer_no_xattr = FileListWriter::new(test_protocol()).with_preserve_xattrs(false);

    let mut entry_with_xattr = FileEntry::new_file("attrs_attached.txt".into(), 100, 0o644);
    entry_with_xattr.set_mtime(1700000000, 0);
    let mut xattr_list = XattrList::new();
    xattr_list.push(XattrEntry::new("user.tag", b"local-only".to_vec()));
    xattr_list.push(XattrEntry::new(
        "security.selinux",
        b"system_u:object_r:default_t:s0".to_vec(),
    ));
    entry_with_xattr.set_xattr_list(xattr_list);

    let mut buf_suppressed = Vec::new();
    writer_no_xattr
        .write_entry(&mut buf_suppressed, &entry_with_xattr)
        .expect("write must succeed even when xattr emission is suppressed");

    // Baseline: same writer config, identical entry but with no xattr list
    // attached. Wire bytes must match exactly because xattrs are gated off.
    let mut writer_baseline = FileListWriter::new(test_protocol()).with_preserve_xattrs(false);
    let mut entry_no_xattr = FileEntry::new_file("attrs_attached.txt".into(), 100, 0o644);
    entry_no_xattr.set_mtime(1700000000, 0);

    let mut buf_baseline = Vec::new();
    writer_baseline
        .write_entry(&mut buf_baseline, &entry_no_xattr)
        .expect("baseline write must succeed");

    assert_eq!(
        buf_suppressed, buf_baseline,
        "writer with peer xattr capability OFF must emit identical bytes \
         regardless of attached xattr_list - no xattr wire data leaks",
    );

    // Cross-check: enabling preservation produces strictly more bytes
    // (literal xattr block appended), proving the suppression in the
    // baseline is meaningful and not a no-op of the encoder.
    let mut writer_enabled = FileListWriter::new(test_protocol()).with_preserve_xattrs(true);
    let mut buf_enabled = Vec::new();
    writer_enabled
        .write_entry(&mut buf_enabled, &entry_with_xattr)
        .expect("enabled write must succeed");
    assert!(
        buf_enabled.len() > buf_suppressed.len(),
        "enabling xattr preservation must add wire bytes \
         (enabled={}, suppressed={})",
        buf_enabled.len(),
        buf_suppressed.len(),
    );
}
