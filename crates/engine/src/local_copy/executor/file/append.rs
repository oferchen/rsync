//! Append-mode transfer logic for resuming partial file copies.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::local_copy::LocalCopyError;

use super::super::super::COPY_BUFFER_SIZE;

/// Result of evaluating `--append` mode for a file transfer.
pub(crate) enum AppendMode {
    /// Append is not active; proceed with normal transfer.
    Disabled,
    /// Destination is already at least as large as the source; skip the file.
    Skip,
    /// Destination is shorter; append starting from `offset`.
    Append {
        /// Byte offset the appended tail starts at - the destination's length.
        offset: u64,
        /// Whether `--append-verify`'s whole-file re-checksum will fail, so the
        /// caller must retain the appended result and redo the file in phase 2.
        verify_failed: bool,
    },
}

/// Decides the append strategy for a file based on existing destination size.
///
/// Returns `Disabled` when append is off, `Skip` when the destination is
/// already at least as large, or `Append` when the destination is shorter and
/// the transfer should resume from that offset.
///
/// Under `--append-verify` a mismatching prefix does **not** cancel the append.
/// Upstream appends first and only then compares whole-file checksums: the
/// sender sums the source's first `flength` bytes and the receiver sums the
/// destination's, both followed by the identical appended tail
/// (match.c:372-391, receiver.c:352-379), so the comparison reduces exactly to
/// "do the two prefixes agree". A disagreement is reported through
/// `verify_failed` rather than acted on here, because upstream keeps the
/// appended bytes on disk (receiver.c:1029, reached because `--append` implies
/// `--inplace` - options.c:2400-2411) and redoes the file in phase 2 against
/// that retained partial.
/// upstream: receiver.c:recv_files() - append mode size comparison
pub(crate) fn determine_append_mode(
    append_allowed: bool,
    append_verify: bool,
    reader: &mut fs::File,
    source: &Path,
    destination: &Path,
    existing_metadata: Option<&fs::Metadata>,
    file_size: u64,
) -> Result<AppendMode, LocalCopyError> {
    if !append_allowed {
        return Ok(AppendMode::Disabled);
    }

    let existing = match existing_metadata {
        Some(meta) if meta.is_file() => meta,
        _ => return Ok(AppendMode::Disabled),
    };

    let existing_len = existing.len();
    if existing_len == 0 {
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| LocalCopyError::io("copy file", source, error))?;
        return Ok(AppendMode::Disabled);
    }

    // Upstream rsync: "If a file needs to be transferred and its size on the
    // receiver is the same or longer than the size on the sender, the file is
    // skipped."
    if existing_len >= file_size {
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| LocalCopyError::io("copy file", source, error))?;
        return Ok(AppendMode::Skip);
    }

    // Plain `--append` (append_mode == 1) never re-checksums: upstream skips the
    // prefix in `sum_update` on both sides (match.c:373-391 runs the CHUNK_SIZE
    // loop only when `append_mode == 2`), so the two whole-file sums cover just
    // the appended tail and always agree.
    let verify_failed = if append_verify {
        !verify_append_prefix(reader, source, destination, existing_len)?
    } else {
        false
    };
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| LocalCopyError::io("copy file", source, error))?;

    Ok(AppendMode::Append {
        offset: existing_len,
        verify_failed,
    })
}

/// Verifies that the existing destination prefix matches the source.
///
/// Uses a single buffer split into two halves to reduce allocation overhead.
fn verify_append_prefix(
    reader: &mut fs::File,
    source: &Path,
    destination: &Path,
    existing_len: u64,
) -> Result<bool, LocalCopyError> {
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| LocalCopyError::io("copy file", source, error))?;
    let mut destination_file = fs::File::open(destination).map_err(|error| {
        LocalCopyError::io(
            "read existing destination",
            destination.to_path_buf(),
            error,
        )
    })?;
    let mut remaining = existing_len;

    let half_size = COPY_BUFFER_SIZE / 2;
    let mut unified_buffer = vec![0u8; COPY_BUFFER_SIZE];
    let (source_buffer, destination_buffer) = unified_buffer.split_at_mut(half_size);

    while remaining > 0 {
        let chunk = remaining.min(half_size as u64) as usize;
        let source_read = reader
            .read(&mut source_buffer[..chunk])
            .map_err(|error| LocalCopyError::io("copy file", source, error))?;
        let destination_read = destination_file
            .read(&mut destination_buffer[..chunk])
            .map_err(|error| {
                LocalCopyError::io(
                    "read existing destination",
                    destination.to_path_buf(),
                    error,
                )
            })?;

        if source_read == 0 || destination_read == 0 || source_read != destination_read {
            return Ok(false);
        }

        if source_buffer[..source_read] != destination_buffer[..destination_read] {
            return Ok(false);
        }

        remaining = remaining.saturating_sub(source_read as u64);
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn determine_append_mode_disabled_when_not_allowed() {
        let temp = tempdir().expect("tempdir");
        let source_path = temp.path().join("source.txt");
        fs::write(&source_path, b"source content").expect("write source");
        let mut reader = fs::File::open(&source_path).expect("open source");

        let result = determine_append_mode(
            false, // append not allowed
            false,
            &mut reader,
            &source_path,
            Path::new("/dest"),
            None,
            14,
        )
        .expect("determine");

        assert!(matches!(result, AppendMode::Disabled));
    }

    #[test]
    fn determine_append_mode_disabled_when_no_existing() {
        let temp = tempdir().expect("tempdir");
        let source_path = temp.path().join("source.txt");
        fs::write(&source_path, b"source content").expect("write source");
        let mut reader = fs::File::open(&source_path).expect("open source");

        let result = determine_append_mode(
            true, // append allowed
            false,
            &mut reader,
            &source_path,
            Path::new("/dest"),
            None, // no existing
            14,
        )
        .expect("determine");

        assert!(matches!(result, AppendMode::Disabled));
    }

    #[test]
    fn determine_append_mode_disabled_when_existing_is_empty() {
        let temp = tempdir().expect("tempdir");
        let source_path = temp.path().join("source.txt");
        let dest_path = temp.path().join("dest.txt");
        fs::write(&source_path, b"source content").expect("write source");
        fs::write(&dest_path, b"").expect("write dest");
        let mut reader = fs::File::open(&source_path).expect("open source");
        let dest_meta = fs::metadata(&dest_path).expect("dest metadata");

        let result = determine_append_mode(
            true,
            false,
            &mut reader,
            &source_path,
            &dest_path,
            Some(&dest_meta),
            14,
        )
        .expect("determine");

        assert!(matches!(result, AppendMode::Disabled));
    }

    #[test]
    fn determine_append_mode_skips_when_existing_larger() {
        let temp = tempdir().expect("tempdir");
        let source_path = temp.path().join("source.txt");
        let dest_path = temp.path().join("dest.txt");
        fs::write(&source_path, b"short").expect("write source");
        fs::write(&dest_path, b"much longer destination content").expect("write dest");
        let mut reader = fs::File::open(&source_path).expect("open source");
        let dest_meta = fs::metadata(&dest_path).expect("dest metadata");

        let result = determine_append_mode(
            true,
            false,
            &mut reader,
            &source_path,
            &dest_path,
            Some(&dest_meta),
            5, // source is 5 bytes
        )
        .expect("determine");

        assert!(matches!(result, AppendMode::Skip));
    }

    #[test]
    fn determine_append_mode_skips_when_existing_equal_size() {
        let temp = tempdir().expect("tempdir");
        let source_path = temp.path().join("source.txt");
        let dest_path = temp.path().join("dest.txt");
        fs::write(&source_path, b"same size").expect("write source");
        fs::write(&dest_path, b"same size").expect("write dest");
        let mut reader = fs::File::open(&source_path).expect("open source");
        let dest_meta = fs::metadata(&dest_path).expect("dest metadata");

        let result = determine_append_mode(
            true,
            false,
            &mut reader,
            &source_path,
            &dest_path,
            Some(&dest_meta),
            9, // source is 9 bytes
        )
        .expect("determine");

        assert!(matches!(result, AppendMode::Skip));
    }

    #[test]
    fn determine_append_mode_returns_offset_when_existing_shorter() {
        let temp = tempdir().expect("tempdir");
        let source_path = temp.path().join("source.txt");
        let dest_path = temp.path().join("dest.txt");
        fs::write(&source_path, b"source content here").expect("write source");
        fs::write(&dest_path, b"source").expect("write dest - partial content");
        let mut reader = fs::File::open(&source_path).expect("open source");
        let dest_meta = fs::metadata(&dest_path).expect("dest metadata");

        let result = determine_append_mode(
            true,
            false, // no verify
            &mut reader,
            &source_path,
            &dest_path,
            Some(&dest_meta),
            19, // full source size
        )
        .expect("determine");

        match result {
            AppendMode::Append {
                offset,
                verify_failed,
            } => {
                assert_eq!(offset, 6); // "source" is 6 bytes
                // Without --append-verify upstream never re-checksums the
                // prefix, so no redo can be requested.
                assert!(!verify_failed);
            }
            AppendMode::Disabled | AppendMode::Skip => panic!("expected Append mode"),
        }
    }

    #[test]
    fn determine_append_mode_with_verify_succeeds_when_prefix_matches() {
        let temp = tempdir().expect("tempdir");
        let source_path = temp.path().join("source.txt");
        let dest_path = temp.path().join("dest.txt");
        fs::write(&source_path, b"matching prefix plus more data").expect("write source");
        fs::write(&dest_path, b"matching prefix").expect("write dest");
        let mut reader = fs::File::open(&source_path).expect("open source");
        let dest_meta = fs::metadata(&dest_path).expect("dest metadata");

        let result = determine_append_mode(
            true,
            true, // verify enabled
            &mut reader,
            &source_path,
            &dest_path,
            Some(&dest_meta),
            30, // full source size
        )
        .expect("determine");

        match result {
            AppendMode::Append {
                offset,
                verify_failed,
            } => {
                assert_eq!(offset, 15);
                assert!(!verify_failed);
            }
            AppendMode::Disabled | AppendMode::Skip => panic!("expected Append mode"),
        }
    }

    #[test]
    fn determine_append_mode_still_appends_when_verify_will_fail() {
        let temp = tempdir().expect("tempdir");
        let source_path = temp.path().join("source.txt");
        let dest_path = temp.path().join("dest.txt");
        fs::write(&source_path, b"source content plus more data").expect("write source");
        fs::write(&dest_path, b"different prefix").expect("write dest");
        let mut reader = fs::File::open(&source_path).expect("open source");
        let dest_meta = fs::metadata(&dest_path).expect("dest metadata");

        let result = determine_append_mode(
            true,
            true, // verify enabled
            &mut reader,
            &source_path,
            &dest_path,
            Some(&dest_meta),
            29, // full source size
        )
        .expect("determine");

        // Degrading to a plain whole-file copy here is exactly the bug this
        // encodes against: upstream appends the tail regardless, keeps the
        // result (--append implies --inplace, options.c:2400-2411), and only
        // then reports the failed whole-file re-checksum so the generator can
        // redo the file against the retained partial (generator.c:2175-2217).
        // Cancelling the append would leave nothing to re-delta and would make
        // the transfer look like a clean single-pass copy.
        match result {
            AppendMode::Append {
                offset,
                verify_failed,
            } => {
                assert_eq!(offset, 16);
                assert!(verify_failed);
            }
            AppendMode::Disabled | AppendMode::Skip => {
                panic!("a failed verification must still append, then redo")
            }
        }
    }

    #[test]
    fn verify_append_prefix_returns_true_when_matches() {
        let temp = tempdir().expect("tempdir");
        let source_path = temp.path().join("source.txt");
        let dest_path = temp.path().join("dest.txt");
        fs::write(&source_path, b"matching prefix and additional content").expect("write source");
        fs::write(&dest_path, b"matching prefix").expect("write dest");
        let mut reader = fs::File::open(&source_path).expect("open source");

        let result =
            verify_append_prefix(&mut reader, &source_path, &dest_path, 15).expect("verify");
        assert!(result);
    }

    #[test]
    fn verify_append_prefix_returns_false_when_mismatch() {
        let temp = tempdir().expect("tempdir");
        let source_path = temp.path().join("source.txt");
        let dest_path = temp.path().join("dest.txt");
        fs::write(&source_path, b"source content and more").expect("write source");
        fs::write(&dest_path, b"different prefix").expect("write dest");
        let mut reader = fs::File::open(&source_path).expect("open source");

        let result =
            verify_append_prefix(&mut reader, &source_path, &dest_path, 16).expect("verify");
        assert!(!result);
    }

    #[test]
    fn verify_append_prefix_handles_partial_reads() {
        let temp = tempdir().expect("tempdir");
        let source_path = temp.path().join("source.txt");
        let dest_path = temp.path().join("dest.txt");

        // Create a file larger than COPY_BUFFER_SIZE to test chunked reading
        let content = "A".repeat(10000);
        fs::write(&source_path, &content).expect("write source");
        fs::write(&dest_path, &content[..5000]).expect("write dest");
        let mut reader = fs::File::open(&source_path).expect("open source");

        let result =
            verify_append_prefix(&mut reader, &source_path, &dest_path, 5000).expect("verify");
        assert!(result);
    }
}
