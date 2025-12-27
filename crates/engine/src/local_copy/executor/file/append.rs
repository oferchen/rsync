use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::local_copy::LocalCopyError;

use super::super::super::COPY_BUFFER_SIZE;

pub(crate) enum AppendMode {
    Disabled,
    Append(u64),
}

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
    if existing_len == 0 || existing_len >= file_size {
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
        return Ok(AppendMode::Disabled);
    }

    if append_verify {
        let matches = verify_append_prefix(reader, source, destination, existing_len)?;
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
        if !matches {
            return Ok(AppendMode::Disabled);
        }
    } else {
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
    }

    Ok(AppendMode::Append(existing_len))
}

fn verify_append_prefix(
    reader: &mut fs::File,
    source: &Path,
    destination: &Path,
    existing_len: u64,
) -> Result<bool, LocalCopyError> {
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
    let mut destination_file = fs::File::open(destination).map_err(|error| {
        LocalCopyError::io(
            "read existing destination",
            destination.to_path_buf(),
            error,
        )
    })?;
    let mut remaining = existing_len;
    let mut source_buffer = vec![0u8; COPY_BUFFER_SIZE];
    let mut destination_buffer = vec![0u8; COPY_BUFFER_SIZE];

    while remaining > 0 {
        let chunk = remaining.min(COPY_BUFFER_SIZE as u64) as usize;
        let source_read = reader
            .read(&mut source_buffer[..chunk])
            .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
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
    fn determine_append_mode_disabled_when_existing_larger() {
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

        assert!(matches!(result, AppendMode::Disabled));
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
            AppendMode::Append(offset) => assert_eq!(offset, 6), // "source" is 6 bytes
            AppendMode::Disabled => panic!("expected Append mode"),
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
            AppendMode::Append(offset) => assert_eq!(offset, 15),
            AppendMode::Disabled => panic!("expected Append mode"),
        }
    }

    #[test]
    fn determine_append_mode_with_verify_disabled_when_prefix_mismatch() {
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

        assert!(matches!(result, AppendMode::Disabled));
    }

    #[test]
    fn verify_append_prefix_returns_true_when_matches() {
        let temp = tempdir().expect("tempdir");
        let source_path = temp.path().join("source.txt");
        let dest_path = temp.path().join("dest.txt");
        fs::write(&source_path, b"matching prefix and additional content")
            .expect("write source");
        fs::write(&dest_path, b"matching prefix").expect("write dest");
        let mut reader = fs::File::open(&source_path).expect("open source");

        let result = verify_append_prefix(&mut reader, &source_path, &dest_path, 15)
            .expect("verify");
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

        let result = verify_append_prefix(&mut reader, &source_path, &dest_path, 16)
            .expect("verify");
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

        let result = verify_append_prefix(&mut reader, &source_path, &dest_path, 5000)
            .expect("verify");
        assert!(result);
    }
}
