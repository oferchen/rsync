#![deny(unsafe_code)]

//! Rendering helpers for parsed `--out-format` specifications.

use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::time::SystemTime;

use crate::{LIST_TIMESTAMP_FORMAT, describe_event_kind, format_list_permissions, platform};
use rsync_checksums::strong::Md5;
use rsync_core::client::{ClientEntryKind, ClientEntryMetadata, ClientEvent, ClientEventKind};
use time::OffsetDateTime;

use super::tokens::{OutFormat, OutFormatContext, OutFormatPlaceholder, OutFormatToken};

impl OutFormat {
    /// Renders an event according to the parsed `--out-format` tokens.
    pub(crate) fn render<W: Write + ?Sized>(
        &self,
        event: &ClientEvent,
        context: &OutFormatContext,
        writer: &mut W,
    ) -> io::Result<()> {
        use std::fmt::Write as _;

        let mut buffer = String::new();
        for token in self.tokens() {
            match token {
                OutFormatToken::Literal(text) => buffer.push_str(text),
                OutFormatToken::Placeholder(placeholder) => match placeholder {
                    OutFormatPlaceholder::FileName
                    | OutFormatPlaceholder::FileNameWithSymlinkTarget
                    | OutFormatPlaceholder::FullPath => {
                        append_rendered_path(
                            &mut buffer,
                            event,
                            matches!(
                                placeholder,
                                OutFormatPlaceholder::FileName
                                    | OutFormatPlaceholder::FileNameWithSymlinkTarget,
                            ),
                        );
                        if matches!(placeholder, OutFormatPlaceholder::FileNameWithSymlinkTarget) {
                            if let Some(metadata) = event.metadata() {
                                if let Some(target) = metadata.symlink_target() {
                                    buffer.push_str(" -> ");
                                    buffer.push_str(&target.to_string_lossy());
                                }
                            }
                        }
                    }
                    OutFormatPlaceholder::ItemizedChanges => {
                        buffer.push_str(&format_itemized_changes(event));
                    }
                    OutFormatPlaceholder::FileLength => {
                        let length = event
                            .metadata()
                            .map(ClientEntryMetadata::length)
                            .unwrap_or(0);
                        let _ = write!(&mut buffer, "{length}");
                    }
                    OutFormatPlaceholder::BytesTransferred => {
                        let bytes = event.bytes_transferred();
                        let _ = write!(&mut buffer, "{bytes}");
                    }
                    OutFormatPlaceholder::ChecksumBytes => {
                        let checksum_bytes = match event.kind() {
                            ClientEventKind::DataCopied => event.bytes_transferred(),
                            _ => 0,
                        };
                        let _ = write!(&mut buffer, "{checksum_bytes}");
                    }
                    OutFormatPlaceholder::Operation => {
                        buffer.push_str(describe_event_kind(event.kind()));
                    }
                    OutFormatPlaceholder::ModifyTime => {
                        buffer.push_str(&format_out_format_mtime(event.metadata()));
                    }
                    OutFormatPlaceholder::PermissionString => {
                        buffer.push_str(&format_out_format_permissions(event.metadata()));
                    }
                    OutFormatPlaceholder::SymlinkTarget => {
                        if let Some(target) = event
                            .metadata()
                            .and_then(ClientEntryMetadata::symlink_target)
                        {
                            buffer.push_str(" -> ");
                            buffer.push_str(&target.to_string_lossy());
                        }
                    }
                    OutFormatPlaceholder::CurrentTime => {
                        buffer.push_str(&format_current_timestamp());
                    }
                    OutFormatPlaceholder::OwnerName => {
                        buffer.push_str(&format_owner_name(event.metadata()));
                    }
                    OutFormatPlaceholder::GroupName => {
                        buffer.push_str(&format_group_name(event.metadata()));
                    }
                    OutFormatPlaceholder::OwnerUid => {
                        let uid = event
                            .metadata()
                            .and_then(ClientEntryMetadata::uid)
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "0".to_string());
                        buffer.push_str(&uid);
                    }
                    OutFormatPlaceholder::OwnerGid => {
                        let gid = event
                            .metadata()
                            .and_then(ClientEntryMetadata::gid)
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "0".to_string());
                        buffer.push_str(&gid);
                    }
                    OutFormatPlaceholder::ProcessId => {
                        let pid = std::process::id();
                        let _ = write!(&mut buffer, "{pid}");
                    }
                    OutFormatPlaceholder::RemoteHost => {
                        append_remote_placeholder(&mut buffer, context.remote_host.as_deref(), 'h');
                    }
                    OutFormatPlaceholder::RemoteAddress => {
                        append_remote_placeholder(
                            &mut buffer,
                            context.remote_address.as_deref(),
                            'a',
                        );
                    }
                    OutFormatPlaceholder::ModuleName => {
                        append_remote_placeholder(&mut buffer, context.module_name.as_deref(), 'm');
                    }
                    OutFormatPlaceholder::ModulePath => {
                        append_remote_placeholder(&mut buffer, context.module_path.as_deref(), 'P');
                    }
                    OutFormatPlaceholder::FullChecksum => {
                        buffer.push_str(&format_full_checksum(event));
                    }
                },
            }
        }

        if buffer.ends_with('\n') {
            writer.write_all(buffer.as_bytes())
        } else {
            writer.write_all(buffer.as_bytes())?;
            writer.write_all(b"\n")
        }
    }
}

/// Emits each event using the supplied `--out-format` specification.
pub(crate) fn emit_out_format<W: Write + ?Sized>(
    events: &[ClientEvent],
    format: &OutFormat,
    context: &OutFormatContext,
    writer: &mut W,
) -> io::Result<()> {
    for event in events {
        format.render(event, context, writer)?;
    }
    Ok(())
}

fn append_remote_placeholder(buffer: &mut String, value: Option<&str>, token: char) {
    if let Some(text) = value {
        buffer.push_str(text);
    } else {
        buffer.push('%');
        buffer.push(token);
    }
}

fn append_rendered_path(buffer: &mut String, event: &ClientEvent, ensure_trailing_slash: bool) {
    let mut rendered = event.relative_path().to_string_lossy().into_owned();
    if ensure_trailing_slash
        && !rendered.ends_with('/')
        && event
            .metadata()
            .map(ClientEntryMetadata::kind)
            .map(ClientEntryKind::is_directory)
            .unwrap_or_else(|| matches!(event.kind(), ClientEventKind::DirectoryCreated))
    {
        rendered.push('/');
    }
    buffer.push_str(&rendered);
}

fn format_out_format_mtime(metadata: Option<&ClientEntryMetadata>) -> String {
    metadata
        .and_then(|meta| meta.modified())
        .and_then(|time| {
            OffsetDateTime::from(time)
                .format(LIST_TIMESTAMP_FORMAT)
                .ok()
        })
        .map(|formatted| formatted.replace(' ', "-"))
        .unwrap_or_else(|| "1970/01/01-00:00:00".to_string())
}

fn format_out_format_permissions(metadata: Option<&ClientEntryMetadata>) -> String {
    metadata
        .map(format_list_permissions)
        .map(|mut perms| {
            if !perms.is_empty() {
                perms.remove(0);
            }
            perms
        })
        .unwrap_or_else(|| "---------".to_string())
}

fn format_owner_name(metadata: Option<&ClientEntryMetadata>) -> String {
    metadata
        .and_then(ClientEntryMetadata::uid)
        .map(resolve_user_name)
        .unwrap_or_else(|| "0".to_string())
}

fn format_group_name(metadata: Option<&ClientEntryMetadata>) -> String {
    metadata
        .and_then(ClientEntryMetadata::gid)
        .map(resolve_group_name)
        .unwrap_or_else(|| "0".to_string())
}

#[cfg(unix)]
fn resolve_user_name(uid: u32) -> String {
    platform::display_user_name(uid).unwrap_or_else(|| uid.to_string())
}

#[cfg(not(unix))]
fn resolve_user_name(uid: u32) -> String {
    uid.to_string()
}

#[cfg(unix)]
fn resolve_group_name(gid: u32) -> String {
    platform::display_group_name(gid).unwrap_or_else(|| gid.to_string())
}

#[cfg(not(unix))]
fn resolve_group_name(gid: u32) -> String {
    gid.to_string()
}

fn format_current_timestamp() -> String {
    let now = OffsetDateTime::from(SystemTime::now());
    now.format(LIST_TIMESTAMP_FORMAT)
        .map(|text| text.replace(' ', "-"))
        .unwrap_or_else(|_| "1970/01/01-00:00:00".to_string())
}

fn format_itemized_changes(event: &ClientEvent) -> String {
    use ClientEventKind::*;

    if matches!(event.kind(), ClientEventKind::EntryDeleted) {
        return "*deleting".to_string();
    }

    let mut fields = ['.'; 11];

    fields[0] = match event.kind() {
        DataCopied => '>',
        MetadataReused
        | SkippedExisting
        | SkippedNewerDestination
        | SkippedNonRegular
        | SkippedUnsafeSymlink
        | SkippedMountPoint => '.',
        HardLink => 'h',
        DirectoryCreated | SymlinkCopied | FifoCopied | DeviceCopied | SourceRemoved => 'c',
        _ => '.',
    };

    fields[1] = match event
        .metadata()
        .map(ClientEntryMetadata::kind)
        .unwrap_or_else(|| match event.kind() {
            DirectoryCreated => ClientEntryKind::Directory,
            SymlinkCopied => ClientEntryKind::Symlink,
            FifoCopied => ClientEntryKind::Fifo,
            DeviceCopied => ClientEntryKind::CharDevice,
            HardLink | DataCopied | MetadataReused | SkippedExisting | SkippedNewerDestination => {
                ClientEntryKind::File
            }
            _ => ClientEntryKind::Other,
        }) {
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

    let attr = &mut fields[2..];

    match event.kind() {
        DirectoryCreated | SymlinkCopied | FifoCopied | DeviceCopied | HardLink => {
            attr.fill('+');
        }
        DataCopied => {
            attr[0] = 'c';
            attr[1] = 's';
            attr[2] = 't';
        }
        SourceRemoved => {
            attr[0] = 'c';
        }
        _ => {}
    }

    fields.iter().collect()
}

fn format_full_checksum(event: &ClientEvent) -> String {
    const EMPTY_CHECKSUM: &str = "                                ";

    if !matches!(
        event.kind(),
        ClientEventKind::DataCopied | ClientEventKind::MetadataReused | ClientEventKind::HardLink,
    ) {
        return EMPTY_CHECKSUM.to_string();
    }

    if let Some(metadata) = event.metadata() {
        if metadata.kind() != ClientEntryKind::File {
            return EMPTY_CHECKSUM.to_string();
        }
    }

    let path = event.destination_path();
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(_) => return EMPTY_CHECKSUM.to_string(),
    };

    let mut hasher = Md5::new();
    let mut buffer = [0u8; 32 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buffer[..read]),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return EMPTY_CHECKSUM.to_string(),
        }
    }

    let digest = hasher.finalize();
    let mut rendered = String::with_capacity(32);
    for byte in digest {
        rendered.push_str(&format!("{byte:02x}"));
    }
    rendered
}
