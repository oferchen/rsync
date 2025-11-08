use crate::error::TaskResult;
use std::fs;
use std::io::{self, BufRead, Read};
use std::path::Path;

/// Heuristically determines whether the path references a binary file.
pub fn is_probably_binary(path: &Path) -> TaskResult<bool> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Ok(false);
    }

    let mut file = fs::File::open(path)?;
    let mut buffer = [0u8; 8192];
    let mut printable = 0usize;
    let mut control = 0usize;
    let mut inspected = 0usize;

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }

        inspected += read;

        for &byte in &buffer[..read] {
            match byte {
                0 => return Ok(true),
                0x07 | 0x08 | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C => printable += 1,
                0x20..=0x7E => printable += 1,
                _ if byte >= 0x80 => printable += 1,
                _ => control += 1,
            }
        }

        if control > printable {
            return Ok(true);
        }

        if inspected >= buffer.len() {
            break;
        }
    }

    Ok(false)
}

/// Counts the number of lines in the provided UTF-8 text file.
pub fn count_file_lines(path: &Path) -> TaskResult<usize> {
    let file = fs::File::open(path)?;
    let mut reader = io::BufReader::new(file);
    let mut buffer = String::new();
    let mut count = 0usize;

    loop {
        buffer.clear();
        let read = reader.read_line(&mut buffer)?;
        if read == 0 {
            break;
        }
        count += 1;
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::{count_file_lines, is_probably_binary};
    use std::fs;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn binary_detection_flags_control_bytes() {
        let dir = tempdir().expect("create temp dir");
        let text_path = dir.path().join("text.rs");
        fs::write(&text_path, b"fn main() {}\n").expect("write text file");
        assert!(!is_probably_binary(&text_path).expect("check succeeds"));

        let binary_path = dir.path().join("binary.bin");
        let mut file = fs::File::create(&binary_path).expect("create binary file");
        file.write_all(b"\x00\x01\x02not ascii")
            .expect("write binary");
        drop(file);
        assert!(is_probably_binary(&binary_path).expect("check succeeds"));
    }

    #[test]
    fn count_file_lines_handles_various_lengths() {
        let dir = tempdir().expect("create temp dir");
        let file_path = dir.path().join("source.rs");
        fs::write(&file_path, "line one\nline two\nline three").expect("write file");
        assert_eq!(count_file_lines(&file_path).expect("count succeeds"), 3);
    }
}
