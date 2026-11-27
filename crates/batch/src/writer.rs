//! Batch file writer for recording transfers.

use crate::BatchConfig;
use crate::error::{BatchError, BatchResult};
use crate::format::{BatchFlags, BatchHeader, BatchStats, FileEntry};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufWriter, Write};

/// Writer for batch mode operations.
///
/// Records file list and delta operations to a batch file that can be
/// replayed later. This allows offline distribution of changes.
#[derive(Debug)]
pub struct BatchWriter {
    /// Configuration for this batch operation.
    config: BatchConfig,
    /// Writer for the binary batch file.
    batch_file: Option<BufWriter<File>>,
    /// The `.sh` companion, created up front and written by
    /// [`BatchWriter::write_header`].
    script_file: Option<File>,
    /// Whether the header has been written.
    header_written: bool,
    /// Stream flags recorded in the batch header.
    ///
    /// Captured at [`BatchWriter::write_header`] so the post-flist id-list
    /// region can be gated on the same `preserve_uid` / `preserve_gid` /
    /// `preserve_acls` predicates upstream uses in `uidlist.c:send_id_lists()`.
    stream_flags: BatchFlags,
}

impl BatchWriter {
    /// Create a new batch writer, creating the `.sh` companion (write modes)
    /// and then the batch file.
    ///
    /// upstream: batch.c:247-273 open_batch_files() - both files exist from
    /// the start of the run, so a run that fails after setup still leaves its
    /// replay script. The `.sh` open failure is a [`BatchError::ScriptFileUnusable`]
    /// (`RERR_FILESELECT`), the batch file's a [`BatchError::BatchFileUnusable`]
    /// (`RERR_FILEIO`).
    pub fn new(config: BatchConfig) -> BatchResult<Self> {
        let script_file = if config.is_write_mode() {
            Some(crate::script::create_script_file(&config)?)
        } else {
            None
        };
        let batch_path = config.batch_file_path();
        // upstream: batch.c:263 creates the batch file through
        // `open_no_attacker_symlinks()` at `S_IRUSR|S_IWUSR`. Owner-only is
        // load-bearing: the batch stream carries the transferred file contents,
        // so a `File::create` default (0666 & ~umask) publishes them.
        let file =
            crate::operator_file::create_write(batch_path, crate::operator_file::BATCH_FILE_MODE)
                .map_err(|e| {
                // upstream: batch.c:269-272 - full_fname(batch_name).
                BatchError::BatchFileUnusable(format!(
                    "Batch file {} open error: {}",
                    logging::full_fname(batch_path),
                    logging::upstream_errno_text(&e)
                ))
            })?;

        Ok(Self {
            config,
            batch_file: Some(BufWriter::new(file)),
            script_file,
            header_written: false,
            stream_flags: BatchFlags::default(),
        })
    }

    /// Returns the stream flags recorded in the batch header.
    ///
    /// Defaults to [`BatchFlags::default`] until [`BatchWriter::write_header`]
    /// records the caller's flags.
    pub const fn stream_flags(&self) -> &BatchFlags {
        &self.stream_flags
    }

    /// Write the batch header and the `.sh` replay script, in upstream's
    /// order: the stream flags (`main.c:1944 write_stream_flags()`), the
    /// script (`compat.c:838-839 write_batch_shell_file()`), then the
    /// protocol fields (`io.c:2747-2763 start_write_batch()`).
    ///
    /// The script depends on the negotiated protocol (its `--filter` vs
    /// `--exclude-from` choice), and a script refusal stops before the
    /// protocol fields, leaving the same 4-byte batch upstream leaves.
    pub fn write_header(&mut self, flags: BatchFlags) -> BatchResult<()> {
        if self.header_written {
            return Err(BatchError::Io(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Batch header already written",
            )));
        }

        let mut header = BatchHeader::new(self.config.protocol_version, self.config.checksum_seed);
        header.compat_flags = self.config.compat_flags;
        header.stream_flags = flags;
        self.stream_flags = flags;

        let header_error = |e: io::Error| {
            BatchError::Io(io::Error::new(
                e.kind(),
                format!("Failed to write batch header: {e}"),
            ))
        };
        let writer = self
            .batch_file
            .as_mut()
            .ok_or_else(|| BatchError::Io(io::Error::other("Batch file not open")))?;
        header.write_stream_flags(writer).map_err(header_error)?;
        self.write_script()?;
        let writer = self
            .batch_file
            .as_mut()
            .ok_or_else(|| BatchError::Io(io::Error::other("Batch file not open")))?;
        header.write_protocol_fields(writer).map_err(header_error)?;
        self.header_written = true;
        Ok(())
    }

    /// Write and close the `.sh` companion created by [`BatchWriter::new`].
    ///
    /// upstream: batch.c:226-229 - a filter rule containing a newline is
    /// refused (`RERR_SYNTAX`) after the script so far is written;
    /// batch.c:343-346 - a write failure is `rsyserr(FERROR, errno, "Batch
    /// file %s.sh write error", ...)` and `exit_cleanup(RERR_FILEIO)`.
    fn write_script(&mut self) -> BatchResult<()> {
        let Some(mut file) = self.script_file.take() else {
            return Ok(());
        };
        let rules = &self.config.filter_rules;
        crate::script::write_script(
            &mut file,
            &self.config,
            (!rules.is_empty()).then_some(rules.as_slice()),
            self.config.operands.last().map(OsString::as_os_str),
        )
        .and_then(|()| file.flush())
        .map_err(|e| {
            if e.get_ref()
                .is_some_and(|inner| inner.is::<crate::script::RuleRefused>())
            {
                return BatchError::FilterRuleRefused(
                    crate::script::NEWLINE_RULE_REFUSAL.to_owned(),
                );
            }
            BatchError::BatchFileUnusable(format!(
                "Batch file {}.sh write error: {}",
                self.config.batch_path,
                logging::upstream_errno_text(&e)
            ))
        })
    }

    /// Write raw data to the batch file.
    ///
    /// This is used to record file list and delta operations as they
    /// occur during the transfer. The data should be in the same format
    /// as it would be sent over the network.
    pub fn write_data(&mut self, data: &[u8]) -> BatchResult<()> {
        if !self.header_written {
            return Err(BatchError::Io(io::Error::other(
                "Must write header before data",
            )));
        }

        if let Some(ref mut writer) = self.batch_file {
            writer.write_all(data).map_err(|e| {
                BatchError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to write batch data: {e}"),
                ))
            })?;
            Ok(())
        } else {
            Err(BatchError::Io(io::Error::other("Batch file not open")))
        }
    }

    /// Write transfer statistics to the end of the batch file.
    ///
    /// Upstream rsync records these statistics at the end of the batch
    /// file so the `--read-batch` process can display accurate metrics.
    ///
    /// The header must be written before calling this method.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:378-388`: stats written with `write_varlong30(batch_fd, ...)`
    pub fn write_stats(&mut self, stats: &BatchStats) -> BatchResult<()> {
        if !self.header_written {
            return Err(BatchError::Io(io::Error::other(
                "Must write header before stats",
            )));
        }

        if let Some(ref mut writer) = self.batch_file {
            stats
                .write_to(writer, self.config.protocol_version)
                .map_err(|e| {
                    BatchError::Io(io::Error::new(
                        e.kind(),
                        format!("Failed to write batch stats: {e}"),
                    ))
                })?;
            Ok(())
        } else {
            Err(BatchError::Io(io::Error::other("Batch file not open")))
        }
    }

    /// Write a file entry to the batch file using a local encoding.
    ///
    /// **Note:** This uses a local serialization format that is not
    /// compatible with upstream rsync's batch files. Upstream rsync
    /// batch files contain a raw tee of the protocol stream, not
    /// separately encoded file entries. This method is provided for
    /// internal tracking during local copy operations that bypass the
    /// protocol pipeline.
    ///
    /// For protocol-compatible batch files, use [`write_data`](Self::write_data)
    /// to write raw protocol stream bytes instead.
    ///
    /// The header must be written before calling this method.
    pub fn write_file_entry(&mut self, entry: &FileEntry) -> BatchResult<()> {
        if !self.header_written {
            return Err(BatchError::Io(io::Error::other(
                "Must write header before file entries",
            )));
        }

        if let Some(ref mut writer) = self.batch_file {
            entry.write_to(writer).map_err(|e| {
                BatchError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to write file entry: {e}"),
                ))
            })?;
            Ok(())
        } else {
            Err(BatchError::Io(io::Error::other("Batch file not open")))
        }
    }

    /// The offset in the batch file the next byte of data goes to.
    pub fn data_position(&mut self) -> BatchResult<u64> {
        use std::io::Seek;
        let Some(ref mut writer) = self.batch_file else {
            return Err(BatchError::Io(io::Error::other("Batch file not open")));
        };
        writer.stream_position().map_err(|e| {
            BatchError::Io(io::Error::new(
                e.kind(),
                format!("Failed to read batch file position: {e}"),
            ))
        })
    }

    /// Overwrites bytes already written at `offset` without moving the end
    /// of the data.
    ///
    /// A record's iflags word can gain report bits after the record's body
    /// went out; this writes the final word in place.
    pub fn patch_data(&mut self, offset: u64, data: &[u8]) -> BatchResult<()> {
        let Some(ref mut writer) = self.batch_file else {
            return Err(BatchError::Io(io::Error::other("Batch file not open")));
        };
        let wrap = |e: io::Error| {
            BatchError::Io(io::Error::new(
                e.kind(),
                format!("Failed to patch batch data: {e}"),
            ))
        };
        writer.flush().map_err(wrap)?;
        let file = writer.get_mut();
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            file.write_all_at(data, offset).map_err(wrap)
        }
        #[cfg(windows)]
        {
            use std::io::{Seek, SeekFrom};
            use std::os::windows::fs::FileExt;
            let mut done = 0;
            while done < data.len() {
                let n = file
                    .seek_write(&data[done..], offset + done as u64)
                    .map_err(wrap)?;
                if n == 0 {
                    return Err(wrap(io::Error::from(io::ErrorKind::WriteZero)));
                }
                done += n;
            }
            // seek_write moves the cursor; data appends at the end.
            file.seek(SeekFrom::End(0)).map_err(wrap)?;
            Ok(())
        }
    }

    /// Flush any buffered data to disk.
    pub fn flush(&mut self) -> BatchResult<()> {
        if let Some(ref mut writer) = self.batch_file {
            writer.flush().map_err(|e| {
                BatchError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to flush batch file: {e}"),
                ))
            })?;
        }
        Ok(())
    }

    /// Finalize the batch file and close it.
    ///
    /// This ensures all data is written and the file is properly closed.
    /// After calling this, the writer can no longer be used.
    pub fn finalize(mut self) -> BatchResult<()> {
        self.flush()?;
        if let Some(writer) = self.batch_file.take() {
            drop(writer);
        }
        Ok(())
    }

    /// Get a reference to the batch configuration.
    pub const fn config(&self) -> &BatchConfig {
        &self.config
    }

    /// Get a mutable reference to the batch configuration.
    ///
    /// Used to update protocol-negotiated values (protocol version, compat
    /// flags, checksum seed) after the handshake completes but before writing
    /// the batch header.
    pub fn config_mut(&mut self) -> &mut BatchConfig {
        &mut self.config
    }
}

impl Drop for BatchWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BatchMode;
    use std::fs;

    #[test]
    fn test_batch_writer_create() {
        let temp_dir = test_support::create_tempdir();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let writer = BatchWriter::new(config);
        assert!(writer.is_ok());
    }

    #[test]
    fn test_batch_writer_write_header() {
        let temp_dir = test_support::create_tempdir();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        )
        .with_checksum_seed(12345);

        let mut writer = BatchWriter::new(config).unwrap();

        let flags = BatchFlags {
            recurse: true,
            preserve_uid: true,
            ..Default::default()
        };

        assert!(writer.write_header(flags).is_ok());
        assert!(writer.flush().is_ok());

        assert!(batch_path.exists());
        let metadata = fs::metadata(&batch_path).unwrap();
        assert!(metadata.len() > 0);
    }

    #[test]
    fn test_batch_writer_write_data() {
        let temp_dir = test_support::create_tempdir();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut writer = BatchWriter::new(config).unwrap();

        assert!(writer.write_data(b"test").is_err());

        let flags = BatchFlags::default();
        writer.write_header(flags).unwrap();

        assert!(writer.write_data(b"test data").is_ok());
        assert!(writer.flush().is_ok());
    }

    #[test]
    fn test_batch_writer_finalize() {
        let temp_dir = test_support::create_tempdir();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut writer = BatchWriter::new(config).unwrap();
        let flags = BatchFlags::default();
        writer.write_header(flags).unwrap();
        writer.write_data(b"some data").unwrap();

        assert!(writer.finalize().is_ok());
        assert!(batch_path.exists());
    }

    /// A record's iflags word is rewritten after its body went out: the patch
    /// must land on the recorded offset, leave every other byte alone, and
    /// keep later writes appending at the end, with part of the data still
    /// buffered when the patch is made.
    #[test]
    fn patch_data_rewrites_in_place_and_keeps_appending() {
        let temp_dir = test_support::create_tempdir();
        let batch_path = temp_dir.path().join("test.batch");
        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.write_data(b"head").unwrap();
        let at = writer.data_position().unwrap();
        writer.write_data(b"\x00\x00").unwrap();
        let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        writer.write_data(&body).unwrap();
        writer.write_data(b"buffered").unwrap();
        writer.patch_data(at, b"\x05\x80").unwrap();
        writer.write_data(b"tail").unwrap();
        writer.finalize().unwrap();

        let bytes = fs::read(&batch_path).unwrap();
        let at = usize::try_from(at).unwrap();
        assert_eq!(&bytes[at - 4..at], b"head");
        assert_eq!(&bytes[at..at + 2], b"\x05\x80");
        assert_eq!(&bytes[at + 2..at + 2 + body.len()], &body[..]);
        assert!(bytes.ends_with(b"bufferedtail"));
        assert_eq!(bytes.len(), at + 2 + body.len() + b"bufferedtail".len());
    }
}
