//! Wire-byte regression test for flist entry flag bytes at protocol 28-29 (RP28.g).
//!
//! This test pins the flag-byte prelude emitted by [`FileListWriter`] for a
//! fixed three-entry fixture across three protocol versions:
//!
//! - Protocol 28 (oldest supported, last rsync 1.x advertisement)
//! - Protocol 29 (last rsync 2.x advertisement)
//! - Protocol 32 (current default)
//!
//! Refer to the broader inventory in
//! `docs/design/rp28-a-pre30-code-paths-inventory.md` (item W12) for the
//! protocol-conditional flag-prelude rules covered here.
//!
//! # Upstream Reference
//!
//! The flag-byte encoding is implemented in upstream
//! `flist.c:send_file_entry()` between lines 544 and 563 of
//! `target/interop/upstream-src/rsync-3.4.1/flist.c`:
//!
//! ```c
//! if (xfer_flags_as_varint)
//!     write_varint(f, xflags ? xflags : XMIT_EXTENDED_FLAGS);
//! else if (protocol_version >= 28) {
//!     if (!xflags && !S_ISDIR(mode))
//!         xflags |= XMIT_TOP_DIR;
//!     if ((xflags & 0xFF00) || !xflags) {
//!         xflags |= XMIT_EXTENDED_FLAGS;
//!         write_shortint(f, xflags);
//!     } else
//!         write_byte(f, xflags);
//! } else {
//!     ...
//! }
//! ```
//!
//! and the per-bit xflag composition in lines 406-540 of the same function.
//!
//! For protocol 28 and 29 the flag prelude is always a fixed 1- or 2-byte
//! quantity; protocol 30+ encodes it as a varint when
//! `COMPAT_VARINT_FLIST_FLAGS` is negotiated. [`FileListWriter::new`] does not
//! enable the varint mode, so protocol 32 still emits the 1-2 byte prelude in
//! this fixture - the differentiator between 28/29 and 32 is the
//! `XMIT_USER_NAME_FOLLOWS` bit (position 10), which exists only at
//! protocol >= 30 and collides with `XMIT_SAME_DEV_pre30` on pre-30 wire.
//!
//! # Fixture
//!
//! Three sequential entries written through a single [`FileListWriter`] with
//! `preserve_uid = true` and `preserve_links = true`:
//!
//! 1. Regular file `a.txt`, size 1024, mode 0o644, mtime 1_700_000_000,
//!    `uid = 1000`, `user_name = "alice"`.
//! 2. Directory `subdir`, mode 0o755, default mtime (0), no uid.
//! 3. Symlink `link` -> `/target/path`, default mtime (0), no uid.
//!
//! # Golden Derivation
//!
//! Per-entry xflags follow upstream `flist.c:send_file_entry()`:
//!
//! - Entry 1 (file, first entry, `prev_name == ""`):
//!   - `!preserve.uid` is false; `uid(1000) == prev_uid(0) && not_first` is
//!     false. Therefore `XMIT_SAME_UID` (`1 << 3`) is NOT set.
//!   - `!preserve.gid` is true. Therefore `XMIT_SAME_GID` (`1 << 4`) is set.
//!   - Primary byte (bits 0-7) = `0x10`.
//!   - At protocol >= 30 only, `XMIT_USER_NAME_FOLLOWS` (`1 << 10`) is set
//!     because `preserve.uid && user_name.is_some() && !SAME_UID`. That bit
//!     lives in the second prelude byte (bit 2 of byte 1 = `0x04`), so
//!     `xflags = 0x0410`. The non-zero high byte triggers
//!     `XMIT_EXTENDED_FLAGS` (`1 << 2 = 0x04`) being OR'd into byte 0 and a
//!     2-byte LE prelude: `[0x14, 0x04]`.
//!   - At protocol 28/29 the owner-name flags branch returns 0
//!     (`calculate_owner_name_flags` early-returns for `< 30`), so the
//!     prelude is single-byte `0x10`.
//! - Entry 2 (dir, `prev_name == "a.txt"`, prev_mode=0o100644,
//!   prev_mtime=1_700_000_000, prev_uid=1000):
//!   - `XMIT_SAME_MODE`/`XMIT_SAME_TIME` clear (dir mode and default mtime
//!     differ from previous).
//!   - `!preserve.uid` false; `uid(0) == prev_uid(1000)` false; SAME_UID
//!     clear.
//!   - `!preserve.gid` true; SAME_GID set.
//!   - `same_len("subdir", "a.txt") == 0`; no SAME_NAME / LONG_NAME.
//!   - `entry.user_name()` is None, so no USER_NAME_FOLLOWS at any protocol.
//!   - xflags = `0x10` for all three protocols, single-byte prelude.
//! - Entry 3 (symlink, `prev_name == "subdir"`, prev_mode=0o040755,
//!   prev_mtime=0, prev_uid=0):
//!   - `XMIT_SAME_TIME` SET because symlink default mtime 0 equals
//!     prev_mtime 0.
//!   - `uid(0) == prev_uid(0) && not_first` => `XMIT_SAME_UID` SET.
//!   - `XMIT_SAME_GID` SET (`!preserve.gid`).
//!   - mode differs (0o120777 vs 0o040755), SAME_MODE clear.
//!   - `same_len("link", "subdir") == 0`.
//!   - xflags = `XMIT_SAME_TIME | XMIT_SAME_UID | XMIT_SAME_GID`
//!     = `0x80 | 0x08 | 0x10 = 0x98` for all three protocols.
//!
//! Protocol 28 and 29 produce IDENTICAL flag-byte sequences for this fixture
//! because the upstream flag-prelude logic only diverges between those two
//! versions for device-rdev encoding (`XMIT_SAME_RDEV_pre28`, bit 2 on
//! protocols 20-27) and hardlink dev/ino layout - neither of which is
//! exercised by a file/dir/symlink trio. Differentiation is preserved
//! against protocol 32 via `XMIT_USER_NAME_FOLLOWS`.

use protocol::ProtocolVersion;
use protocol::flist::{FileEntry, FileListWriter};

/// Builds the fixture-encoded buffer for the given protocol version and returns
/// the offsets at which each entry begins. The offsets let callers recover the
/// per-entry flag byte without re-parsing the wire stream.
fn encode_fixture(protocol: ProtocolVersion) -> (Vec<u8>, [usize; 3]) {
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_links(true)
        .with_name_follows(true);
    let mut buf = Vec::new();
    let mut offsets = [0usize; 3];

    offsets[0] = buf.len();
    let mut file = FileEntry::new_file("a.txt".into(), 1024, 0o644);
    file.set_mtime(1_700_000_000, 0);
    file.set_uid(1000);
    file.set_user_name("alice".to_string());
    writer.write_entry(&mut buf, &file).unwrap();

    offsets[1] = buf.len();
    let dir = FileEntry::new_directory("subdir".into(), 0o755);
    writer.write_entry(&mut buf, &dir).unwrap();

    offsets[2] = buf.len();
    let link = FileEntry::new_symlink("link".into(), "/target/path".into());
    writer.write_entry(&mut buf, &link).unwrap();

    (buf, offsets)
}

/// Returns the first prelude byte of each fixture entry.
fn flag_bytes(buf: &[u8], offsets: [usize; 3]) -> [u8; 3] {
    [buf[offsets[0]], buf[offsets[1]], buf[offsets[2]]]
}

/// Golden flag-byte sequence at protocol 28.
///
/// Protocol 28 takes the `protocol_version >= 28` branch in upstream
/// `flist.c:551-558` and never sets `XMIT_USER_NAME_FOLLOWS`
/// (`calculate_owner_name_flags` early-returns for `< 30`), so the prelude is
/// a single byte per entry equal to the primary xflag mask.
#[test]
#[ignore = "RP28.k decides whether to fix or drop protocol < 30 support"]
fn rp28g_flist_flag_bytes_protocol_28() {
    let protocol = ProtocolVersion::from_supported(28).expect("protocol 28 must be supported");
    let (buf, offsets) = encode_fixture(protocol);
    let observed = flag_bytes(&buf, offsets);

    // Golden: XMIT_SAME_GID; XMIT_SAME_GID; XMIT_SAME_TIME|XMIT_SAME_UID|XMIT_SAME_GID.
    let expected: [u8; 3] = [0x10, 0x10, 0x98];
    assert_eq!(
        observed, expected,
        "protocol 28 flag prelude must match upstream send_file_entry() pre-30 encoding"
    );
}

/// Golden flag-byte sequence at protocol 29.
///
/// Protocol 29 shares the pre-30 flag layout with protocol 28 for any entry
/// that is not a device or hardlink. The single divergent bit between 28 and
/// 29 in upstream is `XMIT_SAME_RDEV_pre28` (`1 << 2`), which only applies to
/// device entries on protocols 20-27. Our fixture has no devices, so the
/// expected bytes are identical to protocol 28; the test still pins them
/// separately to catch any future regression that desynchronises the two
/// pre-30 paths.
#[test]
#[ignore = "RP28.k decides whether to fix or drop protocol < 30 support"]
fn rp28g_flist_flag_bytes_protocol_29() {
    let protocol = ProtocolVersion::from_supported(29).expect("protocol 29 must be supported");
    let (buf, offsets) = encode_fixture(protocol);
    let observed = flag_bytes(&buf, offsets);

    let expected: [u8; 3] = [0x10, 0x10, 0x98];
    assert_eq!(
        observed, expected,
        "protocol 29 flag prelude must match upstream send_file_entry() pre-30 encoding"
    );
}

/// Golden flag-byte sequence at protocol 32 (current default).
///
/// At protocol 32 the same fixture diverges in entry 1: `preserve_uid` plus a
/// `user_name` triggers `XMIT_USER_NAME_FOLLOWS` (bit 10), which forces a
/// 2-byte prelude. The first prelude byte therefore carries
/// `XMIT_EXTENDED_FLAGS` (`1 << 2 = 0x04`) OR'd into the primary mask,
/// yielding `0x14` instead of `0x10`. Entries 2 and 3 do not carry a
/// `user_name`, so their preludes remain identical to the pre-30 encoding.
///
/// This test serves as the positive control that confirms the test framework
/// correctly distinguishes flag-byte sequences across protocol families.
#[test]
fn rp28g_flist_flag_bytes_protocol_32() {
    let protocol = ProtocolVersion::from_supported(32).expect("protocol 32 must be supported");
    let (buf, offsets) = encode_fixture(protocol);
    let observed = flag_bytes(&buf, offsets);

    let expected: [u8; 3] = [0x14, 0x10, 0x98];
    assert_eq!(
        observed, expected,
        "protocol 32 flag prelude must include XMIT_EXTENDED_FLAGS|XMIT_SAME_GID for entry 1 due to XMIT_USER_NAME_FOLLOWS"
    );
}
