//! Helper for serializing a local `--files-from` source into the wire-format
//! byte stream consumed by [`protocol::read_files_from_stream`].
//!
//! Used by both daemon-transfer pull and SSH-transfer pull paths when the
//! client (receiver) must forward a local file list to the remote sender.
//!
//! # Upstream Reference
//!
//! - `io.c:forward_filesfrom_data()` - reads from local fd, writes to socket
//! - `main.c:1191-1198,1372-1375` - `start_filesfrom_forwarding(filesfrom_fd)`

use crate::client::config::{ClientConfig, FilesFromSource};
use crate::client::error::{ClientError, invalid_argument_error};

/// Reads the `--files-from` source and serializes it into the wire format
/// for forwarding to a remote peer.
///
/// Handles both `Stdin` (reads from standard input) and `LocalFile` (reads
/// from the given path). The output is NUL-separated filenames terminated
/// by a double-NUL sentinel, matching the format expected by
/// [`protocol::read_files_from_stream`] on the remote side. Returns an empty
/// `Vec` for sources that do not require local forwarding (`None`,
/// `RemoteFile`).
pub(crate) fn read_local_files_from_for_forwarding(
    config: &ClientConfig,
) -> Result<Vec<u8>, ClientError> {
    let eol_nulls = config.from0();
    // upstream: compat.c:799-806 - filesfrom_convert is set when
    // protect_args && files_from && (am_sender ? ic_send : ic_recv) != -1.
    // For pull, this peer is the receiver writing to the wire; the converter
    // transcodes from local charset to the UTF-8 wire encoding.
    let iconv_converter = if config.protect_args().unwrap_or(false) {
        config.iconv().resolve_converter()
    } else {
        None
    };
    let mut wire_data = Vec::new();

    match config.files_from() {
        FilesFromSource::Stdin => {
            let stdin = std::io::stdin();
            let mut reader = stdin.lock();
            protocol::forward_files_from(
                &mut reader,
                &mut wire_data,
                eol_nulls,
                iconv_converter.as_ref(),
            )
            .map_err(|e| {
                invalid_argument_error(&format!("failed to read --files-from stdin: {e}"), 23)
            })?;
        }
        FilesFromSource::LocalFile(path) => {
            read_path_into(path, &mut wire_data, eol_nulls, iconv_converter.as_ref())?;
        }
        FilesFromSource::HybridLocalRemote { local_path, .. } => {
            // upstream: options.c:2476-2501 - a localhost:path hostspec is a
            // single local fd. This function only runs on PULL (gated by the
            // resolver's stage_local_bytes), where the receiver opens the file
            // locally and forwards its bytes to the remote sender. The remote
            // sender reads `--files-from=-`; the stripped path is never sent.
            read_path_into(
                local_path,
                &mut wire_data,
                eol_nulls,
                iconv_converter.as_ref(),
            )?;
        }
        FilesFromSource::None | FilesFromSource::RemoteFile(_) => {}
    }

    Ok(wire_data)
}

fn read_path_into(
    path: &std::path::Path,
    wire_data: &mut Vec<u8>,
    eol_nulls: bool,
    iconv_converter: Option<&protocol::FilenameConverter>,
) -> Result<(), ClientError> {
    let mut file = std::fs::File::open(path).map_err(|e| {
        invalid_argument_error(
            &format!("failed to open --files-from {}: {e}", path.display()),
            23,
        )
    })?;
    protocol::forward_files_from(&mut file, wire_data, eol_nulls, iconv_converter).map_err(
        |e| {
            invalid_argument_error(
                &format!("failed to read --files-from {}: {e}", path.display()),
                23,
            )
        },
    )?;
    Ok(())
}
