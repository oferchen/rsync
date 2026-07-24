#![deny(unsafe_code)]

//! Placeholder value resolution for `--out-format` tokens.
//!
//! Maps each `OutFormatPlaceholder` variant to its rendered string value
//! by inspecting the event, its metadata, and the rendering context.

use std::time::SystemTime;

use crate::frontend::escape::escape_path;
use crate::{LIST_TIMESTAMP_FORMAT, format_list_permissions};
use core::client::{ClientEntryKind, ClientEntryMetadata, ClientEvent, ClientEventKind};

use crate::frontend::out_format::tokens::{
    MAX_PLACEHOLDER_WIDTH, OutFormatContext, OutFormatPlaceholder, PlaceholderToken,
};

use super::checksum::format_full_checksum;
use super::format::format_numeric_value;
use super::itemize::format_itemized_changes;

/// Returns the `%L` connector for an event carrying a target in its metadata.
///
/// upstream: log.c:643-654 - `%L` renders ` -> %s` when the entry itself is a
/// symlink (`S_ISLNK`) and ` => %s` for a hard-link xname (`=> leader`). A
/// hard-linked symlink is still a symlink, so it uses ` -> `; only a hard-linked
/// regular file uses ` => `.
fn symlink_target_connector(event: &ClientEvent) -> &'static str {
    let is_symlink = event
        .metadata()
        .map(ClientEntryMetadata::kind)
        .is_some_and(|kind| matches!(kind, ClientEntryKind::Symlink));
    if matches!(event.kind(), ClientEventKind::HardLink) && !is_symlink {
        " => "
    } else {
        " -> "
    }
}

/// Resolves a placeholder token to its raw byte value for the given event and context.
///
/// Returns `None` when the placeholder is inapplicable (e.g., symlink target on
/// a regular file). Path placeholders (`%n`/`%f`/`%L`) carry raw filename bytes
/// so a lone invalid-UTF-8 byte under `--8-bit-output` survives verbatim to the
/// writer; all other placeholders are valid UTF-8 rendered into bytes.
pub(super) fn render_placeholder_value(
    event: &ClientEvent,
    context: &OutFormatContext,
    spec: &PlaceholderToken,
) -> Option<Vec<u8>> {
    let allow_8bit = context.eight_bit_output;
    match spec.kind {
        OutFormatPlaceholder::FileName => Some(render_path(event, true, allow_8bit)),
        OutFormatPlaceholder::FullPath => Some(render_path(event, false, allow_8bit)),
        OutFormatPlaceholder::ItemizedChanges => {
            Some(format_itemized_changes(event, context.is_sender).into_bytes())
        }
        OutFormatPlaceholder::FileLength => {
            let length = event.metadata().map_or(0, ClientEntryMetadata::length);
            Some(format_numeric_value(length as i64, &spec.format).into_bytes())
        }
        OutFormatPlaceholder::BytesTransferred => Some(
            format_numeric_value(
                transfer_byte_count(event, context.is_sender, false) as i64,
                &spec.format,
            )
            .into_bytes(),
        ),
        OutFormatPlaceholder::ChecksumBytes => Some(
            format_numeric_value(
                transfer_byte_count(event, context.is_sender, true) as i64,
                &spec.format,
            )
            .into_bytes(),
        ),
        OutFormatPlaceholder::Operation => Some(
            upstream_operation(event.kind(), context.is_pull)
                .as_bytes()
                .to_vec(),
        ),
        OutFormatPlaceholder::ModifyTime => {
            Some(format_out_format_mtime(event.metadata()).into_bytes())
        }
        OutFormatPlaceholder::PermissionString => {
            Some(format_out_format_permissions(event.metadata()).into_bytes())
        }
        OutFormatPlaceholder::SymlinkTarget => match event
            .metadata()
            .and_then(ClientEntryMetadata::symlink_target)
        {
            Some(target) => {
                let mut rendered = symlink_target_connector(event).as_bytes().to_vec();
                rendered.extend_from_slice(&escape_path(target, allow_8bit));
                Some(rendered)
            }
            // upstream: log.c:648-654 - the `case 'L'` else-branch sets n = ""
            // for a non-link/non-hardlink entry. With no width modifier upstream
            // breaks with the empty string (matched here by returning None); with
            // a width modifier it copies four leading spaces then formats the
            // empty string under the width specifier, emitting `4 + width` spaces
            // so the empty target aligns under the ` -> ` connector column.
            None => spec
                .format
                .width()
                .map(|width| vec![b' '; 4 + width.min(MAX_PLACEHOLDER_WIDTH)]),
        },
        OutFormatPlaceholder::CurrentTime => Some(format_current_timestamp().into_bytes()),
        OutFormatPlaceholder::OwnerUid => Some(
            event
                .metadata()
                .and_then(ClientEntryMetadata::uid)
                .map_or_else(|| "0".to_owned(), |value| value.to_string())
                .into_bytes(),
        ),
        // upstream: log.c:574-576 - `case 'G'` renders the literal "DEFAULT"
        // when `!gid_ndx || file->flags & FLAG_SKIP_GROUP`; only an available
        // gid is formatted numerically. This differs from `%U` (log.c:570-573),
        // which renders 0 for an unavailable uid.
        OutFormatPlaceholder::OwnerGid => Some(
            event
                .metadata()
                .and_then(ClientEntryMetadata::gid)
                .map_or_else(|| "DEFAULT".to_owned(), |value| value.to_string())
                .into_bytes(),
        ),
        OutFormatPlaceholder::ProcessId => Some(std::process::id().to_string().into_bytes()),
        OutFormatPlaceholder::RemoteHost => {
            Some(remote_placeholder_value(context.remote_host.as_deref(), 'h').into_bytes())
        }
        OutFormatPlaceholder::RemoteAddress => {
            Some(remote_placeholder_value(context.remote_address.as_deref(), 'a').into_bytes())
        }
        OutFormatPlaceholder::ModuleName => {
            Some(remote_placeholder_value(context.module_name.as_deref(), 'm').into_bytes())
        }
        OutFormatPlaceholder::ModulePath => {
            Some(remote_placeholder_value(context.module_path.as_deref(), 'P').into_bytes())
        }
        OutFormatPlaceholder::FullChecksum => {
            Some(format_full_checksum(event, context).into_bytes())
        }
    }
}

/// Wire size of the `sum_head` a receiver sends per transferred file: four
/// 32-bit little-endian fields (count, blength, s2length, remainder). In the
/// local-copy path transfers are always whole-file, so the header is empty
/// (count=0) and its size is the constant 16 bytes the sender reads back.
///
/// upstream: rsync.h:200 `struct sum_struct`; match.c:380 `write_sum_head()`.
const SUM_HEAD_WIRE_BYTES: u64 = 16;

/// Resolves the byte count for `%b` / `%c`, selecting the direction the way
/// upstream does.
///
/// upstream: log.c:672-684 - `%b` and `%c` are the two per-file wire byte
/// deltas. When the entry was not transferred (`!(iflags & ITEM_TRANSFER)`)
/// both render 0. Otherwise `(!!am_sender) ^ (*p == 'c')` selects between the
/// bytes written (`total_data_written - initial_data_written`) and the bytes
/// read (`total_data_read - initial_data_read`). On the sender the written
/// direction carries the file data and the read direction carries the checksum
/// header echoed back; on the receiver they swap onto the opposite physical
/// counters. The net semantic is role-independent: `%b` always reports the
/// file-data bytes and `%c` always reports the checksum-header bytes.
///
/// oc-rsync's local-copy engine records the file-data bytes as
/// `bytes_transferred`; the checksum direction is the whole-file empty
/// [`SUM_HEAD_WIRE_BYTES`] header. `want_checksum` picks between the two, and
/// the `is_sender` XOR reproduces upstream's counter mapping so `%b`/`%c`
/// remain correct for either transfer role.
fn transfer_byte_count(event: &ClientEvent, is_sender: bool, want_checksum: bool) -> u64 {
    if !matches!(event.kind(), ClientEventKind::DataCopied) {
        return 0;
    }
    // upstream `(!!am_sender) ^ (*p == 'c')`: true -> the bytes-written counter,
    // false -> the bytes-read counter. On the sender the written counter holds
    // the file data (read holds the checksum header); on the receiver the roles
    // of the two physical counters swap. Map each selected counter back to the
    // quantity oc-rsync tracks per file so the printed value matches upstream.
    let selects_written = is_sender ^ want_checksum;
    let written_is_data = is_sender;
    if selects_written == written_is_data {
        event.bytes_transferred()
    } else {
        SUM_HEAD_WIRE_BYTES
    }
}

/// Renders the path from an event as raw bytes, optionally appending a trailing
/// slash for directories.
fn render_path(event: &ClientEvent, ensure_trailing_slash: bool, allow_8bit: bool) -> Vec<u8> {
    let mut rendered = escape_path(event.relative_path(), allow_8bit);
    // upstream: flist.c / log.c - itemize and out-format paths use POSIX
    // forward-slash separators regardless of host OS. Normalize Windows
    // native backslashes here at the rendering boundary; storage retains
    // the platform-native form.
    #[cfg(windows)]
    {
        for byte in rendered.iter_mut() {
            if *byte == b'\\' {
                *byte = b'/';
            }
        }
    }
    if ensure_trailing_slash
        && rendered.last() != Some(&b'/')
        && event.metadata().map(ClientEntryMetadata::kind).map_or_else(
            // upstream: log.c:639-640 - %n appends `/` for any directory entry.
            // `EntryDeleted` rows carry no metadata snapshot, so fall back to the
            // record's directory bit (set by the engine cleanup pass) alongside
            // the freshly-created-directory case.
            || matches!(event.kind(), ClientEventKind::DirectoryCreated) || event.is_directory(),
            ClientEntryKind::is_directory,
        )
    {
        rendered.push(b'/');
    }
    rendered
}

/// Falls back to a literal `%<token>` when a remote context value is unavailable.
fn remote_placeholder_value(value: Option<&str>, token: char) -> String {
    value.map_or_else(|| format!("%{token}"), str::to_owned)
}

/// Formats the modification time from metadata, or returns an epoch placeholder.
fn format_out_format_mtime(metadata: Option<&ClientEntryMetadata>) -> String {
    metadata
        .and_then(|meta| meta.modified())
        .and_then(|time| {
            crate::frontend::local_time::to_local(time)
                .format(LIST_TIMESTAMP_FORMAT)
                .ok()
        })
        .map_or_else(
            || "1970/01/01-00:00:00".to_owned(),
            |formatted| formatted.replace(' ', "-"),
        )
}

/// Formats the permission string from metadata, stripping the leading type character.
fn format_out_format_permissions(metadata: Option<&ClientEntryMetadata>) -> String {
    metadata
        .map(format_list_permissions)
        .map(|mut perms| {
            if !perms.is_empty() {
                perms.remove(0);
            }
            perms
        })
        .unwrap_or_else(|| "---------".to_owned())
}

/// Maps a transfer event to upstream's `%o` operation word.
///
/// upstream log.c `case 'o': n = op` - `op` is `"del."` for a deletion
/// (`log_delete`) and otherwise the transfer direction `s_or_r` from
/// `log_item`: `"recv"` on the receiving client (a pull) and `"send"` otherwise
/// (a push or a local copy). This split differs from the `<`/`>` itemize arrow -
/// a local copy renders `>` yet reports `send` - so it keys on the pull flag
/// rather than the sender role. oc's richer event kinds collapse to these three
/// so drop-in `--out-format=%o` output matches byte-for-byte.
fn upstream_operation(kind: &ClientEventKind, is_pull: bool) -> &'static str {
    match kind {
        ClientEventKind::EntryDeleted => "del.",
        _ if is_pull => "recv",
        _ => "send",
    }
}

/// Formats the current wall-clock time using the list timestamp format.
fn format_current_timestamp() -> String {
    let now = crate::frontend::local_time::to_local(SystemTime::now());
    now.format(LIST_TIMESTAMP_FORMAT).map_or_else(
        |_| "1970/01/01-00:00:00".to_owned(),
        |text| text.replace(' ', "-"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_placeholder_value_some() {
        assert_eq!(
            remote_placeholder_value(Some("example.com"), 'h'),
            "example.com"
        );
        assert_eq!(
            remote_placeholder_value(Some("192.168.1.1"), 'a'),
            "192.168.1.1"
        );
    }

    #[test]
    fn remote_placeholder_value_none() {
        assert_eq!(remote_placeholder_value(None, 'h'), "%h");
        assert_eq!(remote_placeholder_value(None, 'a'), "%a");
        assert_eq!(remote_placeholder_value(None, 'm'), "%m");
        assert_eq!(remote_placeholder_value(None, 'P'), "%P");
    }

    #[test]
    fn format_out_format_permissions_none() {
        assert_eq!(format_out_format_permissions(None), "---------");
    }

    #[test]
    fn format_out_format_mtime_none() {
        assert_eq!(format_out_format_mtime(None), "1970/01/01-00:00:00");
    }
}
