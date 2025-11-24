#![deny(unsafe_code)]
//! Receiver role implementation for server mode.
//!
//! The receiver reads file metadata from the client, receives delta operations,
//! and applies them to create or update local files. This mirrors upstream
//! rsync's receiver.c functionality.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use engine::delta::{DeltaScript, DeltaToken, SignatureLayoutParams, calculate_signature_layout};
use protocol::wire::{FileEntry, FileType, read_delta, write_signature, SignatureBlock};
use protocol::ProtocolVersion;

use super::ServerConfig;
use crate::message::Role;

/// Receiver role error conditions.
#[derive(Debug)]
pub enum ReceiverError {
    /// I/O error during file reception.
    IoError(io::Error),
    /// Failed to read file list from wire protocol.
    FileListReadError {
        /// Underlying I/O error.
        source: io::Error,
    },
    /// Failed to apply delta to file.
    DeltaApplicationError {
        /// Path being updated.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
    /// Failed to set file metadata.
    MetadataError {
        /// Path being updated.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
    /// Signature generation failed.
    SignatureError {
        /// Path being processed.
        path: PathBuf,
        /// Underlying error.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl std::fmt::Display for ReceiverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReceiverError::IoError(e) => write!(f, "I/O error: {}", e),
            ReceiverError::FileListReadError { source } => {
                write!(f, "failed to read file list: {}", source)
            }
            ReceiverError::DeltaApplicationError { path, source } => {
                write!(f, "failed to apply delta to {:?}: {}", path, source)
            }
            ReceiverError::MetadataError { path, source } => {
                write!(f, "failed to set metadata for {:?}: {}", path, source)
            }
            ReceiverError::SignatureError { path, source } => {
                write!(f, "failed to generate signature for {:?}: {}", path, source)
            }
        }
    }
}

impl std::error::Error for ReceiverError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReceiverError::IoError(e) => Some(e),
            ReceiverError::FileListReadError { source } => Some(source),
            ReceiverError::DeltaApplicationError { source, .. } => Some(source),
            ReceiverError::MetadataError { source, .. } => Some(source),
            ReceiverError::SignatureError { source, .. } => Some(source.as_ref()),
        }
    }
}

impl From<io::Error> for ReceiverError {
    fn from(e: io::Error) -> Self {
        ReceiverError::IoError(e)
    }
}

/// Runs the receiver role over stdio.
///
/// The receiver:
/// 1. Reads file list as FileEntry records from stdin
/// 2. For each file that exists locally, generates and sends signature
/// 3. Receives delta operations from stdin
/// 4. Applies deltas to create/update files
/// 5. Sets file metadata (permissions, timestamps)
///
/// This implements the server-side receiver role that corresponds to
/// upstream rsync's receiver.c.
pub fn run_receiver(
    config: ServerConfig,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> Result<i32, ReceiverError> {
    let _role = Role::Receiver;

    let destination = config.args.first().ok_or_else(|| {
        ReceiverError::IoError(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no destination path specified for receiver",
        ))
    })?;

    let dest_path = Path::new(destination);

    let file_list = receive_file_list(stdin)?;

    for entry in &file_list {
        let target_path = dest_path.join(&entry.path);

        if entry.file_type == FileType::Directory {
            create_directory(&target_path, entry)?;
            continue;
        }

        if entry.file_type == FileType::Symlink {
            create_symlink(&target_path, entry)?;
            continue;
        }

        if entry.file_type == FileType::Regular {
            if target_path.exists() && target_path.is_file() {
                send_signature_for_file(&target_path, entry, stdout)?;
            } else {
                request_full_transfer(stdout)?;
            }

            receive_and_apply_delta(stdin, &target_path, entry)?;

            set_file_metadata(&target_path, entry)?;
        }
    }

    stdout.flush()?;

    Ok(0)
}

/// Receives the file list from stdin.
fn receive_file_list(stdin: &mut dyn Read) -> Result<Vec<FileEntry>, ReceiverError> {
    let mut file_list = Vec::new();
    let mut prev: Option<FileEntry> = None;
    let mut buffer = Vec::new();
    stdin.read_to_end(&mut buffer)?;
    let mut cursor = io::Cursor::new(buffer);

    loop {
        let mut flags_buf = [0u8; 1];
        if let Err(e) = cursor.read_exact(&mut flags_buf) {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                break;
            }
            return Err(ReceiverError::FileListReadError { source: e });
        }

        if flags_buf[0] == 0x00 {
            break;
        }

        let pos_before = cursor.position();
        cursor.set_position(pos_before - 1);

        let entry = FileEntry::read_from(&mut cursor, prev.as_ref())
            .map_err(|e| ReceiverError::FileListReadError { source: e })?;

        file_list.push(entry.clone());
        prev = Some(entry);
    }

    Ok(file_list)
}

/// Creates a directory with appropriate metadata.
fn create_directory(path: &Path, entry: &FileEntry) -> Result<(), ReceiverError> {
    if path.exists() && path.is_dir() {
        return Ok(());
    }

    fs::create_dir_all(path).map_err(|e| ReceiverError::MetadataError {
        path: path.to_path_buf(),
        source: e,
    })?;

    set_permissions(path, entry.mode)?;

    Ok(())
}

/// Creates a symlink.
#[cfg(unix)]
fn create_symlink(path: &Path, entry: &FileEntry) -> Result<(), ReceiverError> {
    if let Some(ref target) = entry.symlink_target {
        if path.exists() {
            fs::remove_file(path).map_err(|e| ReceiverError::IoError(e))?;
        }

        std::os::unix::fs::symlink(target, path).map_err(|e| ReceiverError::IoError(e))?;
    }

    Ok(())
}

#[cfg(not(unix))]
fn create_symlink(path: &Path, entry: &FileEntry) -> Result<(), ReceiverError> {
    if let Some(ref target) = entry.symlink_target {
        if path.exists() {
            fs::remove_file(path).map_err(|e| ReceiverError::IoError(e))?;
        }

        #[cfg(windows)]
        {
            if entry.file_type == FileType::Directory {
                std::os::windows::fs::symlink_dir(target, path)
            } else {
                std::os::windows::fs::symlink_file(target, path)
            }
            .map_err(|e| ReceiverError::IoError(e))?;
        }

        #[cfg(not(windows))]
        {
            let _ = (path, target);
            return Err(ReceiverError::IoError(io::Error::new(
                io::ErrorKind::Unsupported,
                "symlinks not supported on this platform",
            )));
        }
    }

    Ok(())
}

/// Generates and sends signature for an existing file.
fn send_signature_for_file(
    path: &Path,
    entry: &FileEntry,
    stdout: &mut dyn Write,
) -> Result<(), ReceiverError> {
    let file_size = entry.size;

    let params = SignatureLayoutParams::new(
        file_size,
        None,
        ProtocolVersion::NEWEST,
        std::num::NonZeroU8::new(16).expect("checksum length is non-zero"),
    );

    let layout = calculate_signature_layout(params).map_err(|e| ReceiverError::SignatureError {
        path: path.to_path_buf(),
        source: Box::new(e),
    })?;

    let file = File::open(path).map_err(|e| ReceiverError::SignatureError {
        path: path.to_path_buf(),
        source: Box::new(e),
    })?;

    let mut reader = BufReader::new(file);
    let block_length = layout.block_length().get() as usize;
    let mut blocks = Vec::new();
    let mut block_index = 0u32;
    let mut buffer = vec![0u8; block_length];

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        let block_data = &buffer[..bytes_read];

        let mut rolling = checksums::RollingChecksum::new();
        rolling.update(block_data);
        let rolling_sum = rolling.digest();

        let mut strong = checksums::strong::Md4::new();
        strong.update(block_data);
        let strong_sum = strong.finalize()[..layout.strong_sum_length().get() as usize].to_vec();

        blocks.push(SignatureBlock {
            index: block_index,
            rolling_sum: rolling_sum.into(),
            strong_sum,
        });

        block_index += 1;
    }

    let mut buffer = Vec::new();
    write_signature(
        &mut buffer,
        blocks.len() as u32,
        layout.block_length().get(),
        layout.strong_sum_length().get(),
        &blocks,
    )?;

    stdout.write_all(&buffer)?;
    stdout.flush()?;

    Ok(())
}

/// Requests full file transfer (no basis file available).
fn request_full_transfer(stdout: &mut dyn Write) -> Result<(), ReceiverError> {
    let mut buffer = Vec::new();
    write_signature(&mut buffer, 0, 0, 0, &[])?;
    stdout.write_all(&buffer)?;
    stdout.flush()?;
    Ok(())
}

/// Receives delta operations and applies them to create/update the file.
fn receive_and_apply_delta(
    stdin: &mut dyn Read,
    target_path: &Path,
    _entry: &FileEntry,
) -> Result<(), ReceiverError> {
    let mut buffer = Vec::new();
    stdin.read_to_end(&mut buffer)?;
    let delta_ops = read_delta(&mut &buffer[..]).map_err(|e| ReceiverError::DeltaApplicationError {
        path: target_path.to_path_buf(),
        source: e,
    })?;

    if delta_ops.is_empty() {
        return Ok(());
    }

    let temp_path = target_path.with_extension("rsync-tmp");

    let basis = if target_path.exists() {
        Some(BufReader::new(
            File::open(target_path).map_err(|e| ReceiverError::DeltaApplicationError {
                path: target_path.to_path_buf(),
                source: e,
            })?,
        ))
    } else {
        None
    };

    let output = BufWriter::new(
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)
            .map_err(|e| ReceiverError::DeltaApplicationError {
                path: target_path.to_path_buf(),
                source: e,
            })?,
    );

    let tokens: Vec<DeltaToken> = delta_ops
        .into_iter()
        .map(|op| match op {
            protocol::wire::DeltaOp::Literal(data) => DeltaToken::Literal(data),
            protocol::wire::DeltaOp::Copy {
                block_index,
                length,
            } => DeltaToken::Copy {
                index: block_index as u64,
                len: length as usize,
            },
        })
        .collect();

    let total_bytes = tokens
        .iter()
        .map(|t| match t {
            DeltaToken::Literal(data) => data.len() as u64,
            DeltaToken::Copy { len, .. } => *len as u64,
        })
        .sum();
    let literal_bytes = tokens
        .iter()
        .filter_map(|t| match t {
            DeltaToken::Literal(data) => Some(data.len() as u64),
            _ => None,
        })
        .sum();

    let script = DeltaScript::new(tokens, total_bytes, literal_bytes);

    if let Some(mut basis_reader) = basis {
        apply_delta_dyn(&mut basis_reader, output, &script).map_err(|e| {
            ReceiverError::DeltaApplicationError {
                path: target_path.to_path_buf(),
                source: e,
            }
        })?;
    } else {
        let mut empty_basis = io::Cursor::new(Vec::new());
        apply_delta_dyn(&mut empty_basis, output, &script).map_err(|e| {
            ReceiverError::DeltaApplicationError {
                path: target_path.to_path_buf(),
                source: e,
            }
        })?;
    }

    fs::rename(&temp_path, target_path).map_err(|e| ReceiverError::DeltaApplicationError {
        path: target_path.to_path_buf(),
        source: e,
    })?;

    Ok(())
}

/// Helper to apply delta with dynamic dispatch.
fn apply_delta_dyn<R: Read, W: Write>(
    basis: &mut R,
    output: W,
    script: &DeltaScript,
) -> io::Result<()> {
    let mut buffer = Vec::new();
    basis.read_to_end(&mut buffer)?;

    let mut cursor = io::Cursor::new(buffer);
    let mut output_writer = output;

    for token in script.tokens() {
        match token {
            DeltaToken::Literal(data) => {
                output_writer.write_all(data)?;
            }
            DeltaToken::Copy { index, len } => {
                let offset = *index * 700;
                cursor.seek(SeekFrom::Start(offset))?;
                let mut limited = Read::by_ref(&mut cursor).take(*len as u64);
                io::copy(&mut limited, &mut output_writer)?;
            }
        }
    }

    Ok(())
}

/// Sets file metadata (permissions, timestamps).
fn set_file_metadata(path: &Path, entry: &FileEntry) -> Result<(), ReceiverError> {
    set_permissions(path, entry.mode)?;

    #[cfg(unix)]
    {
        if let (Some(_uid), Some(_gid)) = (entry.uid, entry.gid) {
        }
    }

    let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(entry.mtime as u64);
    let ftime = filetime::FileTime::from_system_time(mtime);
    filetime::set_file_mtime(path, ftime).map_err(|e| ReceiverError::MetadataError {
        path: path.to_path_buf(),
        source: e,
    })?;

    Ok(())
}

/// Sets file permissions.
#[cfg(unix)]
fn set_permissions(path: &Path, mode: u32) -> Result<(), ReceiverError> {
    use std::os::unix::fs::PermissionsExt;

    let perms = std::fs::Permissions::from_mode(mode);
    fs::set_permissions(path, perms).map_err(|e| ReceiverError::MetadataError {
        path: path.to_path_buf(),
        source: e,
    })?;

    Ok(())
}

#[cfg(not(unix))]
fn set_permissions(_path: &Path, _mode: u32) -> Result<(), ReceiverError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn create_directory_creates_new_dir() {
        let temp = TempDir::new().unwrap();
        let dir_path = temp.path().join("new_dir");

        let entry = FileEntry {
            path: "new_dir".to_string(),
            file_type: FileType::Directory,
            size: 0,
            mtime: 1700000000,
            mode: 0o755,
            uid: Some(1000),
            gid: Some(1000),
            symlink_target: None,
            dev_major: None,
            dev_minor: None,
        };

        create_directory(&dir_path, &entry).unwrap();
        assert!(dir_path.exists());
        assert!(dir_path.is_dir());
    }

    #[test]
    fn set_file_metadata_updates_mtime() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"data").unwrap();

        let entry = FileEntry {
            path: "test.txt".to_string(),
            file_type: FileType::Regular,
            size: 4,
            mtime: 1600000000,
            mode: 0o644,
            uid: Some(1000),
            gid: Some(1000),
            symlink_target: None,
            dev_major: None,
            dev_minor: None,
        };

        set_file_metadata(&file_path, &entry).unwrap();

        let metadata = fs::metadata(&file_path).unwrap();
        let mtime = metadata.modified().unwrap();
        let expected = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1600000000);

        assert_eq!(mtime, expected);
    }

    #[test]
    fn request_full_transfer_sends_empty_signature() {
        let mut output = Vec::new();
        request_full_transfer(&mut output).unwrap();
        assert!(!output.is_empty());
    }
}
