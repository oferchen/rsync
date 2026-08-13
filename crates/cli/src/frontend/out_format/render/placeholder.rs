#![deny(unsafe_code)]

//! Placeholder value resolution for `--out-format` tokens.
//!
//! Maps each `OutFormatPlaceholder` variant to its rendered string value
//! by inspecting the event, its metadata, and the rendering context.

use std::path::Path;
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
        OutFormatPlaceholder::FileName => Some(render_name(event, allow_8bit)),
        OutFormatPlaceholder::FullPath => Some(render_full_path(event, allow_8bit)),
        OutFormatPlaceholder::ItemizedChanges => Some(match event.itemize_override() {
            // A remote transfer supplies the sender's already-correct 11-char
            // itemize string; a local event derives it from its change set.
            Some(itemize) => itemize.as_bytes().to_vec(),
            None => format_itemized_changes(event, context.is_sender).into_bytes(),
        }),
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
        OutFormatPlaceholder::SymlinkTarget => {
            // upstream: log.c:643-646 - `%L` renders the hard-link leader
            // (`hlink`) before the symlink target: `if (hlink && *hlink) { n =
            // hlink; " => " }`. A remote pull carries the leader in
            // `hardlink_leader`; a local copy routes it through the metadata
            // `symlink_target` slot keyed on the `HardLink` event kind (handled by
            // `symlink_target_connector` in the else-branch).
            if let Some(leader) = event.hardlink_leader() {
                let mut rendered = b" => ".to_vec();
                rendered.extend_from_slice(&escape_path(leader, allow_8bit));
                Some(rendered)
            } else {
                match event
                    .metadata()
                    .and_then(ClientEntryMetadata::symlink_target)
                {
                    Some(target) => {
                        let mut rendered = symlink_target_connector(event).as_bytes().to_vec();
                        rendered.extend_from_slice(&escape_path(target, allow_8bit));
                        Some(rendered)
                    }
                    // upstream: log.c:675-679 - the `case 'L'` else-branch sets n
                    // = "" for a non-link/non-hardlink entry. With no width
                    // modifier upstream breaks with the empty string (matched here
                    // by returning None); with a width modifier it copies four
                    // leading spaces then formats the empty string under the width
                    // specifier, emitting `4 + width` spaces so the empty target
                    // aligns under the ` -> ` connector column.
                    None => spec
                        .format
                        .width()
                        .map(|width| vec![b' '; 4 + width.min(MAX_PLACEHOLDER_WIDTH)]),
                }
            }
        }
        OutFormatPlaceholder::CurrentTime => Some(format_current_timestamp().into_bytes()),
        // upstream: log.c:600-603 - `case 'U'` renders `uid_ndx ? F_OWNER : 0`,
        // so the numeric uid appears only under `-o`/`--owner`; otherwise `0`.
        OutFormatPlaceholder::OwnerUid => Some(
            if context.preserve_owner {
                event
                    .metadata()
                    .and_then(ClientEntryMetadata::uid)
                    .map_or_else(|| "0".to_owned(), |value| value.to_string())
            } else {
                "0".to_owned()
            }
            .into_bytes(),
        ),
        // upstream: log.c:606-608 - `case 'G'` renders the literal "DEFAULT"
        // when `!gid_ndx || file->flags & FLAG_SKIP_GROUP`; only an available
        // gid under `-g`/`--group` is formatted numerically. This differs from
        // `%U` (log.c:570-573), which renders 0 for an unavailable uid.
        OutFormatPlaceholder::OwnerGid => Some(
            if context.preserve_group {
                event
                    .metadata()
                    .and_then(ClientEntryMetadata::gid)
                    .map_or_else(|| "DEFAULT".to_owned(), |value| value.to_string())
            } else {
                "DEFAULT".to_owned()
            }
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
/// upstream: rsync.h:987 `struct sum_struct`; io.c:2257 `write_sum_head()`,
/// which emits `s2length` only for `protocol_version >= 27` (always true for
/// the protocol range oc-rsync speaks).
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

/// Rewrites Windows path separators (`\`) to POSIX `/` in already-escaped
/// render bytes, leaving `\#ooo` octal escape sequences intact.
///
/// Pure and platform-independent so it is unit-testable on every host;
/// [`escape_render_path`] invokes it only under `cfg(windows)`, where `\` is the
/// native separator. A naive rewrite of every backslash would corrupt the
/// leading `\` of a `\#ooo` escape (a non-ASCII name such as `café`, escaped to
/// `caf\#303\#251` in the default non-`-8` mode, would become `caf/#303/#251`),
/// so the leading backslash of a `\#` + three-digit sequence is preserved.
///
/// upstream: rsync stores filenames with `/` separators before logging
/// (flist.c), so `filtered_fwrite` never sees a `\` separator; oc-rsync retains
/// the platform-native separator in storage and normalizes only here, at the
/// render boundary.
#[cfg(any(windows, test))]
fn normalize_render_separators(escaped: &[u8]) -> Vec<u8> {
    let len = escaped.len();
    let mut out = Vec::with_capacity(len);
    let mut i = 0;
    while i < len {
        let byte = escaped[i];
        if byte == b'\\' {
            // A `\#ddd` escape sequence keeps its backslash; a bare separator
            // backslash becomes `/`. Mirrors the escape guard in escape.rs.
            if i + 4 < len
                && escaped[i + 1] == b'#'
                && escaped[i + 2].is_ascii_digit()
                && escaped[i + 3].is_ascii_digit()
                && escaped[i + 4].is_ascii_digit()
            {
                out.extend_from_slice(&escaped[i..i + 5]);
                i += 5;
                continue;
            }
            out.push(b'/');
        } else {
            out.push(byte);
        }
        i += 1;
    }
    out
}

/// Escapes a path to raw output bytes, normalizing Windows separators.
///
/// upstream: flist.c / log.c - itemize and out-format paths use POSIX
/// forward-slash separators regardless of host OS. Storage retains the
/// platform-native form; this normalizes only at the rendering boundary.
fn escape_render_path(path: &Path, allow_8bit: bool) -> Vec<u8> {
    let rendered = escape_path(path, allow_8bit);
    #[cfg(windows)]
    {
        normalize_render_separators(&rendered)
    }
    #[cfg(not(windows))]
    {
        rendered
    }
}

/// Renders `%n`: the transfer-relative name, with a trailing slash for a
/// directory (upstream `log.c:639-640`).
fn render_name(event: &ClientEvent, allow_8bit: bool) -> Vec<u8> {
    let mut rendered = escape_render_path(event.relative_path(), allow_8bit);
    if rendered.last() != Some(&b'/')
        && event.metadata().map(ClientEntryMetadata::kind).map_or_else(
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

/// Renders `%f`: on a push the full source-side path the sender supplied
/// (upstream `pathjoin(F_PATHNAME(file), f_name(file))` with a single leading
/// `/` stripped, `log.c`), otherwise the transfer-relative name. Unlike `%n`, no
/// trailing slash is appended for directories.
fn render_full_path(event: &ClientEvent, allow_8bit: bool) -> Vec<u8> {
    let path = match event.source_prefix() {
        Some(prefix) => prefix,
        None => event.relative_path(),
    };
    let mut rendered = escape_render_path(path, allow_8bit);
    // upstream: log.c `%f` strips a single leading '/' from the joined path.
    if rendered.first() == Some(&b'/') {
        rendered.remove(0);
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

    // -- normalize_render_separators (pure, tested on all platforms) --

    #[test]
    fn normalize_render_separators_rewrites_backslashes() {
        // WHY: upstream logs POSIX `/` separators regardless of host (flist.c
        // stores `/` before logging); the Windows render path must present
        // `a\b\c` as `a/b/c`.
        assert_eq!(normalize_render_separators(b"a\\b\\c"), b"a/b/c".to_vec());
    }

    #[test]
    fn normalize_render_separators_preserves_octal_escapes() {
        // WHY: `café` in the default (non-`-8`) mode escapes to `caf\#303\#251`.
        // A naive backslash rewrite would corrupt it to `caf/#303/#251`; the
        // `\#ooo` escape backslash is not a separator and must survive so `%n`
        // stays byte-faithful to upstream `filtered_fwrite`.
        assert_eq!(
            normalize_render_separators(b"caf\\#303\\#251"),
            b"caf\\#303\\#251".to_vec()
        );
    }

    #[test]
    fn normalize_render_separators_mixed_sep_and_escape() {
        // Real separators become `/` while an embedded octal escape is kept.
        assert_eq!(
            normalize_render_separators(b"dir\\caf\\#303\\#251"),
            b"dir/caf\\#303\\#251".to_vec()
        );
    }

    #[test]
    fn normalize_render_separators_trailing_backslash_is_separator() {
        // A backslash with fewer than the 4 trailing bytes of an escape
        // sequence is a bare separator and is rewritten.
        assert_eq!(normalize_render_separators(b"a\\"), b"a/".to_vec());
        assert_eq!(normalize_render_separators(b"a\\#12"), b"a/#12".to_vec());
    }

    #[test]
    fn normalize_render_separators_noop_without_backslash() {
        assert_eq!(normalize_render_separators(b"a/b/c"), b"a/b/c".to_vec());
    }
}
