//! A root receiver keeps the peer's non-`user.*` xattrs.
//!
//! upstream: `xattrs.c:receive_xattr()` line 876 -
//! `if (am_root <= 0 && !HAS_PREFIX(name, USER_PREFIX))` drops (or, with an
//! xattr filter or --fake-super, disguises) every other namespace. A real
//! root receiver skips that branch, so `security.*` and `trusted.*` reach the
//! cache and `set_xattr()` writes them. Dropping them silently loses SELinux
//! labels and file capabilities on a `sudo rsync -aX` backup.

use std::io::Cursor;

use protocol::flist::{FileEntry, FileListWriter};
use protocol::xattr::{XattrEntry, XattrList};

use super::super::ReceiverContext;
use super::support::{test_config, test_handshake};

/// Sends one entry carrying `user.keep` and `trusted.oc_probe` through the
/// receiver's own flist reader and returns the names it cached.
fn received_xattr_names(fake_super: bool) -> Vec<Vec<u8>> {
    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.xattrs = true;
    config.flags.xattrs_level = 1;
    config.fake_super = fake_super;
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let mut list = XattrList::new();
    list.push(XattrEntry::new("trusted.oc_probe", b"T".to_vec()));
    list.push(XattrEntry::new("user.keep", b"U".to_vec()));
    let mut entry = FileEntry::new_file("f".into(), 1, 0o644);
    entry.set_xattr_list(list);

    let mut wire = Vec::new();
    let mut writer = FileListWriter::new(ctx.protocol()).with_preserve_xattrs(true);
    writer.write_entry(&mut wire, &entry).expect("write entry");
    writer.write_end(&mut wire, None).expect("write end");

    let mut reader = ctx.build_flist_reader();
    let mut cursor = Cursor::new(wire);
    reader
        .read_entry_with_flist(&mut cursor, &[])
        .expect("decode entry")
        .expect("one entry");
    let cached = reader.xattr_cache().get(0).expect("cached xattr set");
    cached.iter().map(|x| x.name().to_vec()).collect()
}

/// Root keeps the trusted.* name verbatim. Skips when not running as root,
/// because the non-root arm is upstream's documented drop.
#[test]
fn root_receiver_keeps_non_user_xattr_names() {
    if !metadata::am_root() {
        eprintln!("not root, skipping root receiver xattr namespace test");
        return;
    }
    let names = received_xattr_names(false);
    assert!(
        names.iter().any(|n| n == b"trusted.oc_probe"),
        "root receiver must keep trusted.* (xattrs.c:876): {names:?}"
    );
    assert!(names.iter().any(|n| n == b"user.keep"), "{names:?}");
}

/// Non-root, and root under --fake-super (`am_root = -1`), stay on upstream's
/// `am_root <= 0` side: without an xattr filter the trusted.* name is dropped.
#[test]
fn non_root_or_fake_super_receiver_drops_non_user_xattr_names() {
    let names = received_xattr_names(true);
    assert!(
        !names.iter().any(|n| n == b"trusted.oc_probe"),
        "am_root <= 0 must drop trusted.*: {names:?}"
    );
    assert!(names.iter().any(|n| n == b"user.keep"), "{names:?}");
}
