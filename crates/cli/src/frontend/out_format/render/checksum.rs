#![deny(unsafe_code)]

//! Full-file MD5 checksum computation for the `%C` out-format placeholder.

use std::fmt::Write as FmtWrite;
use std::fs::File;
use std::io::{ErrorKind, Read};

use checksums::strong::Md5;
use core::client::{ClientEntryKind, ClientEvent, ClientEventKind};

/// Computes and formats the full-file MD5 checksum for an event's destination file.
///
/// Returns a 32-character hex string, or 32 spaces when the checksum is not applicable
/// (non-file entries, non-transfer events, or I/O errors).
pub(super) fn format_full_checksum(event: &ClientEvent) -> String {
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
