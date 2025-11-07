#![deny(unsafe_code)]

//! Rendering helpers for parsed `--out-format` specifications.

use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::time::SystemTime;

use crate::{LIST_TIMESTAMP_FORMAT, describe_event_kind, format_list_permissions, platform};
use oc_rsync_checksums::strong::Md5;
use oc_rsync_core::client::{ClientEntryKind, ClientEntryMetadata, ClientEvent, ClientEventKind};
use time::OffsetDateTime;

use super::tokens::{
    HumanizeMode, MAX_PLACEHOLDER_WIDTH, OutFormat, OutFormatContext, OutFormatPlaceholder,
    OutFormatToken, PlaceholderAlignment, PlaceholderFormat, PlaceholderToken,
};

impl OutFormat {
    /// Renders an event according to the parsed `--out-format` tokens.
    pub(crate) fn render<W: Write + ?Sized>(
        &self,
        event: &ClientEvent,
        context: &OutFormatContext,
        writer: &mut W,
    ) -> io::Result<()> {
        let mut buffer = String::new();
        for token in self.tokens() {
            match token {
                OutFormatToken::Literal(text) => buffer.push_str(text),
                OutFormatToken::Placeholder(spec) => {
                    if let Some(rendered) = render_placeholder_value(event, context, spec) {
                        let formatted = apply_placeholder_format(rendered, &spec.format);
                        buffer.push_str(&formatted);
                    }
                }
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

fn render_placeholder_value(
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
        OutFormatPlaceholder::ItemizedChanges => Some(format_itemized_changes(event)),
        OutFormatPlaceholder::FileLength => {
            let length = event
                .metadata()
                .map(ClientEntryMetadata::length)
                .unwrap_or(0);
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
        OutFormatPlaceholder::Operation => Some(describe_event_kind(event.kind()).to_string()),
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
                .map(|value| value.to_string())
                .unwrap_or_else(|| "0".to_string()),
        ),
        OutFormatPlaceholder::OwnerGid => Some(
            event
                .metadata()
                .and_then(ClientEntryMetadata::gid)
                .map(|value| value.to_string())
                .unwrap_or_else(|| "0".to_string()),
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

fn render_path(event: &ClientEvent, ensure_trailing_slash: bool) -> String {
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
    rendered
}

fn format_numeric_value(value: i64, format: &PlaceholderFormat) -> String {
    match format.humanize() {
        HumanizeMode::None => value.to_string(),
        HumanizeMode::Separator => format_with_separator(value),
        HumanizeMode::DecimalUnits => {
            format_with_units(value, 1000).unwrap_or_else(|| format_with_separator(value))
        }
        HumanizeMode::BinaryUnits => {
            format_with_units(value, 1024).unwrap_or_else(|| format_with_separator(value))
        }
    }
}

fn format_with_units(value: i64, base: i64) -> Option<String> {
    if value.abs() < base {
        return None;
    }

    let mut magnitude = value as f64 / base as f64;
    let negative = magnitude.is_sign_negative();
    if negative {
        magnitude = -magnitude;
    }

    const UNITS: [char; 5] = ['K', 'M', 'G', 'T', 'P'];
    let mut units = 'P';
    for (index, candidate) in UNITS.iter().enumerate() {
        units = *candidate;
        if magnitude < base as f64 || index == UNITS.len() - 1 {
            break;
        }
        magnitude /= base as f64;
    }

    if negative {
        magnitude = -magnitude;
    }

    Some(format!("{magnitude:.2}{units}"))
}

fn format_with_separator(value: i64) -> String {
    let separator = ',';
    let mut magnitude = if value < 0 {
        -(value as i128)
    } else {
        value as i128
    };

    if magnitude == 0 {
        return "0".to_string();
    }

    let mut groups = Vec::new();
    while magnitude > 0 {
        groups.push((magnitude % 1000) as i16);
        magnitude /= 1000;
    }

    let mut rendered = String::new();
    if value < 0 {
        rendered.push('-');
    }

    if let Some(last) = groups.pop() {
        rendered.push_str(&last.to_string());
    }

    for group in groups.iter().rev() {
        rendered.push(separator);
        rendered.push_str(&format!("{group:03}"));
    }

    rendered
}

fn apply_placeholder_format(mut value: String, format: &PlaceholderFormat) -> String {
    if let Some(width) = format.width() {
        let capped_width = width.min(MAX_PLACEHOLDER_WIDTH);
        let len = value.chars().count();
        if len < capped_width {
            let padding = " ".repeat(capped_width - len);
            if format.align() == PlaceholderAlignment::Left {
                value.push_str(&padding);
            } else {
                value.insert_str(0, &padding);
            }
        }
    }

    value
}

fn remote_placeholder_value(value: Option<&str>, token: char) -> String {
    value
        .map(str::to_owned)
        .unwrap_or_else(|| format!("%{token}"))
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

fn resolve_user_name(uid: u32) -> String {
    platform::display_user_name(uid).unwrap_or_else(|| uid.to_string())
}

fn resolve_group_name(gid: u32) -> String {
    platform::display_group_name(gid).unwrap_or_else(|| gid.to_string())
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
        | SkippedMissingDestination
        | SkippedNewerDestination
        | SkippedNonRegular
        | SkippedDirectory
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
            HardLink
            | DataCopied
            | MetadataReused
            | SkippedExisting
            | SkippedMissingDestination
            | SkippedNewerDestination => ClientEntryKind::File,
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
