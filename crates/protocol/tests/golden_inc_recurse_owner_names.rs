//! Golden bytes: inline owner names under INC_RECURSE cross the wire once per id.
//!
//! The file list below is what rsync 3.5.1 sends (captured from a
//! `--write-batch` push, protocol 33, compat flags 0x1ff) for `src/{a,b,c}/f`,
//! every entry owned by uid 1000 / gid 1000 ("ofer") except `b/f`, whose group
//! is gid 998 ("wheel"). The top level is one list, then one sub-list per
//! directory. `c/f` switches the group back to gid 1000 without naming it,
//! because the top-level list already did: upstream's `add_gid()` returns a
//! name only on an id's first sighting (uidlist.c:357-371, flist.c:795-800).
//!
//! Decoding drops the repeated names, so the test re-attaches a name to every
//! entry, as a sender's lookup does, before re-encoding with one writer.

use std::io::Cursor;

use protocol::acl::{IdAccess, IdaEntries, NO_ENTRY, RsyncAcl};
use protocol::flist::{FileEntry, FileListReader, FileListWriter};
use protocol::{CompatibilityFlags, ProtocolVersion};

/// 3.5.1's file list: the initial list, three sub-lists, `NDX_FLIST_EOF`.
const UPSTREAM_FLIST: &str = concat!(
    "8c01012e0000104bf02c3ded41000083e8046f66657283e8046f666572",
    "809a0163000010809a0161000010809a01620000100000",
    "ff65",
    "809803612f66000200a48100000000",
    "ff01",
    "888a03622f6600020083e605776865656c0000",
    "ff01",
    "808a03632f6600020083e80000",
    "fffe8002",
);

/// The NDX markers that open each sub-list and end the lists. They come from
/// the NDX codec, not the file-list writer, so the test carries them over.
const MARKERS: [&str; 4] = ["ff65", "ff01", "ff01", "fffe8002"];

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn protocol() -> (ProtocolVersion, CompatibilityFlags) {
    (
        ProtocolVersion::try_from(33u8).unwrap(),
        CompatibilityFlags::from_bits(0x1ff),
    )
}

/// Splits the captured stream into its four lists.
fn lists(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut rest = bytes;
    let mut out = Vec::new();
    for marker in MARKERS.map(hex) {
        let at = rest
            .windows(marker.len())
            .position(|w| w == marker.as_slice())
            .expect("marker present");
        out.push(rest[..at].to_vec());
        rest = &rest[at + marker.len()..];
    }
    assert!(rest.is_empty());
    out
}

fn decode(lists: &[Vec<u8>]) -> Vec<Vec<FileEntry>> {
    let (proto, compat) = protocol();
    let mut reader = FileListReader::with_compat_flags(proto, compat)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_preserve_links(true)
        .with_preserve_devices(true)
        .with_preserve_specials(true);
    lists
        .iter()
        .map(|list| {
            let mut cursor = Cursor::new(list.as_slice());
            let mut entries = Vec::new();
            while let Some(entry) = reader.read_entry(&mut cursor).unwrap() {
                entries.push(entry);
            }
            assert_eq!(
                cursor.position() as usize,
                list.len(),
                "list fully consumed"
            );
            entries
        })
        .collect()
}

fn named(mut entry: FileEntry) -> FileEntry {
    entry.set_user_name("ofer".to_string());
    let group = match entry.gid() {
        Some(998) => "wheel",
        _ => "ofer",
    };
    entry.set_group_name(group.to_string());
    entry
}

#[test]
fn inc_recurse_owner_names_match_upstream_3_5_1() {
    let upstream = hex(UPSTREAM_FLIST);
    let decoded = decode(&lists(&upstream));
    assert_eq!(
        decoded.iter().map(Vec::len).collect::<Vec<_>>(),
        [4, 1, 1, 1]
    );

    let (proto, compat) = protocol();
    let mut writer = FileListWriter::with_compat_flags(proto, compat)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_preserve_links(true)
        .with_preserve_devices(true)
        .with_preserve_specials(true)
        .with_name_follows(true);
    let mut encoded = Vec::new();
    for (entries, marker) in decoded.into_iter().zip(MARKERS) {
        for entry in entries {
            writer.write_entry(&mut encoded, &named(entry)).unwrap();
        }
        writer.write_end(&mut encoded, None).unwrap();
        encoded.extend(hex(marker));
    }
    assert_eq!(hex_of(&encoded), hex_of(&upstream));
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 3.5.1's `-aA` file list for the same tree, with `a/f`, `b/f` and `c/f`
/// each carrying a different access ACL that names user `daemon` (uid 2),
/// user `ofer` (uid 1000) and group `wheel` (gid 998). Each ACL is literal,
/// yet `daemon` and `wheel` are named only in `a/f`'s, and `ofer` never in an
/// ACL: the owner names already sent it. upstream: acls.c:821-826
/// send_ida_entries() takes its names from the same add_uid()/add_gid() list.
const UPSTREAM_ACL_FLIST: &str = concat!(
    "8c01012e0000104bf02c3ded41000083e8046f66657283e8046f66657200000000",
    "809a01630000100101809a01610000100101809a016200001001010000",
    "ff65",
    "809803612f66000200a48100000010030213066461656d6f6e83e81283e61105776865656c0000",
    "ff01",
    "809803622f66000200b481000000120403021a83e81283e6180000",
    "ff01",
    "809803632f66000200bc81000000120403021e83e81283e61c0000",
    "fffe8002",
);

fn id_name(id: u32, user: bool) -> &'static str {
    match (id, user) {
        (2, true) => "daemon",
        (998, false) => "wheel",
        _ => "ofer",
    }
}

/// Rebuilds the ACL as the sender read it from disk: upstream's
/// rsync_acl_strip_perms() (acls.c:142-158) dropped the mask that equals the
/// mode's group bits, and the writer strips it again.
fn named_acl(acl: &RsyncAcl, mode: u32) -> RsyncAcl {
    let mut acl = acl.clone();
    if acl.mask_obj == NO_ENTRY && !acl.names.is_empty() {
        acl.mask_obj = ((mode >> 3) & 7) as u8;
    }
    let mut names = IdaEntries::new();
    for ida in acl.names.iter() {
        let name = id_name(ida.id, ida.is_user()).as_bytes().to_vec();
        names.push(IdAccess {
            name: Some(name),
            ..ida.clone()
        });
    }
    acl.names = names;
    acl
}

#[test]
fn inc_recurse_acl_names_share_the_owner_sightings_like_upstream_3_5_1() {
    let upstream = hex(UPSTREAM_ACL_FLIST);
    let (proto, compat) = protocol();
    let mut reader = FileListReader::with_compat_flags(proto, compat)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_preserve_links(true)
        .with_preserve_devices(true)
        .with_preserve_specials(true)
        .with_preserve_acls(true);
    let mut decoded = Vec::new();
    for list in lists(&upstream) {
        let mut cursor = Cursor::new(list.as_slice());
        let mut entries = Vec::new();
        while let Some(entry) = reader.read_entry(&mut cursor).unwrap() {
            let cache = reader.acl_cache();
            let access = entry
                .acl_ndx()
                .and_then(|i| cache.get_access(i))
                .map(|acl| named_acl(acl, entry.mode()));
            let default = entry
                .def_acl_ndx()
                .and_then(|i| cache.get_default(i))
                .map(|acl| named_acl(acl, entry.mode()));
            entries.push((entry, access, default));
        }
        assert_eq!(
            cursor.position() as usize,
            list.len(),
            "list fully consumed"
        );
        decoded.push(entries);
    }

    let mut writer = FileListWriter::with_compat_flags(proto, compat)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_preserve_links(true)
        .with_preserve_devices(true)
        .with_preserve_specials(true)
        .with_preserve_acls(true)
        .with_acl_send_names(true)
        .with_name_follows(true);
    let mut encoded = Vec::new();
    for (entries, marker) in decoded.into_iter().zip(MARKERS) {
        for (entry, access, default) in entries {
            if let Some(access) = access {
                writer.set_pending_acl(access, default);
            }
            writer.write_entry(&mut encoded, &named(entry)).unwrap();
        }
        writer.write_end(&mut encoded, None).unwrap();
        encoded.extend(hex(marker));
    }
    assert_eq!(hex_of(&encoded), hex_of(&upstream));
}
