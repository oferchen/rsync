#![deny(unsafe_code)]
//! Generator role implementation for server mode.
//!
//! The generator walks the local filesystem and sends file metadata and signatures
//! to the client/receiver. This mirrors upstream rsync's generator.c functionality.

use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use checksums::RollingChecksum;
use checksums::strong::Md4;
use engine::delta::{SignatureLayoutParams, calculate_signature_layout};
use protocol::wire::{FileEntry, SignatureBlock, write_signature};
use protocol::ProtocolVersion;
use walk::{WalkBuilder, WalkEntry};

use super::ServerConfig;
use crate::message::Role;

/// Generator role error conditions.
#[derive(Debug)]
pub enum GeneratorError {
    /// File walk failed.
    WalkError(walk::WalkError),
    /// I/O error during file list transmission.
    IoError(io::Error),
    /// Failed to read file for signature generation.
    SignatureReadError {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// Signature layout calculation failed.
    SignatureLayoutError {
        /// Path being processed.
        path: PathBuf,
        /// Layout error.
        source: engine::delta::SignatureLayoutError,
    },
}

impl std::fmt::Display for GeneratorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GeneratorError::WalkError(e) => write!(f, "file walk failed: {}", e),
            GeneratorError::IoError(e) => write!(f, "I/O error: {}", e),
            GeneratorError::SignatureReadError { path, source } => {
                write!(f, "failed to read file {:?} for signature: {}", path, source)
            }
            GeneratorError::SignatureLayoutError { path, source } => {
                write!(f, "signature layout error for {:?}: {}", path, source)
            }
        }
    }
}

impl std::error::Error for GeneratorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GeneratorError::WalkError(e) => Some(e),
            GeneratorError::IoError(e) => Some(e),
            GeneratorError::SignatureReadError { source, .. } => Some(source),
            GeneratorError::SignatureLayoutError { source, .. } => Some(source),
        }
    }
}

impl From<io::Error> for GeneratorError {
    fn from(e: io::Error) -> Self {
        GeneratorError::IoError(e)
    }
}

impl From<walk::WalkError> for GeneratorError {
    fn from(e: walk::WalkError) -> Self {
        GeneratorError::WalkError(e)
    }
}

/// Runs the generator role over stdio.
///
/// The generator:
/// 1. Walks the local filesystem specified in config.args
/// 2. Sends file list as FileEntry records via stdout
/// 3. Waits for signature requests on stdin
/// 4. Generates and sends signatures for requested files
///
/// This implements the server-side generator role that corresponds to
/// upstream rsync's generator.c.
pub fn run_generator(
    config: ServerConfig,
    _stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> Result<i32, GeneratorError> {
    let _role = Role::Generator;

    let source_paths = config.args;
    if source_paths.is_empty() {
        return Err(GeneratorError::IoError(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no source paths specified for generator",
        )));
    }

    let file_list = build_file_list(&source_paths)?;
    send_file_list_dyn(stdout, &file_list)?;

    Ok(0)
}

/// Helper to send file list with dynamic dispatch.
fn send_file_list_dyn(
    stdout: &mut dyn Write,
    file_list: &[FileEntry],
) -> Result<(), GeneratorError> {
    let mut prev: Option<&FileEntry> = None;
    let mut buffer = Vec::new();

    for entry in file_list {
        buffer.clear();
        entry.write_to(&mut buffer, prev)?;
        stdout.write_all(&buffer)?;
        prev = Some(entry);
    }

    stdout.write_all(&[0x00])?;
    stdout.flush()?;

    Ok(())
}

/// Builds a file list by walking the provided source paths.
fn build_file_list(sources: &[std::ffi::OsString]) -> Result<Vec<FileEntry>, GeneratorError> {
    let mut file_list = Vec::new();

    for source in sources {
        let source_path = Path::new(source);

        if !source_path.exists() {
            return Err(GeneratorError::IoError(io::Error::new(
                io::ErrorKind::NotFound,
                format!("source path {:?} does not exist", source_path),
            )));
        }

        let walker = WalkBuilder::new(source_path)
            .include_root(true)
            .build()?;

        for entry_result in walker {
            let entry = entry_result?;

            if let Ok(file_entry) = convert_walk_entry_to_file_entry(&entry) {
                file_list.push(file_entry);
            }
        }
    }

    Ok(file_list)
}

/// Converts a walk entry to a wire protocol FileEntry.
#[cfg(unix)]
fn convert_walk_entry_to_file_entry(entry: &WalkEntry) -> io::Result<FileEntry> {
    let metadata = entry.metadata();
    FileEntry::from_metadata(entry.full_path(), metadata)
}

#[cfg(not(unix))]
fn convert_walk_entry_to_file_entry(entry: &WalkEntry) -> io::Result<FileEntry> {
    use protocol::wire::FileType;

    let metadata = entry.metadata();
    let path = entry.relative_path();

    let file_type = if metadata.is_dir() {
        FileType::Directory
    } else if metadata.is_symlink() {
        FileType::Symlink
    } else {
        FileType::Regular
    };

    let symlink_target = if file_type == FileType::Symlink {
        Some(std::fs::read_link(entry.full_path())?.to_string_lossy().into_owned())
    } else {
        None
    };

    Ok(FileEntry {
        path: path.to_string_lossy().into_owned(),
        file_type,
        size: metadata.len(),
        mtime: metadata.modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        mode: 0o644,
        uid: None,
        gid: None,
        symlink_target,
        dev_major: None,
        dev_minor: None,
    })
}

/// Sends the file list to stdout using wire protocol format.
fn send_file_list<W: Write>(
    writer: &mut W,
    file_list: &[FileEntry],
) -> Result<(), GeneratorError> {
    let mut prev: Option<&FileEntry> = None;

    for entry in file_list {
        entry.write_to(writer, prev)?;
        prev = Some(entry);
    }

    writer.write_all(&[0x00])?;
    writer.flush()?;

    Ok(())
}

/// Generates a signature for a file and sends it via stdout.
///
/// This reads the file in blocks, computes rolling and strong checksums,
/// and writes the signature using the wire protocol format.
pub fn generate_and_send_signature<W: Write>(
    file_path: &Path,
    file_size: u64,
    protocol: ProtocolVersion,
    stdout: &mut W,
) -> Result<(), GeneratorError> {
    let params = SignatureLayoutParams::new(
        file_size,
        None,
        protocol,
        std::num::NonZeroU8::new(16).expect("checksum length is non-zero"),
    );

    let layout = calculate_signature_layout(params).map_err(|e| {
        GeneratorError::SignatureLayoutError {
            path: file_path.to_path_buf(),
            source: e,
        }
    })?;

    let file = File::open(file_path).map_err(|e| GeneratorError::SignatureReadError {
        path: file_path.to_path_buf(),
        source: e,
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

        let mut rolling = RollingChecksum::new();
        rolling.update(block_data);
        let rolling_sum = rolling.digest();

        let mut strong = Md4::new();
        strong.update(block_data);
        let strong_sum = strong.finalize()[..layout.strong_sum_length().get() as usize].to_vec();

        blocks.push(SignatureBlock {
            index: block_index,
            rolling_sum: rolling_sum.into(),
            strong_sum,
        });

        block_index += 1;
    }

    write_signature(
        stdout,
        blocks.len() as u32,
        layout.block_length().get(),
        layout.strong_sum_length().get(),
        &blocks,
    )?;

    stdout.flush()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::TempDir;

    #[test]
    fn build_file_list_for_single_file() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"hello world").unwrap();

        let sources = vec![file_path.as_os_str().to_owned()];
        let file_list = build_file_list(&sources).unwrap();

        assert_eq!(file_list.len(), 1);
        assert!(file_list[0].path.contains("test.txt"));
        assert_eq!(file_list[0].size, 11);
    }

    #[test]
    fn build_file_list_for_directory() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("subdir");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("file1.txt"), b"data1").unwrap();
        std::fs::write(dir.join("file2.txt"), b"data2").unwrap();

        let sources = vec![dir.as_os_str().to_owned()];
        let file_list = build_file_list(&sources).unwrap();

        assert!(file_list.len() >= 3);
    }

    #[test]
    fn send_file_list_writes_wire_format() {
        let entry = FileEntry {
            path: "test.txt".to_string(),
            file_type: protocol::wire::FileType::Regular,
            size: 100,
            mtime: 1700000000,
            mode: 0o644,
            uid: Some(1000),
            gid: Some(1000),
            symlink_target: None,
            dev_major: None,
            dev_minor: None,
        };

        let mut output = Vec::new();
        send_file_list(&mut output, &[entry]).unwrap();

        assert!(!output.is_empty());
        assert_eq!(output[output.len() - 1], 0x00);
    }

    #[test]
    fn generate_signature_for_small_file() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("data.bin");
        let data = vec![0xAA; 2048];
        std::fs::write(&file_path, &data).unwrap();

        let mut output = Vec::new();
        generate_and_send_signature(
            &file_path,
            2048,
            ProtocolVersion::NEWEST,
            &mut output,
        )
        .unwrap();

        assert!(!output.is_empty());
    }
}
