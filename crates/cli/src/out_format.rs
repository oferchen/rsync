//! `--out-format` parsing and rendering helpers.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::time::SystemTime;

use rsync_checksums::strong::Md5;
use rsync_core::client::{ClientEntryKind, ClientEntryMetadata, ClientEvent, ClientEventKind};
use rsync_core::message::{Message, Role};
use rsync_core::rsync_error;
use time::OffsetDateTime;
use users::{get_group_by_gid, get_user_by_uid, gid_t, uid_t};

use super::defaults::LIST_TIMESTAMP_FORMAT;
use super::{describe_event_kind, format_list_permissions};

/// Parsed representation of an `--out-format` specification.
#[derive(Clone, Debug)]
pub(super) struct OutFormat {
    tokens: Vec<OutFormatToken>,
}

#[derive(Clone, Debug)]
enum OutFormatToken {
    Literal(String),
    Placeholder(OutFormatPlaceholder),
}

#[derive(Clone, Copy, Debug)]
enum OutFormatPlaceholder {
    FileName,
    FileNameWithSymlinkTarget,
    FullPath,
    ItemizedChanges,
    FileLength,
    BytesTransferred,
    ChecksumBytes,
    Operation,
    ModifyTime,
    PermissionString,
    CurrentTime,
    SymlinkTarget,
    OwnerName,
    GroupName,
    OwnerUid,
    OwnerGid,
    ProcessId,
    RemoteHost,
    RemoteAddress,
    ModuleName,
    ModulePath,
    FullChecksum,
}

/// Context values used when rendering `--out-format` placeholders.
#[derive(Clone, Debug, Default)]
pub(super) struct OutFormatContext {
    pub(super) remote_host: Option<String>,
    pub(super) remote_address: Option<String>,
    pub(super) module_name: Option<String>,
    pub(super) module_path: Option<String>,
}

/// Parses a command-line supplied `--out-format` specification into tokens.
pub(super) fn parse_out_format(value: &OsStr) -> Result<OutFormat, Message> {
    let text = value.to_string_lossy();
    if text.is_empty() {
        return Err(rsync_error!(1, "--out-format value must not be empty").with_role(Role::Client));
    }

    let mut tokens = Vec::new();
    let mut literal = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '%' => {
                let Some(next) = chars.next() else {
                    return Err(rsync_error!(1, "--out-format value may not end with '%'")
                        .with_role(Role::Client));
                };
                match next {
                    '%' => literal.push('%'),
                    'n' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::FileName));
                    }
                    'N' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::FileNameWithSymlinkTarget,
                        ));
                    }
                    'f' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::FullPath));
                    }
                    'i' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::ItemizedChanges,
                        ));
                    }
                    'l' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::FileLength,
                        ));
                    }
                    'b' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::BytesTransferred,
                        ));
                    }
                    'c' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::ChecksumBytes,
                        ));
                    }
                    'o' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::Operation));
                    }
                    'M' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::ModifyTime,
                        ));
                    }
                    'B' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::PermissionString,
                        ));
                    }
                    'L' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::SymlinkTarget,
                        ));
                    }
                    't' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::CurrentTime,
                        ));
                    }
                    'u' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::OwnerName));
                    }
                    'g' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::GroupName));
                    }
                    'U' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::OwnerUid));
                    }
                    'G' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::OwnerGid));
                    }
                    'p' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::ProcessId));
                    }
                    'h' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::RemoteHost,
                        ));
                    }
                    'a' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::RemoteAddress,
                        ));
                    }
                    'm' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::ModuleName,
                        ));
                    }
                    'P' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::ModulePath,
                        ));
                    }
                    'C' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::FullChecksum,
                        ));
                    }
                    other => {
                        return Err(rsync_error!(
                            1,
                            format!("unsupported --out-format placeholder '%{other}'")
                        )
                        .with_role(Role::Client));
                    }
                }
            }
            _ => literal.push(ch),
        }
    }

    if !literal.is_empty() {
        tokens.push(OutFormatToken::Literal(literal));
    }

    Ok(OutFormat { tokens })
}

impl OutFormat {
    /// Renders an event according to the parsed `--out-format` tokens.
    pub(super) fn render<W: Write + ?Sized>(
        &self,
        event: &ClientEvent,
        context: &OutFormatContext,
        writer: &mut W,
    ) -> io::Result<()> {
        use std::fmt::Write as _;
        let mut buffer = String::new();
        for token in &self.tokens {
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
                                    | OutFormatPlaceholder::FileNameWithSymlinkTarget
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

    /// Returns `true` when no tokens were parsed from the format string.
    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

/// Emits each event using the supplied `--out-format` specification.
pub(super) fn emit_out_format<W: Write + ?Sized>(
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

fn resolve_user_name(uid: u32) -> String {
    get_user_by_uid(uid as uid_t)
        .map(|user| user.name().to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| uid.to_string())
}

fn resolve_group_name(gid: u32) -> String {
    get_group_by_gid(gid as gid_t)
        .map(|group| group.name().to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| gid.to_string())
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
        ClientEventKind::DataCopied | ClientEventKind::MetadataReused | ClientEventKind::HardLink
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
