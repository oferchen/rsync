#![deny(unsafe_code)]

//! Itemized change formatting for `--itemize-changes` (`%i`).
//!
//! Produces the upstream 11-character `YXcstpoguax` string that describes
//! what changed about an entry during transfer.

use core::client::{ClientEntryKind, ClientEntryMetadata, ClientEvent, ClientEventKind};

/// Formats the itemized-changes string for a single event.
///
/// The output is an 11-character string matching upstream rsync's `log.c`
/// `YXcstpoguax` format, where Y is the update type, X is the file type,
/// and positions 2-10 indicate which attributes changed.
pub(super) fn format_itemized_changes(event: &ClientEvent, is_sender: bool) -> String {
    use ClientEventKind::*;

    if matches!(event.kind(), ClientEventKind::EntryDeleted) {
        // upstream: log.c:697 - padded to 11 chars to match YXcstpoguax width
        return "*deleting  ".to_owned();
    }

    let mut fields = ['.'; 11];

    let entry_kind = event
        .metadata()
        .map(ClientEntryMetadata::kind)
        .unwrap_or_else(|| match event.kind() {
            DirectoryCreated => ClientEntryKind::Directory,
            SymlinkCopied => ClientEntryKind::Symlink,
            FifoCopied => ClientEntryKind::Fifo,
            DeviceCopied => ClientEntryKind::CharDevice,
            HardLink
            | DataCopied
            | ReferenceCopied
            | MetadataReused
            | SkippedExisting
            | SkippedMissingDestination
            | SkippedNewerDestination => ClientEntryKind::File,
            _ => ClientEntryKind::Other,
        });
    let is_regular = matches!(entry_kind, ClientEntryKind::File);

    // upstream: log.c:704 - '<' when am_sender && !am_server, '>' otherwise
    fields[0] = match event.kind() {
        // upstream: log.c:704-710 - the transfer direction ('<'/'>') is only
        // emitted for a regular-file data transfer (ITEM_TRANSFER). A
        // non-regular entry - including a --fake-super device/FIFO/socket
        // placeholder whose real type comes from the `%stat` xattr - carries
        // no data and itemizes as a local change ('c', ITEM_LOCAL_CHANGE).
        DataCopied if is_regular => {
            if is_sender {
                '<'
            } else {
                '>'
            }
        }
        DataCopied => 'c',
        HardLink => 'h',
        // upstream: generator.c:1051 - copy-dest reconstruction itemizes with
        // ITEM_LOCAL_CHANGE, rendered as 'c' by log.c:707-708.
        ReferenceCopied => 'c',
        DirectoryCreated | SymlinkCopied | FifoCopied | DeviceCopied | SourceRemoved => 'c',
        MetadataReused
        | SkippedExisting
        | SkippedMissingDestination
        | SkippedNewerDestination
        | SkippedNonRegular
        | SkippedDirectory
        | SkippedUnsafeSymlink
        | SkippedMountPoint => '.',
        _ => '.',
    };

    fields[1] = match entry_kind {
        ClientEntryKind::File => 'f',
        ClientEntryKind::Directory => 'd',
        ClientEntryKind::Symlink => 'L',
        ClientEntryKind::Fifo | ClientEntryKind::Socket | ClientEntryKind::Other => 'S',
        ClientEntryKind::CharDevice | ClientEntryKind::BlockDevice => 'D',
    };

    if event.was_created() {
        for slot in fields.iter_mut().skip(2) {
            *slot = '+';
        }
        return fields.iter().collect();
    }

    let change_set = event.change_set();

    // upstream: log.c:730-734 - ITEM_MISSING_DATA fills attribute positions with '?'
    if change_set.missing_data() {
        for slot in fields.iter_mut().skip(2) {
            *slot = '?';
        }
        return fields.iter().collect();
    }
    let attr = &mut fields[2..];

    if change_set.checksum_changed() {
        attr[0] = 'c';
    }

    // upstream: log.c:706 - only regular files report size changes; symlinks,
    // devices, FIFOs, and sockets (including --fake-super placeholders that
    // virtualise to those types) never set the 's' column.
    if is_regular && change_set.size_changed() {
        attr[1] = 's';
    }

    if let Some(marker) = change_set.time_change_marker() {
        attr[2] = marker;
    }

    if change_set.permissions_changed() {
        attr[3] = 'p';
    }
    if change_set.owner_changed() {
        attr[4] = 'o';
    }
    if change_set.group_changed() {
        attr[5] = 'g';
    }
    attr[6] = match (
        change_set.access_time_changed(),
        change_set.create_time_changed(),
    ) {
        (true, true) => 'b',
        (true, false) => 'u',
        (false, true) => 'n',
        _ => attr[6],
    };
    if change_set.acl_changed() {
        attr[7] = 'a';
    }
    if change_set.xattr_changed() {
        attr[8] = 'x';
    }

    // upstream: log.c:735-744 - when update type is '.', 'h', or 'c' and all
    // attribute positions (2-10) are dots, collapse them to spaces.
    if matches!(fields[0], '.' | 'h' | 'c') && fields[2..].iter().all(|&ch| ch == '.') {
        for slot in fields[2..].iter_mut() {
            *slot = ' ';
        }
    }

    fields.iter().collect()
}
