#![deny(unsafe_code)]

//! Rendering helpers for parsed `--out-format` specifications.

use std::fmt::Write as FmtWrite;
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::time::SystemTime;

use crate::{describe_event_kind, format_list_permissions, platform, LIST_TIMESTAMP_FORMAT};
use checksums::strong::Md5;
use core::client::{ClientEntryKind, ClientEntryMetadata, ClientEvent, ClientEventKind};
use time::OffsetDateTime;

use super::tokens::{
    HumanizeMode, OutFormat, OutFormatContext, OutFormatPlaceholder, OutFormatToken,
    PlaceholderAlignment, PlaceholderFormat, PlaceholderToken, MAX_PLACEHOLDER_WIDTH,
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
        return "0".to_owned();
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
        // write! to String is infallible
        let _ = write!(rendered, "{group:03}");
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
    value.map_or_else(|| format!("%{token}"), str::to_owned)
}

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

fn format_owner_name(metadata: Option<&ClientEntryMetadata>) -> String {
    metadata
        .and_then(ClientEntryMetadata::uid)
        .map_or_else(|| "0".to_owned(), resolve_user_name)
}

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

fn format_current_timestamp() -> String {
    let now = OffsetDateTime::from(SystemTime::now());
    now.format(LIST_TIMESTAMP_FORMAT).map_or_else(
        |_| "1970/01/01-00:00:00".to_owned(),
        |text| text.replace(' ', "-"),
    )
}

fn format_itemized_changes(event: &ClientEvent) -> String {
    use ClientEventKind::*;

    if matches!(event.kind(), ClientEventKind::EntryDeleted) {
        return "*deleting".to_owned();
    }

    let mut fields = ['.'; 11];

    fields[0] = match event.kind() {
        DataCopied => '>',
        HardLink => 'h',
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

    let change_set = event.change_set();
    let attr = &mut fields[2..];

    if change_set.checksum_changed() {
        attr[0] = 'c';
    }
    if change_set.size_changed() {
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

    fields.iter().collect()
}

fn format_full_checksum(event: &ClientEvent) -> String {
    const EMPTY_CHECKSUM: &str = "                                ";

    if !matches!(
        event.kind(),
        ClientEventKind::DataCopied | ClientEventKind::MetadataReused | ClientEventKind::HardLink,
    ) {
        return EMPTY_CHECKSUM.to_owned();
    }

    if let Some(metadata) = event.metadata()
        && metadata.kind() != ClientEntryKind::File
    {
        return EMPTY_CHECKSUM.to_owned();
    }

    let path = event.destination_path();
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(_) => return EMPTY_CHECKSUM.to_owned(),
    };

    let mut hasher = Md5::new();
    let mut buffer = vec![0u8; 32 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buffer[..read]),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return EMPTY_CHECKSUM.to_owned(),
        }
    }

    let digest = hasher.finalize();
    let mut rendered = String::with_capacity(32);
    for byte in digest {
        // write! to String is infallible
        let _ = write!(rendered, "{byte:02x}");
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_with_separator_zero() {
        assert_eq!(format_with_separator(0), "0");
    }

    #[test]
    fn format_with_separator_small() {
        assert_eq!(format_with_separator(1), "1");
        assert_eq!(format_with_separator(999), "999");
    }

    #[test]
    fn format_with_separator_thousands() {
        assert_eq!(format_with_separator(1000), "1,000");
        assert_eq!(format_with_separator(1234), "1,234");
        assert_eq!(format_with_separator(999999), "999,999");
    }

    #[test]
    fn format_with_separator_millions() {
        assert_eq!(format_with_separator(1_000_000), "1,000,000");
        assert_eq!(format_with_separator(1_234_567), "1,234,567");
    }

    #[test]
    fn format_with_separator_billions() {
        assert_eq!(format_with_separator(1_000_000_000), "1,000,000,000");
    }

    #[test]
    fn format_with_separator_negative() {
        assert_eq!(format_with_separator(-1), "-1");
        assert_eq!(format_with_separator(-999), "-999");
        assert_eq!(format_with_separator(-1000), "-1,000");
        assert_eq!(format_with_separator(-1_234_567), "-1,234,567");
    }

    #[test]
    fn format_with_units_below_base() {
        assert_eq!(format_with_units(999, 1000), None);
        assert_eq!(format_with_units(1023, 1024), None);
    }

    #[test]
    fn format_with_units_decimal_kilo() {
        assert_eq!(format_with_units(1000, 1000), Some("1.00K".to_owned()));
        assert_eq!(format_with_units(1500, 1000), Some("1.50K".to_owned()));
        assert_eq!(
            format_with_units(999_999, 1000),
            Some("1000.00K".to_owned())
        );
    }

    #[test]
    fn format_with_units_binary_kilo() {
        assert_eq!(format_with_units(1024, 1024), Some("1.00K".to_owned()));
        assert_eq!(format_with_units(1536, 1024), Some("1.50K".to_owned()));
    }

    #[test]
    fn format_with_units_decimal_mega() {
        assert_eq!(format_with_units(1_000_000, 1000), Some("1.00M".to_owned()));
        assert_eq!(format_with_units(2_500_000, 1000), Some("2.50M".to_owned()));
    }

    #[test]
    fn format_with_units_binary_mega() {
        assert_eq!(format_with_units(1_048_576, 1024), Some("1.00M".to_owned()));
    }

    #[test]
    fn format_with_units_giga() {
        assert_eq!(
            format_with_units(1_000_000_000, 1000),
            Some("1.00G".to_owned())
        );
        assert_eq!(
            format_with_units(1_073_741_824, 1024),
            Some("1.00G".to_owned())
        );
    }

    #[test]
    fn format_with_units_tera() {
        assert_eq!(
            format_with_units(1_000_000_000_000, 1000),
            Some("1.00T".to_owned())
        );
    }

    #[test]
    fn format_with_units_negative() {
        assert_eq!(format_with_units(-1000, 1000), Some("-1.00K".to_owned()));
        assert_eq!(
            format_with_units(-1_000_000, 1000),
            Some("-1.00M".to_owned())
        );
    }

    #[test]
    fn apply_placeholder_format_no_width() {
        let format = PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::None);
        assert_eq!(apply_placeholder_format("test".to_owned(), &format), "test");
    }

    #[test]
    fn apply_placeholder_format_right_align() {
        let format =
            PlaceholderFormat::new(Some(10), PlaceholderAlignment::Right, HumanizeMode::None);
        assert_eq!(
            apply_placeholder_format("test".to_owned(), &format),
            "      test"
        );
    }

    #[test]
    fn apply_placeholder_format_left_align() {
        let format =
            PlaceholderFormat::new(Some(10), PlaceholderAlignment::Left, HumanizeMode::None);
        assert_eq!(
            apply_placeholder_format("test".to_owned(), &format),
            "test      "
        );
    }

    #[test]
    fn apply_placeholder_format_exact_width() {
        let format =
            PlaceholderFormat::new(Some(4), PlaceholderAlignment::Right, HumanizeMode::None);
        assert_eq!(apply_placeholder_format("test".to_owned(), &format), "test");
    }

    #[test]
    fn apply_placeholder_format_exceed_width() {
        let format =
            PlaceholderFormat::new(Some(2), PlaceholderAlignment::Right, HumanizeMode::None);
        assert_eq!(apply_placeholder_format("test".to_owned(), &format), "test");
    }

    #[test]
    fn apply_placeholder_format_max_width_capped() {
        // Width is capped to MAX_PLACEHOLDER_WIDTH
        let format = PlaceholderFormat::new(
            Some(MAX_PLACEHOLDER_WIDTH + 100),
            PlaceholderAlignment::Right,
            HumanizeMode::None,
        );
        let result = apply_placeholder_format("x".to_owned(), &format);
        assert_eq!(result.len(), MAX_PLACEHOLDER_WIDTH);
    }

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
    fn format_numeric_value_plain() {
        let format = PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::None);
        assert_eq!(format_numeric_value(12345, &format), "12345");
    }

    #[test]
    fn format_numeric_value_with_separator() {
        let format =
            PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::Separator);
        assert_eq!(format_numeric_value(1234567, &format), "1,234,567");
    }

    #[test]
    fn format_numeric_value_decimal_units() {
        let format = PlaceholderFormat::new(
            None,
            PlaceholderAlignment::Right,
            HumanizeMode::DecimalUnits,
        );
        assert_eq!(format_numeric_value(1000, &format), "1.00K");
        assert_eq!(format_numeric_value(999, &format), "999");
    }

    #[test]
    fn format_numeric_value_binary_units() {
        let format =
            PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::BinaryUnits);
        assert_eq!(format_numeric_value(1024, &format), "1.00K");
        assert_eq!(format_numeric_value(1023, &format), "1,023");
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

    // ==================== format_itemized_changes tests ====================
    //
    // Upstream rsync --itemize-changes format reference:
    //   YXcstpoguax  filename
    //   ^^ ^^^^^^^^^ (11 characters total)
    //   ||
    //   |+-- X = file type: f (file), d (directory), L (symlink), D (device), S (special)
    //   +--- Y = update type: > (received), c (created), h (hardlink), . (not updated), * (message)
    //
    //   Positions 2-10 (c s t p o g u a x):
    //     '.' = attribute is unchanged
    //     '+' = file is new (all attributes are new)
    //     letter = attribute changed (c/s/t/T/p/o/g/u/n/b/a/x)

    use core::client::{ClientEntryKind, ClientEventKind};
    use engine::local_copy::{LocalCopyChangeSet, TimeChange};
    use std::path::PathBuf;

    fn make_event(
        kind: ClientEventKind,
        created: bool,
        metadata_kind: Option<ClientEntryKind>,
        change_set: LocalCopyChangeSet,
    ) -> ClientEvent {
        let metadata = metadata_kind.map(ClientEvent::test_metadata);
        ClientEvent::for_test(
            PathBuf::from("test.txt"),
            kind,
            created,
            metadata,
            change_set,
        )
    }

    // ---- Format length ----

    #[test]
    fn itemize_format_length_is_eleven_for_new_file() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.len(),
            11,
            "format string should be 11 characters: {result:?}"
        );
    }

    #[test]
    fn itemize_format_length_is_eleven_for_unchanged_file() {
        let event = make_event(
            ClientEventKind::MetadataReused,
            false,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.len(),
            11,
            "format string should be 11 characters: {result:?}"
        );
    }

    #[test]
    fn itemize_format_length_is_eleven_for_modified_file() {
        let cs = LocalCopyChangeSet::new()
            .with_checksum_changed(true)
            .with_size_changed(true)
            .with_time_change(Some(TimeChange::Modified));
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.len(),
            11,
            "format string should be 11 characters: {result:?}"
        );
    }

    // ---- Y (position 0): update type character ----

    #[test]
    fn itemize_y_position_data_copied_shows_greater_than() {
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(0),
            Some('>'),
            "Y should be '>' for DataCopied: {result:?}"
        );
    }

    #[test]
    fn itemize_y_position_hardlink_shows_h() {
        let event = make_event(
            ClientEventKind::HardLink,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(0),
            Some('h'),
            "Y should be 'h' for HardLink: {result:?}"
        );
    }

    #[test]
    fn itemize_y_position_directory_created_shows_c() {
        let event = make_event(
            ClientEventKind::DirectoryCreated,
            true,
            Some(ClientEntryKind::Directory),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(0),
            Some('c'),
            "Y should be 'c' for DirectoryCreated: {result:?}"
        );
    }

    #[test]
    fn itemize_y_position_symlink_copied_shows_c() {
        let event = make_event(
            ClientEventKind::SymlinkCopied,
            true,
            Some(ClientEntryKind::Symlink),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(0),
            Some('c'),
            "Y should be 'c' for SymlinkCopied: {result:?}"
        );
    }

    #[test]
    fn itemize_y_position_fifo_copied_shows_c() {
        let event = make_event(
            ClientEventKind::FifoCopied,
            true,
            Some(ClientEntryKind::Fifo),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(0),
            Some('c'),
            "Y should be 'c' for FifoCopied: {result:?}"
        );
    }

    #[test]
    fn itemize_y_position_device_copied_shows_c() {
        let event = make_event(
            ClientEventKind::DeviceCopied,
            true,
            Some(ClientEntryKind::CharDevice),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(0),
            Some('c'),
            "Y should be 'c' for DeviceCopied: {result:?}"
        );
    }

    #[test]
    fn itemize_y_position_metadata_reused_shows_dot() {
        let event = make_event(
            ClientEventKind::MetadataReused,
            false,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(0),
            Some('.'),
            "Y should be '.' for MetadataReused: {result:?}"
        );
    }

    #[test]
    fn itemize_y_position_skipped_existing_shows_dot() {
        let event = make_event(
            ClientEventKind::SkippedExisting,
            false,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(0),
            Some('.'),
            "Y should be '.' for SkippedExisting: {result:?}"
        );
    }

    #[test]
    fn itemize_y_position_source_removed_shows_c() {
        let event = make_event(
            ClientEventKind::SourceRemoved,
            false,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(0),
            Some('c'),
            "Y should be 'c' for SourceRemoved: {result:?}"
        );
    }

    // ---- X (position 1): file type character ----

    #[test]
    fn itemize_x_position_file_shows_f() {
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(1),
            Some('f'),
            "X should be 'f' for File: {result:?}"
        );
    }

    #[test]
    fn itemize_x_position_directory_shows_d() {
        let event = make_event(
            ClientEventKind::DirectoryCreated,
            true,
            Some(ClientEntryKind::Directory),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(1),
            Some('d'),
            "X should be 'd' for Directory: {result:?}"
        );
    }

    #[test]
    fn itemize_x_position_symlink_shows_l() {
        let event = make_event(
            ClientEventKind::SymlinkCopied,
            true,
            Some(ClientEntryKind::Symlink),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(1),
            Some('L'),
            "X should be 'L' for Symlink: {result:?}"
        );
    }

    #[test]
    fn itemize_x_position_char_device_shows_d_upper() {
        let event = make_event(
            ClientEventKind::DeviceCopied,
            true,
            Some(ClientEntryKind::CharDevice),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(1),
            Some('D'),
            "X should be 'D' for CharDevice: {result:?}"
        );
    }

    #[test]
    fn itemize_x_position_block_device_shows_d_upper() {
        let event = make_event(
            ClientEventKind::DeviceCopied,
            true,
            Some(ClientEntryKind::BlockDevice),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(1),
            Some('D'),
            "X should be 'D' for BlockDevice: {result:?}"
        );
    }

    #[test]
    fn itemize_x_position_fifo_shows_s_upper() {
        let event = make_event(
            ClientEventKind::FifoCopied,
            true,
            Some(ClientEntryKind::Fifo),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(1),
            Some('S'),
            "X should be 'S' for Fifo: {result:?}"
        );
    }

    #[test]
    fn itemize_x_position_socket_shows_s_upper() {
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::Socket),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(1),
            Some('S'),
            "X should be 'S' for Socket: {result:?}"
        );
    }

    // ---- New file: all attributes show '+' (positions 2-10) ----

    #[test]
    fn itemize_new_file_shows_all_plus_in_attribute_positions() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result, ">f+++++++++",
            "new file should be >f+++++++++: {result:?}"
        );
    }

    #[test]
    fn itemize_new_directory_shows_all_plus_in_attribute_positions() {
        let event = make_event(
            ClientEventKind::DirectoryCreated,
            true,
            Some(ClientEntryKind::Directory),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result, "cd+++++++++",
            "new directory should be cd+++++++++: {result:?}"
        );
    }

    #[test]
    fn itemize_new_symlink_shows_all_plus_in_attribute_positions() {
        let event = make_event(
            ClientEventKind::SymlinkCopied,
            true,
            Some(ClientEntryKind::Symlink),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result, "cL+++++++++",
            "new symlink should be cL+++++++++: {result:?}"
        );
    }

    #[test]
    fn itemize_new_device_shows_all_plus_in_attribute_positions() {
        let event = make_event(
            ClientEventKind::DeviceCopied,
            true,
            Some(ClientEntryKind::CharDevice),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result, "cD+++++++++",
            "new device should be cD+++++++++: {result:?}"
        );
    }

    #[test]
    fn itemize_new_fifo_shows_all_plus_in_attribute_positions() {
        let event = make_event(
            ClientEventKind::FifoCopied,
            true,
            Some(ClientEntryKind::Fifo),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result, "cS+++++++++",
            "new fifo should be cS+++++++++: {result:?}"
        );
    }

    #[test]
    fn itemize_hardlink_shows_all_plus_in_attribute_positions() {
        let event = make_event(
            ClientEventKind::HardLink,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result, "hf+++++++++",
            "new hardlink should be hf+++++++++: {result:?}"
        );
    }

    // ---- Delete format ----

    #[test]
    fn itemize_deleted_entry_shows_star_deleting() {
        let event = make_event(
            ClientEventKind::EntryDeleted,
            false,
            None,
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result, "*deleting",
            "deleted entry should be '*deleting': {result:?}"
        );
    }

    // ---- Individual attribute positions for changed files ----

    #[test]
    fn itemize_checksum_changed_shows_c_at_position_2() {
        let cs = LocalCopyChangeSet::new().with_checksum_changed(true);
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(2),
            Some('c'),
            "position 2 should be 'c' for checksum: {result:?}"
        );
    }

    #[test]
    fn itemize_size_changed_shows_s_at_position_3() {
        let cs = LocalCopyChangeSet::new().with_size_changed(true);
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(3),
            Some('s'),
            "position 3 should be 's' for size: {result:?}"
        );
    }

    #[test]
    fn itemize_time_modified_shows_lowercase_t_at_position_4() {
        let cs = LocalCopyChangeSet::new().with_time_change(Some(TimeChange::Modified));
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(4),
            Some('t'),
            "position 4 should be 't' for Modified time: {result:?}"
        );
    }

    #[test]
    fn itemize_time_transfer_shows_uppercase_t_at_position_4() {
        let cs = LocalCopyChangeSet::new().with_time_change(Some(TimeChange::TransferTime));
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(4),
            Some('T'),
            "position 4 should be 'T' for TransferTime: {result:?}"
        );
    }

    #[test]
    fn itemize_permissions_changed_shows_p_at_position_5() {
        let cs = LocalCopyChangeSet::new().with_permissions_changed(true);
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(5),
            Some('p'),
            "position 5 should be 'p' for permissions: {result:?}"
        );
    }

    #[test]
    fn itemize_owner_changed_shows_o_at_position_6() {
        let cs = LocalCopyChangeSet::new().with_owner_changed(true);
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(6),
            Some('o'),
            "position 6 should be 'o' for owner: {result:?}"
        );
    }

    #[test]
    fn itemize_group_changed_shows_g_at_position_7() {
        let cs = LocalCopyChangeSet::new().with_group_changed(true);
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(7),
            Some('g'),
            "position 7 should be 'g' for group: {result:?}"
        );
    }

    #[test]
    fn itemize_access_time_changed_shows_u_at_position_8() {
        let cs = LocalCopyChangeSet::new().with_access_time_changed(true);
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(8),
            Some('u'),
            "position 8 should be 'u' for access time: {result:?}"
        );
    }

    #[test]
    fn itemize_create_time_changed_shows_n_at_position_8() {
        let cs = LocalCopyChangeSet::new().with_create_time_changed(true);
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(8),
            Some('n'),
            "position 8 should be 'n' for create time: {result:?}"
        );
    }

    #[test]
    fn itemize_both_access_and_create_time_changed_shows_b_at_position_8() {
        let cs = LocalCopyChangeSet::new()
            .with_access_time_changed(true)
            .with_create_time_changed(true);
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(8),
            Some('b'),
            "position 8 should be 'b' for both times: {result:?}"
        );
    }

    #[test]
    fn itemize_acl_changed_shows_a_at_position_9() {
        let cs = LocalCopyChangeSet::new().with_acl_changed(true);
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(9),
            Some('a'),
            "position 9 should be 'a' for ACL: {result:?}"
        );
    }

    #[test]
    fn itemize_xattr_changed_shows_x_at_position_10() {
        let cs = LocalCopyChangeSet::new().with_xattr_changed(true);
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(10),
            Some('x'),
            "position 10 should be 'x' for xattr: {result:?}"
        );
    }

    // ---- No change shows dots for all attributes ----

    #[test]
    fn itemize_no_changes_shows_all_dots_in_attribute_positions() {
        let event = make_event(
            ClientEventKind::MetadataReused,
            false,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result, ".f.........",
            "no change should show all dots: {result:?}"
        );
    }

    // ---- Combined changes ----

    #[test]
    fn itemize_checksum_and_size_change_shows_cs_at_positions_2_3() {
        let cs = LocalCopyChangeSet::new()
            .with_checksum_changed(true)
            .with_size_changed(true);
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            &result[..4],
            ">fcs",
            "should show '>fcs' for checksum+size: {result:?}"
        );
    }

    #[test]
    fn itemize_full_change_shows_all_indicators() {
        let cs = LocalCopyChangeSet::new()
            .with_checksum_changed(true)
            .with_size_changed(true)
            .with_time_change(Some(TimeChange::Modified))
            .with_permissions_changed(true)
            .with_owner_changed(true)
            .with_group_changed(true)
            .with_access_time_changed(true)
            .with_create_time_changed(true)
            .with_acl_changed(true)
            .with_xattr_changed(true);
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result, ">fcstpogbax",
            "full changes should show all indicators: {result:?}"
        );
    }

    #[test]
    fn itemize_typical_content_update_shows_cst_pattern() {
        let cs = LocalCopyChangeSet::new()
            .with_checksum_changed(true)
            .with_size_changed(true)
            .with_time_change(Some(TimeChange::Modified));
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result, ">fcst......",
            "typical update should show '>fcst......': {result:?}"
        );
    }

    #[test]
    fn itemize_directory_timestamp_update_shows_dot_d_dot_t_pattern() {
        let cs = LocalCopyChangeSet::new().with_time_change(Some(TimeChange::Modified));
        let event = make_event(
            ClientEventKind::DirectoryCreated,
            false,
            Some(ClientEntryKind::Directory),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result, "cd..t......",
            "directory time update should show 'cd..t......': {result:?}"
        );
    }

    #[test]
    fn itemize_permission_only_change() {
        let cs = LocalCopyChangeSet::new().with_permissions_changed(true);
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(result, ">f...p.....", "permission-only change: {result:?}");
    }

    #[test]
    fn itemize_owner_and_group_change() {
        let cs = LocalCopyChangeSet::new()
            .with_owner_changed(true)
            .with_group_changed(true);
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(result, ">f....og...", "owner+group change: {result:?}");
    }

    // ---- File type inference when metadata is None ----

    #[test]
    fn itemize_infers_file_type_from_data_copied_when_no_metadata() {
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            None,
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(1),
            Some('f'),
            "should infer 'f' for DataCopied: {result:?}"
        );
    }

    #[test]
    fn itemize_infers_directory_type_from_directory_created_when_no_metadata() {
        let event = make_event(
            ClientEventKind::DirectoryCreated,
            true,
            None,
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(1),
            Some('d'),
            "should infer 'd' for DirectoryCreated: {result:?}"
        );
    }

    #[test]
    fn itemize_infers_symlink_type_from_symlink_copied_when_no_metadata() {
        let event = make_event(
            ClientEventKind::SymlinkCopied,
            true,
            None,
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(1),
            Some('L'),
            "should infer 'L' for SymlinkCopied: {result:?}"
        );
    }

    #[test]
    fn itemize_infers_fifo_type_from_fifo_copied_when_no_metadata() {
        let event = make_event(
            ClientEventKind::FifoCopied,
            true,
            None,
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(1),
            Some('S'),
            "should infer 'S' for FifoCopied: {result:?}"
        );
    }

    #[test]
    fn itemize_infers_device_type_from_device_copied_when_no_metadata() {
        let event = make_event(
            ClientEventKind::DeviceCopied,
            true,
            None,
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(1),
            Some('D'),
            "should infer 'D' for DeviceCopied: {result:?}"
        );
    }

    // ---- Skipped variations show dot as Y ----

    #[test]
    fn itemize_skipped_missing_destination_shows_dot() {
        let event = make_event(
            ClientEventKind::SkippedMissingDestination,
            false,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(0),
            Some('.'),
            "SkippedMissingDestination should be '.': {result:?}"
        );
    }

    #[test]
    fn itemize_skipped_newer_destination_shows_dot() {
        let event = make_event(
            ClientEventKind::SkippedNewerDestination,
            false,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(0),
            Some('.'),
            "SkippedNewerDestination should be '.': {result:?}"
        );
    }

    #[test]
    fn itemize_skipped_non_regular_shows_dot() {
        let event = make_event(
            ClientEventKind::SkippedNonRegular,
            false,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(0),
            Some('.'),
            "SkippedNonRegular should be '.': {result:?}"
        );
    }

    #[test]
    fn itemize_skipped_directory_shows_dot() {
        let event = make_event(
            ClientEventKind::SkippedDirectory,
            false,
            Some(ClientEntryKind::Directory),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(0),
            Some('.'),
            "SkippedDirectory should be '.': {result:?}"
        );
    }

    #[test]
    fn itemize_skipped_unsafe_symlink_shows_dot() {
        let event = make_event(
            ClientEventKind::SkippedUnsafeSymlink,
            false,
            Some(ClientEntryKind::Symlink),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(0),
            Some('.'),
            "SkippedUnsafeSymlink should be '.': {result:?}"
        );
    }

    #[test]
    fn itemize_skipped_mount_point_shows_dot() {
        let event = make_event(
            ClientEventKind::SkippedMountPoint,
            false,
            Some(ClientEntryKind::Directory),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(0),
            Some('.'),
            "SkippedMountPoint should be '.': {result:?}"
        );
    }

    // ---- Upstream format strings: complete patterns ----

    #[test]
    fn itemize_upstream_new_regular_file_pattern() {
        // Upstream rsync: >f+++++++++ for a brand new regular file
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        assert_eq!(format_itemized_changes(&event), ">f+++++++++");
    }

    #[test]
    fn itemize_upstream_new_directory_pattern() {
        // Upstream rsync: cd+++++++++ for a new directory
        let event = make_event(
            ClientEventKind::DirectoryCreated,
            true,
            Some(ClientEntryKind::Directory),
            LocalCopyChangeSet::new(),
        );
        assert_eq!(format_itemized_changes(&event), "cd+++++++++");
    }

    #[test]
    fn itemize_upstream_new_symlink_pattern() {
        // Upstream rsync: cL+++++++++ for a new symlink
        let event = make_event(
            ClientEventKind::SymlinkCopied,
            true,
            Some(ClientEntryKind::Symlink),
            LocalCopyChangeSet::new(),
        );
        assert_eq!(format_itemized_changes(&event), "cL+++++++++");
    }

    #[test]
    fn itemize_upstream_delete_pattern() {
        // Upstream rsync: *deleting for deleted entries
        let event = make_event(
            ClientEventKind::EntryDeleted,
            false,
            None,
            LocalCopyChangeSet::new(),
        );
        assert_eq!(format_itemized_changes(&event), "*deleting");
    }

    #[test]
    fn itemize_upstream_unchanged_file_pattern() {
        // Upstream rsync: .f......... for an unchanged file
        let event = make_event(
            ClientEventKind::MetadataReused,
            false,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        assert_eq!(format_itemized_changes(&event), ".f.........");
    }

    #[test]
    fn itemize_upstream_content_and_time_update_pattern() {
        // Upstream rsync: >fcst...... for a file with content+size+time changes
        let cs = LocalCopyChangeSet::new()
            .with_checksum_changed(true)
            .with_size_changed(true)
            .with_time_change(Some(TimeChange::Modified));
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        assert_eq!(format_itemized_changes(&event), ">fcst......");
    }

    #[test]
    fn itemize_upstream_time_only_update_pattern() {
        // Upstream rsync: >f..t...... for a file with only time change
        let cs = LocalCopyChangeSet::new().with_time_change(Some(TimeChange::Modified));
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        assert_eq!(format_itemized_changes(&event), ">f..t......");
    }

    #[test]
    fn itemize_upstream_transfer_time_pattern() {
        // Upstream rsync: >f..T...... when times not preserved (capital T)
        let cs = LocalCopyChangeSet::new().with_time_change(Some(TimeChange::TransferTime));
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            cs,
        );
        assert_eq!(format_itemized_changes(&event), ">f..T......");
    }

    // ---- Edge cases ----

    #[test]
    fn itemize_new_file_ignores_change_set_values() {
        // When created=true, all positions 2-10 should be '+' regardless of change_set
        let cs = LocalCopyChangeSet::new()
            .with_checksum_changed(true)
            .with_size_changed(true)
            .with_permissions_changed(true);
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            cs,
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result, ">f+++++++++",
            "created=true should override change_set with '+': {result:?}"
        );
    }

    #[test]
    fn itemize_position_8_no_time_change_shows_dot() {
        // When neither access_time nor create_time changed, position 8 should be '.'
        let event = make_event(
            ClientEventKind::DataCopied,
            false,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(
            result.chars().nth(8),
            Some('.'),
            "position 8 should be '.' with no time changes: {result:?}"
        );
    }

    #[test]
    fn itemize_delete_does_not_have_eleven_char_length() {
        // *deleting is a special case -- it is 9 characters, not 11
        let event = make_event(
            ClientEventKind::EntryDeleted,
            false,
            None,
            LocalCopyChangeSet::new(),
        );
        let result = format_itemized_changes(&event);
        assert_eq!(result, "*deleting");
        assert_eq!(result.len(), 9, "delete format should be 9 characters");
    }

    // ===========================================================================
    // End-to-end render tests: parse + render + verify output
    // ===========================================================================

    use super::super::parser::parse_out_format;

    fn render_format(format_str: &str, event: &ClientEvent) -> String {
        let format = parse_out_format(std::ffi::OsStr::new(format_str)).unwrap();
        let mut output = Vec::new();
        format
            .render(event, &OutFormatContext::default(), &mut output)
            .unwrap();
        String::from_utf8(output).unwrap()
    }

    fn render_format_with_context(
        format_str: &str,
        event: &ClientEvent,
        context: &OutFormatContext,
    ) -> String {
        let format = parse_out_format(std::ffi::OsStr::new(format_str)).unwrap();
        let mut output = Vec::new();
        format.render(event, context, &mut output).unwrap();
        String::from_utf8(output).unwrap()
    }

    // ---- %n renders filename ----

    #[test]
    fn render_percent_n_shows_filename() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        assert_eq!(render_format("%n", &event), "test.txt\n");
    }

    // ---- %n adds trailing slash for directories ----

    #[test]
    fn render_percent_n_adds_trailing_slash_for_directory() {
        let metadata = Some(ClientEvent::test_metadata(ClientEntryKind::Directory));
        let event = ClientEvent::for_test(
            PathBuf::from("mydir"),
            ClientEventKind::DirectoryCreated,
            true,
            metadata,
            LocalCopyChangeSet::new(),
        );
        assert_eq!(render_format("%n", &event), "mydir/\n");
    }

    // ---- %f does NOT add trailing slash for directories (full path) ----

    #[test]
    fn render_percent_f_no_trailing_slash_for_directory() {
        let metadata = Some(ClientEvent::test_metadata(ClientEntryKind::Directory));
        let event = ClientEvent::for_test(
            PathBuf::from("mydir"),
            ClientEventKind::DirectoryCreated,
            true,
            metadata,
            LocalCopyChangeSet::new(),
        );
        let rendered = render_format("%f", &event);
        // %f uses render_path(event, false) so should not add trailing slash
        assert!(
            !rendered.trim().ends_with('/'),
            "%%f should not add trailing slash: {rendered:?}"
        );
    }

    // ---- %l shows file length ----

    #[test]
    fn render_percent_l_shows_file_length() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        // test_metadata sets length to 0
        assert_eq!(render_format("%l", &event), "0\n");
    }

    // ---- %b shows bytes transferred (0 for test events) ----

    #[test]
    fn render_percent_b_shows_bytes_transferred() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        // for_test always sets bytes_transferred to 0
        assert_eq!(render_format("%b", &event), "0\n");
    }

    // ---- %o shows operation description ----

    #[test]
    fn render_percent_o_shows_operation() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        assert_eq!(render_format("%o", &event), "copied\n");
    }

    #[test]
    fn render_percent_o_shows_deleted_for_entry_deleted() {
        let event = make_event(
            ClientEventKind::EntryDeleted,
            false,
            None,
            LocalCopyChangeSet::new(),
        );
        assert_eq!(render_format("%o", &event), "deleted\n");
    }

    // ---- %p shows process ID ----

    #[test]
    fn render_percent_p_shows_current_pid() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let rendered = render_format("%p", &event);
        let expected = format!("{}\n", std::process::id());
        assert_eq!(rendered, expected);
    }

    // ---- %t shows current timestamp in correct format ----

    #[test]
    fn render_percent_t_shows_timestamp_in_expected_format() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let rendered = render_format("%t", &event);
        let trimmed = rendered.trim();
        // Upstream format: yyyy/mm/dd-hh:mm:ss
        assert!(
            trimmed.len() == 19,
            "%%t should be 19 chars (yyyy/mm/dd-hh:mm:ss), got {trimmed:?}"
        );
        assert_eq!(&trimmed[4..5], "/", "position 4 should be '/'");
        assert_eq!(&trimmed[7..8], "/", "position 7 should be '/'");
        assert_eq!(&trimmed[10..11], "-", "position 10 should be '-'");
        assert_eq!(&trimmed[13..14], ":", "position 13 should be ':'");
        assert_eq!(&trimmed[16..17], ":", "position 16 should be ':'");
    }

    // ---- %M shows modification time (epoch default when no metadata) ----

    #[test]
    fn render_percent_m_shows_epoch_when_no_mtime() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        assert_eq!(render_format("%M", &event), "1970/01/01-00:00:00\n");
    }

    // ---- %U and %G show uid/gid (0 when no metadata uid/gid) ----

    #[test]
    fn render_percent_u_upper_shows_uid_zero_for_test_metadata() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        assert_eq!(render_format("%U", &event), "0\n");
    }

    #[test]
    fn render_percent_g_upper_shows_gid_zero_for_test_metadata() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        assert_eq!(render_format("%G", &event), "0\n");
    }

    // ---- %B shows permissions (dashes when no mode) ----

    #[test]
    fn render_percent_b_upper_shows_dashes_for_test_metadata() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        // test_metadata has mode = None => "---------"
        assert_eq!(render_format("%B", &event), "---------\n");
    }

    // ---- %L shows nothing for non-symlink files ----

    #[test]
    fn render_percent_l_upper_shows_nothing_for_regular_file() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        // SymlinkTarget returns None for non-symlinks, so nothing is rendered
        // But since it returns None, the placeholder is omitted entirely
        // and the output is just the newline
        assert_eq!(render_format("%L", &event), "\n");
    }

    // ---- %i%n combined upstream pattern ----

    #[test]
    fn render_itemize_and_filename_combined_no_space() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        // %i%n with no space means the two are concatenated directly
        assert_eq!(render_format("%i%n", &event), ">f+++++++++test.txt\n");
    }

    #[test]
    fn render_itemize_and_filename_with_space() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        // %i %n with a space between them matches upstream -i output
        assert_eq!(render_format("%i %n", &event), ">f+++++++++ test.txt\n");
    }

    // ---- %% literal percent ----

    #[test]
    fn render_escaped_percent_produces_literal_percent() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        assert_eq!(render_format("%%", &event), "%\n");
    }

    #[test]
    fn render_literal_text_with_escaped_percent() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        assert_eq!(render_format("100%%", &event), "100%\n");
    }

    // ---- Multiple codes in one format string ----

    #[test]
    fn render_multiple_codes_in_format_string() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let rendered = render_format("[%n] %b bytes %o", &event);
        assert_eq!(rendered, "[test.txt] 0 bytes copied\n");
    }

    // ---- Literal text mixed with codes ----

    #[test]
    fn render_literal_prefix_and_suffix_around_placeholder() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let rendered = render_format("<<<%n>>>", &event);
        assert_eq!(rendered, "<<<test.txt>>>\n");
    }

    // ---- Remote context placeholders with populated context ----

    #[test]
    fn render_remote_host_with_context_populated() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let context = OutFormatContext {
            remote_host: Some("server.example.com".to_owned()),
            remote_address: Some("10.0.0.1".to_owned()),
            module_name: Some("backup".to_owned()),
            module_path: Some("/var/backup".to_owned()),
        };
        assert_eq!(
            render_format_with_context("%h", &event, &context),
            "server.example.com\n"
        );
    }

    #[test]
    fn render_remote_address_with_context_populated() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let context = OutFormatContext {
            remote_host: None,
            remote_address: Some("192.168.1.100".to_owned()),
            module_name: None,
            module_path: None,
        };
        assert_eq!(
            render_format_with_context("%a", &event, &context),
            "192.168.1.100\n"
        );
    }

    #[test]
    fn render_module_name_with_context_populated() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let context = OutFormatContext {
            remote_host: None,
            remote_address: None,
            module_name: Some("data".to_owned()),
            module_path: None,
        };
        assert_eq!(render_format_with_context("%m", &event, &context), "data\n");
    }

    #[test]
    fn render_module_path_with_context_populated() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let context = OutFormatContext {
            remote_host: None,
            remote_address: None,
            module_name: None,
            module_path: Some("/srv/data".to_owned()),
        };
        assert_eq!(
            render_format_with_context("%P", &event, &context),
            "/srv/data\n"
        );
    }

    #[test]
    fn render_all_remote_placeholders_with_full_context() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let context = OutFormatContext {
            remote_host: Some("host".to_owned()),
            remote_address: Some("addr".to_owned()),
            module_name: Some("mod".to_owned()),
            module_path: Some("/path".to_owned()),
        };
        let rendered = render_format_with_context("%h %a %m %P", &event, &context);
        assert_eq!(rendered, "host addr mod /path\n");
    }

    // ---- emit_out_format with multiple events ----

    #[test]
    fn emit_out_format_renders_multiple_events() {
        let event1 = ClientEvent::for_test(
            PathBuf::from("alpha.txt"),
            ClientEventKind::DataCopied,
            true,
            Some(ClientEvent::test_metadata(ClientEntryKind::File)),
            LocalCopyChangeSet::new(),
        );
        let event2 = ClientEvent::for_test(
            PathBuf::from("beta.txt"),
            ClientEventKind::DataCopied,
            true,
            Some(ClientEvent::test_metadata(ClientEntryKind::File)),
            LocalCopyChangeSet::new(),
        );
        let events = [event1, event2];
        let format = parse_out_format(std::ffi::OsStr::new("%n")).unwrap();
        let mut output = Vec::new();
        emit_out_format(&events, &format, &OutFormatContext::default(), &mut output).unwrap();
        let rendered = String::from_utf8(output).unwrap();
        assert_eq!(rendered, "alpha.txt\nbeta.txt\n");
    }

    #[test]
    fn emit_out_format_renders_empty_event_list() {
        let events: &[ClientEvent] = &[];
        let format = parse_out_format(std::ffi::OsStr::new("%n")).unwrap();
        let mut output = Vec::new();
        emit_out_format(events, &format, &OutFormatContext::default(), &mut output).unwrap();
        assert!(output.is_empty());
    }

    // ---- Output always ends with newline ----

    #[test]
    fn render_output_always_ends_with_newline() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let rendered = render_format("no-newline-in-format", &event);
        assert!(rendered.ends_with('\n'));
    }

    #[test]
    fn render_output_does_not_double_newline_when_format_ends_with_newline() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        // The render function checks if the buffer already ends with '\n'
        // and doesn't add another one.
        let format = parse_out_format(std::ffi::OsStr::new("%n")).unwrap();
        let mut output = Vec::new();
        format
            .render(&event, &OutFormatContext::default(), &mut output)
            .unwrap();
        let rendered = String::from_utf8(output).unwrap();
        // The output should end with exactly one newline
        assert!(rendered.ends_with('\n'));
        assert!(!rendered.ends_with("\n\n"));
    }

    // ---- Width formatting with different placeholders ----

    #[test]
    fn render_width_right_aligned_filename() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let rendered = render_format("%20n", &event);
        let trimmed = rendered.trim_end_matches('\n');
        // "test.txt" is 8 chars, padded to 20 right-aligned
        assert_eq!(trimmed.len(), 20);
        assert!(trimmed.ends_with("test.txt"));
        assert!(trimmed.starts_with("            ")); // 12 spaces
    }

    #[test]
    fn render_width_left_aligned_filename() {
        let event = make_event(
            ClientEventKind::DataCopied,
            true,
            Some(ClientEntryKind::File),
            LocalCopyChangeSet::new(),
        );
        let rendered = render_format("%-20n", &event);
        let trimmed = rendered.trim_end_matches('\n');
        assert_eq!(trimmed.len(), 20);
        assert!(trimmed.starts_with("test.txt"));
        assert!(trimmed.ends_with("            ")); // 12 trailing spaces
    }

    // ---- Humanized bytes formatting (unit-level, via format_numeric_value) ----

    #[test]
    fn render_separator_humanized_large_value() {
        let format =
            PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::Separator);
        assert_eq!(format_numeric_value(1_234_567, &format), "1,234,567");
    }

    #[test]
    fn render_decimal_units_large_value() {
        let format = PlaceholderFormat::new(
            None,
            PlaceholderAlignment::Right,
            HumanizeMode::DecimalUnits,
        );
        assert_eq!(format_numeric_value(5000, &format), "5.00K");
    }

    #[test]
    fn render_binary_units_large_value() {
        let format =
            PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::BinaryUnits);
        assert_eq!(format_numeric_value(2048, &format), "2.00K");
    }

    // ---- Humanized bytes below threshold falls back ----

    #[test]
    fn render_decimal_units_below_threshold_falls_back_to_separator() {
        let format = PlaceholderFormat::new(
            None,
            PlaceholderAlignment::Right,
            HumanizeMode::DecimalUnits,
        );
        // Values below 1000 fall back to separator format (which is just the number for small values)
        assert_eq!(format_numeric_value(999, &format), "999");
    }

    #[test]
    fn render_binary_units_below_threshold_falls_back_to_separator() {
        let format =
            PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::BinaryUnits);
        // Values below 1024 fall back to separator format
        assert_eq!(format_numeric_value(1023, &format), "1,023");
    }
}
