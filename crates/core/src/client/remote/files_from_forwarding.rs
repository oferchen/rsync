//! Helper for serializing a local `--files-from` source into the wire-format
//! byte stream consumed by [`protocol::read_files_from_stream`].
//!
//! Used by both daemon-transfer pull and SSH-transfer pull paths when the
//! client (receiver) must forward a local file list to the remote sender.
//!
//! # Upstream Reference
//!
//! - `io.c:forward_filesfrom_data()` - reads from local fd, writes to socket
//! - `main.c:1226-1234,1428-1431` - `start_filesfrom_forwarding(filesfrom_fd)`

use crate::client::config::{ClientConfig, FilesFromSource};
use crate::client::error::{ClientError, invalid_argument_error};

/// A local `--files-from` list staged for a pull.
#[derive(Debug)]
pub(crate) struct StagedFilesFrom {
    /// The bytes forwarded to the sender: NUL-separated names ending in the
    /// double-NUL sentinel, converted to the wire charset when `--iconv`
    /// applies to the list.
    pub(crate) wire: Vec<u8>,
    /// The same names before conversion, or `None` when nothing was
    /// converted and `wire` already holds them.
    ///
    /// upstream: io.c:86 and io.c:512 call `add_implied_include()` on each
    /// name before `iconvbufs()` converts it, so the implied rules are in the
    /// local charset - the one received names are converted back into.
    pub(crate) local: Option<Vec<u8>>,
}

/// Reads the `--files-from` source and serializes it into the wire format
/// for forwarding to a remote peer.
///
/// Handles both `Stdin` (reads from standard input) and `LocalFile` (reads
/// from the given path). The output is NUL-separated filenames terminated
/// by a double-NUL sentinel, matching the format expected by
/// [`protocol::read_files_from_stream`] on the remote side. Returns empty
/// lists for sources that do not require local forwarding (`None`,
/// `RemoteFile`).
pub(crate) fn read_local_files_from_for_forwarding(
    config: &ClientConfig,
) -> Result<StagedFilesFrom, ClientError> {
    let eol_nulls = config.from0();
    // upstream: compat.c:820-827 - filesfrom_convert is set when
    // protect_args && files_from && (am_sender ? ic_send : ic_recv) != -1.
    // For pull, this peer is the receiver writing to the wire; the converter
    // transcodes from local charset to the UTF-8 wire encoding.
    let iconv_converter = if config.protect_args().unwrap_or(false) {
        config.iconv().resolve_converter()
    } else {
        None
    };
    // The list is read once, unconverted; the wire form is derived from it so
    // the pre-conversion names stay available for the implied includes.
    let mut wire_data = Vec::new();

    match config.files_from() {
        FilesFromSource::Stdin => {
            let stdin = std::io::stdin();
            let mut reader = stdin.lock();
            protocol::forward_files_from(&mut reader, &mut wire_data, eol_nulls, None).map_err(
                |e| invalid_argument_error(&format!("failed to read --files-from stdin: {e}"), 23),
            )?;
        }
        FilesFromSource::LocalFile(path) => {
            read_path_into(path, &mut wire_data, eol_nulls)?;
        }
        FilesFromSource::HybridLocalRemote { local_path, .. } => {
            // upstream: options.c:2675-2720 - a localhost:path hostspec is a
            // single local fd. This function only runs on PULL (gated by the
            // resolver's stage_local_bytes), where the receiver opens the file
            // locally and forwards its bytes to the remote sender. The remote
            // sender reads `--files-from=-`; the stripped path is never sent.
            read_path_into(local_path, &mut wire_data, eol_nulls)?;
        }
        FilesFromSource::None | FilesFromSource::RemoteFile(_) => {}
    }

    let Some(converter) = iconv_converter else {
        return Ok(StagedFilesFrom {
            wire: wire_data,
            local: None,
        });
    };
    // The staged list is NUL-separated already, so re-reading it with
    // eol_nulls converts each name and keeps the sentinel unchanged.
    let mut converted = Vec::with_capacity(wire_data.len());
    protocol::forward_files_from(
        &mut wire_data.as_slice(),
        &mut converted,
        true,
        Some(&converter),
    )
    .map_err(|e| invalid_argument_error(&format!("failed to convert --files-from: {e}"), 23))?;
    Ok(StagedFilesFrom {
        wire: converted,
        local: Some(wire_data),
    })
}

fn read_path_into(
    path: &std::path::Path,
    wire_data: &mut Vec<u8>,
    eol_nulls: bool,
) -> Result<(), ClientError> {
    // upstream: options.c:2719 - the same `open_no_attacker_symlinks()` guard
    // as the local read. Forwarding the list to a remote sender does not make
    // the local path any less operator-supplied.
    let mut file = super::operator_file::open_read(path).map_err(|e| {
        invalid_argument_error(
            &format!("failed to open --files-from {}: {e}", path.display()),
            23,
        )
    })?;
    protocol::forward_files_from(&mut file, wire_data, eol_nulls, None).map_err(|e| {
        invalid_argument_error(
            &format!("failed to read --files-from {}: {e}", path.display()),
            23,
        )
    })?;
    Ok(())
}
