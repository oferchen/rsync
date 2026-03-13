#![deny(unsafe_code)]

//! Placeholder value resolution for `--out-format` tokens.
//!
//! Maps each `OutFormatPlaceholder` variant to its rendered string value
//! by inspecting the event, its metadata, and the rendering context.

use std::time::SystemTime;

use crate::{LIST_TIMESTAMP_FORMAT, describe_event_kind, format_list_permissions, platform};
use core::client::{ClientEntryKind, ClientEntryMetadata, ClientEvent, ClientEventKind};
use time::OffsetDateTime;

use crate::frontend::out_format::tokens::{
    OutFormatContext, OutFormatPlaceholder, PlaceholderToken,
};

use super::checksum::format_full_checksum;
use super::format::format_numeric_value;
use super::itemize::format_itemized_changes;

/// Resolves a placeholder token to its string value for the given event and context.
///
/// Returns `None` when the placeholder is inapplicable (e.g., symlink target on a regular file).
pub(super) fn render_placeholder_value(
    event: &ClientEvent,
    context: &OutFormatContext,
    spec: &PlaceholderToken,
) -> Option<String> {
    match spec.kind {
        OutFormatPlaceholder::FileName => Some(render_path(event, true)),
        OutFormatPlaceholder::FileNameWithSymlinkTarget => {
            let mut rendered = render_path(event, true);
            if let Some(target) = event
                .metadata()
                .and_then(ClientEntryMetadata::symlink_target)
            {
                rendered.push_str(" -> ");
                rendered.push_str(&target.to_string_lossy());
            }
            Some(rendered)
        }
        OutFormatPlaceholder::FullPath => Some(render_path(event, false)),
        OutFormatPlaceholder::ItemizedChanges => {
            Some(format_itemized_changes(event, context.is_sender))
        }
        OutFormatPlaceholder::FileLength => {
            let length = event.metadata().map_or(0, ClientEntryMetadata::length);
            Some(format_numeric_value(length as i64, &spec.format))
        }
        OutFormatPlaceholder::BytesTransferred => Some(format_numeric_value(
            event.bytes_transferred() as i64,
            &spec.format,
        )),
        OutFormatPlaceholder::ChecksumBytes => {
            let checksum_bytes = match event.kind() {
                ClientEventKind::DataCopied => event.bytes_transferred(),
                _ => 0,
            };
            Some(format_numeric_value(checksum_bytes as i64, &spec.format))
        }
        OutFormatPlaceholder::Operation => Some(describe_event_kind(event.kind()).to_owned()),
        OutFormatPlaceholder::ModifyTime => Some(format_out_format_mtime(event.metadata())),
        OutFormatPlaceholder::PermissionString => {
            Some(format_out_format_permissions(event.metadata()))
        }
        OutFormatPlaceholder::SymlinkTarget => event
            .metadata()
            .and_then(ClientEntryMetadata::symlink_target)
            .map(|target| {
                let mut rendered = String::from(" -> ");
                rendered.push_str(&target.to_string_lossy());
                rendered
            }),
        OutFormatPlaceholder::CurrentTime => Some(format_current_timestamp()),
        OutFormatPlaceholder::OwnerName => Some(format_owner_name(event.metadata())),
        OutFormatPlaceholder::GroupName => Some(format_group_name(event.metadata())),
        OutFormatPlaceholder::OwnerUid => Some(
            event
                .metadata()
                .and_then(ClientEntryMetadata::uid)
                .map_or_else(|| "0".to_owned(), |value| value.to_string()),
        ),
        OutFormatPlaceholder::OwnerGid => Some(
            event
                .metadata()
                .and_then(ClientEntryMetadata::gid)
                .map_or_else(|| "0".to_owned(), |value| value.to_string()),
        ),
        OutFormatPlaceholder::ProcessId => Some(std::process::id().to_string()),
        OutFormatPlaceholder::RemoteHost => Some(remote_placeholder_value(
            context.remote_host.as_deref(),
            'h',
        )),
        OutFormatPlaceholder::RemoteAddress => Some(remote_placeholder_value(
            context.remote_address.as_deref(),
            'a',
        )),
        OutFormatPlaceholder::ModuleName => Some(remote_placeholder_value(
            context.module_name.as_deref(),
            'm',
        )),
        OutFormatPlaceholder::ModulePath => Some(remote_placeholder_value(
            context.module_path.as_deref(),
            'P',
        )),
        OutFormatPlaceholder::FullChecksum => Some(format_full_checksum(event)),
    }
}

/// Renders the path from an event, optionally appending a trailing slash for directories.
fn render_path(event: &ClientEvent, ensure_trailing_slash: bool) -> String {
    let mut rendered = event.relative_path().to_string_lossy().into_owned();
    if ensure_trailing_slash
        && !rendered.ends_with('/')
        && event.metadata().map(ClientEntryMetadata::kind).map_or_else(
            || matches!(event.kind(), ClientEventKind::DirectoryCreated),
            ClientEntryKind::is_directory,
        )
    {
        rendered.push('/');
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
            OffsetDateTime::from(time)
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

/// Resolves the owner name for a uid, falling back to the numeric string.
fn format_owner_name(metadata: Option<&ClientEntryMetadata>) -> String {
    metadata
        .and_then(ClientEntryMetadata::uid)
        .map_or_else(|| "0".to_owned(), resolve_user_name)
}

/// Resolves the group name for a gid, falling back to the numeric string.
fn format_group_name(metadata: Option<&ClientEntryMetadata>) -> String {
    metadata
        .and_then(ClientEntryMetadata::gid)
        .map_or_else(|| "0".to_owned(), resolve_group_name)
}

fn resolve_user_name(uid: u32) -> String {
    platform::display_user_name(uid).unwrap_or_else(|| uid.to_string())
}

fn resolve_group_name(gid: u32) -> String {
    platform::display_group_name(gid).unwrap_or_else(|| gid.to_string())
}

/// Formats the current wall-clock time using the list timestamp format.
fn format_current_timestamp() -> String {
    let now = OffsetDateTime::from(SystemTime::now());
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
    fn format_owner_name_none() {
        assert_eq!(format_owner_name(None), "0");
    }

    #[test]
    fn format_group_name_none() {
        assert_eq!(format_group_name(None), "0");
    }

    #[test]
    fn format_out_format_mtime_none() {
        assert_eq!(format_out_format_mtime(None), "1970/01/01-00:00:00");
    }
}
